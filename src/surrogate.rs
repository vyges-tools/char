//! A baseline **CPU-side surrogate** over a characterization grid: a small bivariate
//! polynomial least-squares model (std-only, no external crate, no CUDA) that learns
//! delay/transition/… as a smooth function of `(input_slew, output_load)`.
//!
//! The point is **sample-efficiency**: fit on a *subset* of the SPICE-measured grid,
//! predict the rest, and report the held-out error. If the error is small, char can
//! simulate fewer points and fill the grid with the surrogate — the fast inner loop
//! without faster SPICE (and without a GPU). This is an honest baseline, not a claim
//! that a quadratic beats SPICE; the reported error is the whole point.
//!
//! Numerics: inputs are normalized to `[0,1]` over the training range before fitting,
//! so the (otherwise Vandermonde-ill-conditioned) normal equations stay well-behaved.
//! Pure std — unit-tested offline.

/// A bivariate polynomial `sum_{i,j} c_ij * sn^i * ln^j` over normalized inputs
/// `sn,ln in [0,1]`, with the normalization baked in so `predict` takes raw `(s,l)`.
#[derive(Debug, Clone)]
pub struct Poly2 {
    pub deg_s: usize,
    pub deg_l: usize,
    s_min: f64,
    s_span: f64,
    l_min: f64,
    l_span: f64,
    coeffs: Vec<f64>, // term (i,j) at index i*(deg_l+1)+j
}

impl Poly2 {
    /// Number of basis terms (coefficients) for the given per-axis degrees.
    pub fn n_terms(deg_s: usize, deg_l: usize) -> usize {
        (deg_s + 1) * (deg_l + 1)
    }

    /// Least-squares fit to `(s, l, y)` points. Returns `None` if under-determined
    /// (fewer points than terms) or the normal matrix is singular.
    pub fn fit(points: &[(f64, f64, f64)], deg_s: usize, deg_l: usize) -> Option<Poly2> {
        let nt = Poly2::n_terms(deg_s, deg_l);
        if points.len() < nt {
            return None;
        }
        let (s_min, s_max) = min_max(points.iter().map(|p| p.0));
        let (l_min, l_max) = min_max(points.iter().map(|p| p.1));
        let s_span = if (s_max - s_min).abs() < f64::EPSILON { 1.0 } else { s_max - s_min };
        let l_span = if (l_max - l_min).abs() < f64::EPSILON { 1.0 } else { l_max - l_min };

        // Normal equations A^T A c = A^T y, accumulated point-by-point.
        let mut ata = vec![vec![0.0f64; nt]; nt];
        let mut aty = vec![0.0f64; nt];
        for &(s, l, y) in points {
            let phi = basis((s - s_min) / s_span, (l - l_min) / l_span, deg_s, deg_l);
            for r in 0..nt {
                aty[r] += phi[r] * y;
                for c in 0..nt {
                    ata[r][c] += phi[r] * phi[c];
                }
            }
        }
        let coeffs = solve(ata, aty)?;
        Some(Poly2 { deg_s, deg_l, s_min, s_span, l_min, l_span, coeffs })
    }

    /// Predict the value at raw `(s, l)`.
    pub fn predict(&self, s: f64, l: f64) -> f64 {
        let phi = basis((s - self.s_min) / self.s_span, (l - self.l_min) / self.l_span, self.deg_s, self.deg_l);
        phi.iter().zip(&self.coeffs).map(|(p, c)| p * c).sum()
    }
}

/// A fitted surrogate over `(slew, load)`: a `Poly2`, optionally in log-log space
/// (`ln(value)` over `ln(slew), ln(load)`). `predict` always takes/returns linear units.
#[derive(Debug, Clone)]
pub struct Model {
    poly: Poly2,
    log: bool,
    /// Number of points actually used to fit (log drops non-positive samples).
    pub n_fit: usize,
}

impl Model {
    /// Fit on `(s, l, value)` points. In log mode, non-positive samples are dropped
    /// (log undefined); the per-axis degree auto-reduces if too few points remain.
    /// Returns `None` if it can't be determined / solved.
    pub fn fit(points: &[(f64, f64, f64)], degree: usize, log: bool) -> Option<Model> {
        let pts: Vec<(f64, f64, f64)> = if log {
            points
                .iter()
                .filter(|&&(s, l, v)| s > 0.0 && l > 0.0 && v > 0.0)
                .map(|&(s, l, v)| (s.ln(), l.ln(), v.ln()))
                .collect()
        } else {
            points.to_vec()
        };
        let mut d = degree.max(1);
        while Poly2::n_terms(d, d) > pts.len() && d > 1 {
            d -= 1;
        }
        let poly = Poly2::fit(&pts, d, d)?;
        Some(Model { poly, log, n_fit: pts.len() })
    }

    /// Predict the value at raw `(s, l)` (exponentiates back from log space if needed).
    pub fn predict(&self, s: f64, l: f64) -> f64 {
        if self.log {
            self.poly.predict(s.ln(), l.ln()).exp()
        } else {
            self.poly.predict(s, l)
        }
    }

    pub fn degree(&self) -> usize {
        self.poly.deg_s
    }
}

/// Held-out accuracy of the surrogate on one grid. Relative errors are normalized by
/// `scale` (the peak `|value|` over the grid), not per-point — so a near-zero corner
/// point can't blow the percentage up. `max_abs`/`rms` are the raw linear-unit errors.
#[derive(Debug, Clone, PartialEq)]
pub struct Eval {
    pub deg_s: usize,
    pub deg_l: usize,
    pub n_train: usize,
    pub n_test: usize,
    pub max_abs: f64,
    pub rms: f64,
    pub mean_abs: f64,
    pub scale: f64,        // peak |value| over the finite grid (the % denominator)
    pub max_rel_pct: f64,  // max_abs / scale * 100  (worst error as % of peak)
    pub rms_rel_pct: f64,  // rms / scale * 100
}

/// Fit the surrogate on a checkerboard subset of a `slews x loads` grid (`(i+j)` even),
/// predict the held-out points (`(i+j)` odd), and report the error — in **linear**
/// coordinates. Errors are always measured in the original linear units.
pub fn holdout_eval(slews: &[f64], loads: &[f64], values: &[Vec<f64>], deg: usize) -> Option<Eval> {
    holdout_impl(slews, loads, values, deg, false)
}

/// As [`holdout_eval`], but fit in **log-log space** — `ln(value)` as a polynomial in
/// `ln(slew), ln(load)`. NLDM grids are log-spaced and their surfaces are far closer to
/// low-order-polynomial in log space, so this typically fits far better than linear.
/// Falls back to a linear fit if any involved value is non-positive (log undefined).
pub fn holdout_eval_log(slews: &[f64], loads: &[f64], values: &[Vec<f64>], deg: usize) -> Option<Eval> {
    holdout_impl(slews, loads, values, deg, true)
}

/// Checkerboard fit/predict. `log` selects log-log space (inputs and target). The
/// requested degree auto-reduces (down to 1) if the training set is too small.
/// Returns `None` if there is no held-out point or too few train points even at degree 1.
fn holdout_impl(slews: &[f64], loads: &[f64], values: &[Vec<f64>], deg: usize, log: bool) -> Option<Eval> {
    let mut train: Vec<(f64, f64, f64)> = Vec::new(); // (s, l, value) in linear units
    let mut test: Vec<(f64, f64, f64)> = Vec::new();
    let mut scale = 0.0f64; // peak |value| over the finite grid (relative-error denominator)
    for (i, &s) in slews.iter().enumerate() {
        if i >= values.len() {
            break;
        }
        for (j, &l) in loads.iter().enumerate() {
            if j >= values[i].len() {
                break;
            }
            let v = values[i][j];
            if !v.is_finite() {
                continue; // skip uncharacterizable points
            }
            scale = scale.max(v.abs());
            if (i + j) % 2 == 0 {
                train.push((s, l, v));
            } else {
                test.push((s, l, v));
            }
        }
    }
    if test.is_empty() {
        return None;
    }
    // Log mode drops stray non-positive samples (a single negative-delay artifact must
    // not disable log for the whole table); falls back to linear if too few remain.
    let log_ok = log
        && train.iter().filter(|&&(s, l, v)| s > 0.0 && l > 0.0 && v > 0.0).count()
            >= Poly2::n_terms(1, 1);
    let model = Model::fit(&train, deg, log_ok)?;

    let (mut max_abs, mut sumsq, mut sumabs) = (0.0f64, 0.0f64, 0.0f64);
    for &(s, l, actual) in &test {
        let err = (model.predict(s, l) - actual).abs(); // always measured in linear units
        max_abs = max_abs.max(err);
        sumsq += err * err;
        sumabs += err;
    }
    let n = test.len() as f64;
    let rms = (sumsq / n).sqrt();
    let den = if scale > 1e-12 { scale } else { 1.0 };
    Some(Eval {
        deg_s: model.degree(),
        deg_l: model.degree(),
        n_train: model.n_fit, // points actually used to fit (log drops non-positive)
        n_test: test.len(),
        max_abs,
        rms,
        mean_abs: sumabs / n,
        scale,
        max_rel_pct: max_abs / den * 100.0,
        rms_rel_pct: rms / den * 100.0,
    })
}

/// Basis vector `[sn^i * ln^j]` ordered by `i*(deg_l+1)+j`.
fn basis(sn: f64, ln: f64, deg_s: usize, deg_l: usize) -> Vec<f64> {
    let mut v = Vec::with_capacity((deg_s + 1) * (deg_l + 1));
    let mut sp = 1.0;
    for _ in 0..=deg_s {
        let mut lp = 1.0;
        for _ in 0..=deg_l {
            v.push(sp * lp);
            lp *= ln;
        }
        sp *= sn;
    }
    v
}

fn min_max(it: impl Iterator<Item = f64>) -> (f64, f64) {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for x in it {
        lo = lo.min(x);
        hi = hi.max(x);
    }
    (lo, hi)
}

/// Solve `A x = b` (square) by Gaussian elimination with partial pivoting.
/// Returns `None` if the matrix is singular (near-zero pivot).
#[allow(clippy::needless_range_loop)] // row-pair indexing is clearest here
fn solve(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
    let n = b.len();
    for col in 0..n {
        // partial pivot
        let mut piv = col;
        for r in (col + 1)..n {
            if a[r][col].abs() > a[piv][col].abs() {
                piv = r;
            }
        }
        if a[piv][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, piv);
        b.swap(col, piv);
        // eliminate below
        for r in (col + 1)..n {
            let f = a[r][col] / a[col][col];
            if f != 0.0 {
                for c in col..n {
                    a[r][c] -= f * a[col][c];
                }
                b[r] -= f * b[col];
            }
        }
    }
    // back-substitution
    let mut x = vec![0.0f64; n];
    for i in (0..n).rev() {
        let mut s = b[i];
        for c in (i + 1)..n {
            s -= a[i][c] * x[c];
        }
        x[i] = s / a[i][i];
    }
    Some(x)
}
