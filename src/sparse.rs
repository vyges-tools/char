//! Sparse-sweep + surrogate fill: simulate a **coarse** `(slew, load)` grid in SPICE,
//! fit a log-space [`Model`] per NLDM metric, and **predict the full dense grid** — a
//! complete `.lib` at a fraction of the simulation count. Pure (no simulator) so the
//! fit/fill/predict path is unit-tested offline; the engine supplies the measured points.

use crate::liberty::Table;
use crate::surrogate::Model;

/// One simulated grid point: the four NLDM values at a `(slew, load)`.
#[derive(Debug, Clone, Copy)]
pub struct ArcPoint {
    pub slew: f64,
    pub load: f64,
    pub cell_rise: f64,
    pub cell_fall: f64,
    pub rise_transition: f64,
    pub fall_transition: f64,
}

/// One fitted surrogate per NLDM metric for a single arc.
pub struct ArcModels {
    cell_rise: Model,
    cell_fall: Model,
    rise_transition: Model,
    fall_transition: Model,
}

impl ArcModels {
    /// Fit (log-space) surrogates for all four metrics from the simulated points.
    /// Returns `None` if any metric can't be fit (too few usable points).
    pub fn fit(sim: &[ArcPoint], degree: usize) -> Option<ArcModels> {
        let col = |f: fn(&ArcPoint) -> f64| -> Vec<(f64, f64, f64)> {
            sim.iter().map(|p| (p.slew, p.load, f(p))).collect()
        };
        Some(ArcModels {
            cell_rise: Model::fit(&col(|p| p.cell_rise), degree, true)?,
            cell_fall: Model::fit(&col(|p| p.cell_fall), degree, true)?,
            rise_transition: Model::fit(&col(|p| p.rise_transition), degree, true)?,
            fall_transition: Model::fit(&col(|p| p.fall_transition), degree, true)?,
        })
    }

    /// Predict `(cell_rise, cell_fall, rise_transition, fall_transition)` at `(s, l)`.
    pub fn predict(&self, s: f64, l: f64) -> (f64, f64, f64, f64) {
        (
            self.cell_rise.predict(s, l),
            self.cell_fall.predict(s, l),
            self.rise_transition.predict(s, l),
            self.fall_transition.predict(s, l),
        )
    }

    /// Predict the full dense grid → the four NLDM tables `[slew][load]`.
    pub fn fill(&self, slews: &[f64], loads: &[f64]) -> (Table, Table, Table, Table) {
        let (ns, nl) = (slews.len(), loads.len());
        let (mut cr, mut cf, mut rt, mut ft) =
            (Table::new(ns, nl), Table::new(ns, nl), Table::new(ns, nl), Table::new(ns, nl));
        for (i, &s) in slews.iter().enumerate() {
            for (j, &l) in loads.iter().enumerate() {
                let (a, b, c, d) = self.predict(s, l);
                cr.values[i][j] = a;
                cf.values[i][j] = b;
                rt.values[i][j] = c;
                ft.values[i][j] = d;
            }
        }
        (cr, cf, rt, ft)
    }
}

/// Leave-one-out cross-validation error (worst metric, as % of that metric's peak) of a
/// degree-`degree` log surrogate on the simulated points — a no-extra-simulation estimate
/// of how well the current sample predicts unseen points. Drives the active-sampling stop.
/// Returns `f64::INFINITY` if it can't be computed (too few points for any fold).
pub fn loo_cv_rms_pct(sim: &[ArcPoint], degree: usize) -> f64 {
    let metrics: [fn(&ArcPoint) -> f64; 4] =
        [|p| p.cell_rise, |p| p.cell_fall, |p| p.rise_transition, |p| p.fall_transition];
    metrics
        .iter()
        .map(|f| loo_one(sim, *f, degree))
        .fold(0.0f64, f64::max)
}

/// LOO RMS (% of peak) for a single metric column.
fn loo_one(sim: &[ArcPoint], f: fn(&ArcPoint) -> f64, degree: usize) -> f64 {
    let pts: Vec<(f64, f64, f64)> = sim.iter().map(|p| (p.slew, p.load, f(p))).collect();
    let scale = pts.iter().map(|&(_, _, v)| v.abs()).fold(0.0f64, f64::max);
    let (mut sumsq, mut n) = (0.0f64, 0usize);
    for k in 0..pts.len() {
        let rest: Vec<(f64, f64, f64)> =
            pts.iter().enumerate().filter(|&(i, _)| i != k).map(|(_, &p)| p).collect();
        if let Some(m) = Model::fit(&rest, degree, true) {
            let e = (m.predict(pts[k].0, pts[k].1) - pts[k].2).abs();
            sumsq += e * e;
            n += 1;
        }
    }
    if n == 0 {
        return f64::INFINITY;
    }
    let den = if scale > 1e-12 { scale } else { 1.0 };
    (sumsq / n as f64).sqrt() / den * 100.0
}

/// Pick the next point to simulate: the `candidate` whose nearest already-`sampled`
/// point is **farthest** (maximin) in log space — fill the biggest gap. `None` if there
/// are no candidates. This is the model-free space-filling heart of active sampling.
pub fn maximin_next(sampled: &[(f64, f64)], candidates: &[(f64, f64)]) -> Option<(f64, f64)> {
    let logd = |a: (f64, f64), b: (f64, f64)| {
        let ds = a.0.max(1e-300).ln() - b.0.max(1e-300).ln();
        let dl = a.1.max(1e-300).ln() - b.1.max(1e-300).ln();
        ds * ds + dl * dl
    };
    candidates
        .iter()
        .map(|&c| {
            let nearest = sampled.iter().map(|&s| logd(c, s)).fold(f64::INFINITY, f64::min);
            (c, nearest)
        })
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(c, _)| c)
}

/// Fit one surrogate on `(x, y, value)` points and predict the `xs × ys` grid → a Table.
/// `log` selects log-log space (use `false` for metrics that can be negative, e.g. a
/// flop's hold or recovery). Returns `None` if the fit can't be determined.
pub fn fill_one(points: &[(f64, f64, f64)], xs: &[f64], ys: &[f64], degree: usize, log: bool) -> Option<Table> {
    let finite: Vec<(f64, f64, f64)> = points.iter().copied().filter(|&(_, _, v)| v.is_finite()).collect();
    let m = Model::fit(&finite, degree, log)?;
    let mut t = Table::new(xs.len(), ys.len());
    for (i, &x) in xs.iter().enumerate() {
        for (j, &y) in ys.iter().enumerate() {
            t.values[i][j] = m.predict(x, y);
        }
    }
    Some(t)
}

/// `n` points geometrically (log-)spaced over `[min, max]` inclusive — the natural
/// spacing for a characterization axis. Falls back to linear if `min <= 0`.
pub fn geometric(min: f64, max: f64, n: usize) -> Vec<f64> {
    if n <= 1 || (max - min).abs() < f64::EPSILON {
        return vec![min];
    }
    if min <= 0.0 {
        let step = (max - min) / (n - 1) as f64;
        return (0..n).map(|k| min + step * k as f64).collect();
    }
    let ratio = (max / min).powf(1.0 / (n - 1) as f64);
    (0..n).map(|k| min * ratio.powi(k as i32)).collect()
}
