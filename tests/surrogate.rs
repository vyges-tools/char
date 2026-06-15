//! The surrogate fitter/evaluator is pure math — exercised offline on synthetic grids.

use vyges_char::surrogate::{holdout_eval, holdout_eval_log, Poly2};

/// Build a `slews x loads` grid by sampling `f`.
fn grid(slews: &[f64], loads: &[f64], f: impl Fn(f64, f64) -> f64) -> Vec<Vec<f64>> {
    slews.iter().map(|&s| loads.iter().map(|&l| f(s, l)).collect()).collect()
}

const SLEWS4: [f64; 4] = [0.01, 0.04, 0.16, 0.64];
const LOADS4: [f64; 4] = [0.0005, 0.002, 0.008, 0.032];

/// A bilinear surface is exactly representable by a degree-(1,1) polynomial.
fn bilinear(s: f64, l: f64) -> f64 {
    2.0 + 3.0 * s + 4.0 * l + 5.0 * s * l
}

#[test]
fn fit_recovers_exact_bilinear() {
    let mut pts = Vec::new();
    for &s in &SLEWS4 {
        for &l in &LOADS4 {
            pts.push((s, l, bilinear(s, l)));
        }
    }
    let m = Poly2::fit(&pts, 1, 1).expect("fit");
    // predict off-grid: must match the closed form to numerical precision.
    for (s, l) in [(0.1, 0.01), (0.5, 0.02), (0.001, 0.0001)] {
        assert!((m.predict(s, l) - bilinear(s, l)).abs() < 1e-9, "({s},{l})");
    }
}

#[test]
fn holdout_bilinear_is_essentially_exact() {
    let vals = grid(&SLEWS4, &LOADS4, bilinear);
    let e = holdout_eval(&SLEWS4, &LOADS4, &vals, 1).expect("eval");
    assert_eq!((e.n_train, e.n_test), (8, 8)); // 4x4 checkerboard
    assert!(e.max_abs < 1e-9, "max_abs={}", e.max_abs);
    assert!(e.max_rel_pct < 1e-6, "max_rel_pct={}", e.max_rel_pct);
}

#[test]
fn holdout_on_curved_surface_is_small() {
    // a smooth, slightly non-polynomial delay-like surface (sqrt term).
    let slews = [0.01, 0.02, 0.04, 0.08, 0.16, 0.32, 0.64];
    let loads = [0.0005, 0.001, 0.002, 0.004, 0.008, 0.016, 0.032];
    let f = |s: f64, l: f64| 0.03 + 0.25 * s + 0.9 * l + 3.5 * s * l + 0.4 * l.sqrt();
    let vals = grid(&slews, &loads, f);
    let e = holdout_eval(&slews, &loads, &vals, 2).expect("eval");
    assert_eq!(e.deg_s, 2);
    assert!(e.n_train > e.deg_s * e.deg_s, "enough train points");
    // a degree-2 fit should track this surface to within a few percent of peak.
    assert!(e.max_rel_pct < 10.0, "max_rel_pct={}", e.max_rel_pct);
    // relative errors are normalized by the grid peak (definitional), and rms <= max.
    let peak = vals.iter().flatten().cloned().fold(0.0f64, |m, v| m.max(v.abs()));
    assert!((e.scale - peak).abs() < 1e-12, "scale should be the grid peak");
    assert!((e.max_rel_pct - e.max_abs / e.scale * 100.0).abs() < 1e-9);
    assert!(e.rms_rel_pct <= e.max_rel_pct + 1e-9);
}

#[test]
fn log_space_fits_power_law_essentially_exactly() {
    // A power law y = a * s^p * l^q is exactly linear in log-log space, so a degree-1
    // log fit recovers it — where a linear-space poly would struggle across decades.
    let power = |s: f64, l: f64| 0.5 * s.powf(0.8) * l.powf(0.4);
    let vals = grid(&SLEWS4, &LOADS4, power);
    let e = holdout_eval_log(&SLEWS4, &LOADS4, &vals, 1).expect("log eval");
    assert!(e.max_rel_pct < 1e-6, "log power-law max_rel_pct={}", e.max_rel_pct);
    // the same surface in linear space at degree 1 is far worse (sanity: log helps).
    let lin = holdout_eval(&SLEWS4, &LOADS4, &vals, 1).expect("lin eval");
    assert!(lin.max_rel_pct > e.max_rel_pct, "log should beat linear here");
}

#[test]
fn underdetermined_fit_returns_none() {
    // 2 points cannot determine a degree-(2,2) model (9 terms).
    let pts = [(0.0, 0.0, 1.0), (1.0, 1.0, 2.0)];
    assert!(Poly2::fit(&pts, 2, 2).is_none());
}

#[test]
fn degree_auto_reduces_for_small_grid() {
    // 3x3 -> checkerboard train = 5 points; requested degree 2 (9 terms) can't fit,
    // so it reduces to degree 1 (4 terms) — which is exact for a bilinear surface.
    let slews = [0.01, 0.04, 0.16];
    let loads = [0.001, 0.004, 0.016];
    let vals = grid(&slews, &loads, bilinear);
    let e = holdout_eval(&slews, &loads, &vals, 2).expect("eval");
    assert_eq!(e.deg_s, 1, "degree reduced to fit the small training set");
    assert!(e.max_abs < 1e-9);
}

#[test]
fn no_holdout_point_returns_none() {
    // a 1x1 grid has a single (even) train point and no held-out point.
    assert!(holdout_eval(&[0.02], &[0.001], &[vec![0.5]], 2).is_none());
}

#[test]
fn non_finite_points_are_skipped() {
    // a NaN in the grid must not poison the fit; the rest still evaluate.
    let mut vals = grid(&SLEWS4, &LOADS4, bilinear);
    vals[0][1] = f64::NAN; // (0+1) is odd -> a held-out test point; must be dropped
    let e = holdout_eval(&SLEWS4, &LOADS4, &vals, 1).expect("eval");
    assert_eq!((e.n_train, e.n_test), (8, 7)); // one test point dropped, train intact
    assert!(e.max_abs < 1e-9);
}
