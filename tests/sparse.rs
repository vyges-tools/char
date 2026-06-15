//! Sparse fill is pure math (fit log surrogates on simulated points, predict the grid) —
//! exercised offline on synthetic power-law surfaces (exact in log space).

use vyges_char::sparse::{geometric, loo_cv_rms_pct, maximin_next, ArcModels, ArcPoint};

#[test]
fn geometric_spacing() {
    let g = geometric(0.01, 1.0, 3);
    assert_eq!(g.len(), 3);
    assert!((g[0] - 0.01).abs() < 1e-12);
    assert!((g[1] - 0.1).abs() < 1e-9); // geometric midpoint of 0.01..1.0
    assert!((g[2] - 1.0).abs() < 1e-9);
    assert_eq!(geometric(0.5, 0.5, 4), vec![0.5]); // degenerate range
    assert_eq!(geometric(0.2, 9.0, 1), vec![0.2]); // n=1
}

// Power-law surfaces are exactly linear in log-log space, so a log-fit surrogate
// recovers them — the cleanest check that fit→fill→predict is wired correctly.
fn surf(s: f64, l: f64) -> (f64, f64, f64, f64) {
    (
        0.10 * s.powf(0.5) * l.powf(0.3),
        0.20 * s.powf(0.4) * l.powf(0.2),
        0.05 * s.powf(0.6) * l.powf(0.1),
        0.04 * s.powf(0.7) * l.powf(0.15),
    )
}

fn sim_grid() -> Vec<ArcPoint> {
    let slews = [0.01, 0.04, 0.16, 0.64];
    let loads = [0.001, 0.004, 0.016, 0.064];
    let mut v = Vec::new();
    for &s in &slews {
        for &l in &loads {
            let (cr, cf, rt, ft) = surf(s, l);
            v.push(ArcPoint {
                slew: s,
                load: l,
                cell_rise: cr,
                cell_fall: cf,
                rise_transition: rt,
                fall_transition: ft,
            });
        }
    }
    v
}

#[test]
fn fit_then_predict_recovers_power_law_offgrid() {
    let m = ArcModels::fit(&sim_grid(), 2).expect("fit");
    // predict at points NOT in the sim grid; log fit of a power law is ~exact.
    for (s, l) in [(0.02, 0.002), (0.3, 0.05), (0.5, 0.03)] {
        let (cr, cf, rt, ft) = m.predict(s, l);
        let (tr_cr, tr_cf, tr_rt, tr_ft) = surf(s, l);
        let close = |a: f64, b: f64| (a - b).abs() / b < 1e-6;
        assert!(close(cr, tr_cr), "cell_rise {cr} vs {tr_cr}");
        assert!(close(cf, tr_cf), "cell_fall {cf} vs {tr_cf}");
        assert!(close(rt, tr_rt), "rise_transition {rt} vs {tr_rt}");
        assert!(close(ft, tr_ft), "fall_transition {ft} vs {tr_ft}");
    }
}

#[test]
fn fill_matches_predict_on_the_dense_grid() {
    let m = ArcModels::fit(&sim_grid(), 2).expect("fit");
    let slews = geometric(0.01, 0.64, 7);
    let loads = geometric(0.001, 0.064, 7);
    let (cr, _, _, ft) = m.fill(&slews, &loads);
    assert_eq!(cr.values.len(), 7);
    assert_eq!(cr.values[0].len(), 7);
    // a filled cell equals predicting that cell directly, and tracks the truth.
    for (i, &s) in slews.iter().enumerate() {
        for (j, &l) in loads.iter().enumerate() {
            let (p_cr, _, _, _) = m.predict(s, l);
            assert!((cr.values[i][j] - p_cr).abs() < 1e-12);
            let (t_cr, _, _, t_ft) = surf(s, l);
            assert!((cr.values[i][j] - t_cr).abs() / t_cr < 1e-6);
            assert!((ft.values[i][j] - t_ft).abs() / t_ft < 1e-6);
        }
    }
}

#[test]
fn loo_cv_near_zero_on_power_law() {
    // a power law is exactly fit in log space, so leave-one-out error is ~0.
    let cv = loo_cv_rms_pct(&sim_grid(), 2);
    assert!(cv.is_finite() && cv < 1e-6, "loo cv = {cv}");
}

#[test]
fn maximin_picks_the_biggest_gap() {
    // sampled at two far-apart corners; the candidate in the middle (log space) wins
    // over one hugging an existing sample.
    let sampled = [(0.01, 0.001), (0.64, 0.064)];
    let near = (0.012, 0.0011); // right next to the first sample
    let gap = (0.08, 0.008); // roughly the log-midpoint
    let pick = maximin_next(&sampled, &[near, gap]).unwrap();
    assert_eq!(pick, gap);
    assert!(maximin_next(&sampled, &[]).is_none());
}

#[test]
fn too_few_points_fails_to_fit() {
    // 3 points can't determine a degree-2 model (9 terms) and there's no lower fallback
    // possible below degree 1's 4 terms.
    let pts: Vec<ArcPoint> = sim_grid().into_iter().take(3).collect();
    assert!(ArcModels::fit(&pts, 2).is_none());
}
