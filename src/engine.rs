//! Characterization engine: sweep slew x load, simulate, fill NLDM tables.
//!
//! Shells out to `ngspice` per measurement point (the file/CLI boundary). When
//! `ngspice` is absent (off the EDA host) `run` returns `NgspiceNotFound` so the
//! pure pieces (deck gen, parsing, Liberty emit) can still be exercised offline.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::job::CharJob;

/// Per-process counter for unique temp deck filenames.
static DECK_SEQ: AtomicUsize = AtomicUsize::new(0);
use crate::liberty::{self, Arc, Table, Units, Waveform};
use crate::spice;

#[derive(Debug)]
pub enum CharError {
    NgspiceNotFound,
    Sim(String),
    Io(String),
    Netlist(String),
}

impl std::fmt::Display for CharError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CharError::NgspiceNotFound => write!(
                f,
                "ngspice not found on PATH. Run inside the EDA environment \
                 (VyBox / build host) with ngspice + the PDK models installed."
            ),
            CharError::Sim(m) => write!(f, "simulation error: {m}"),
            CharError::Io(m) => write!(f, "io error: {m}"),
            CharError::Netlist(m) => write!(f, "netlist error: {m}"),
        }
    }
}
impl std::error::Error for CharError {}

fn ngspice_available() -> bool {
    Command::new("ngspice").arg("-v").output().is_ok()
}

/// The deck fragment wiring the cell under test for one timing arc, in the cell's
/// real port order.
///
/// Reads the `.subckt` port list from the netlist and maps each pin to a deck node:
/// the arc's in/out pins keep their net names (which the deck's source and load
/// drive); a **side input** (any other signal pin, declared on the `arc:` line) is
/// pinned to its non-controlling level by a fixed source and tied to its own net;
/// power pins tie to `VDD`, ground to `VSS`. A port that is none of these is a hard
/// error — for a multi-input cell, every non-arc input must be declared as a side
/// input (so its logic state is explicit), not silently floated.
///
/// Returns the side-input source lines followed by the `X1 …` instance line.
fn arc_wiring(job: &CharJob, spec: &crate::job::ArcSpec) -> Result<String, CharError> {
    let netlist = std::fs::read_to_string(&job.netlist)
        .map_err(|e| CharError::Netlist(format!("{}: {e}", job.netlist)))?;
    let pins = spice::parse_subckt_pins(&netlist, &job.cell).ok_or_else(|| {
        CharError::Netlist(format!("no `.subckt {}` found in {}", job.cell, job.netlist))
    })?;
    let mut nodes = Vec::with_capacity(pins.len());
    let mut sources = String::new();
    for pin in &pins {
        let node = if pin.eq_ignore_ascii_case(&spec.in_pin) {
            spec.in_pin.clone()
        } else if pin.eq_ignore_ascii_case(&spec.out_pin) {
            spec.out_pin.clone()
        } else if let Some((_, high)) =
            spec.side.iter().find(|(p, _)| p.eq_ignore_ascii_case(pin))
        {
            // hold this side input at its declared (non-controlling) logic level.
            let v = if *high { job.vdd } else { 0.0 };
            sources.push_str(&format!("V{pin} {pin} 0 {v}\n"));
            pin.clone()
        } else if job.power.iter().any(|p| p.eq_ignore_ascii_case(pin)) {
            "VDD".to_string()
        } else if job.ground.iter().any(|p| p.eq_ignore_ascii_case(pin)) {
            "VSS".to_string()
        } else {
            return Err(CharError::Netlist(format!(
                "subckt pin {pin:?} of {} is not the arc's in/out pin, a declared side \
                 input (add `{pin}=0|1` on the arc: line), or a power/ground pin",
                job.cell
            )));
        };
        nodes.push(node);
    }
    Ok(format!("{sources}X1 {} {}", nodes.join(" "), job.cell))
}

#[allow(clippy::too_many_arguments)]
fn run_point(
    job: &CharJob,
    subckt_call: &str,
    in_pin: &str,
    out_pin: &str,
    slew: f64,
    load: f64,
    rising_input: bool,
    mc: Option<u64>,
) -> Result<(f64, f64), CharError> {
    let includes: Vec<String> = std::iter::once(job.netlist.clone())
        .chain(job.models.iter().cloned())
        .collect();
    let d = spice::deck(
        &format!("char {} slew={slew} load={load}", job.cell),
        &includes,
        &job.osdi,
        subckt_call,
        in_pin,
        out_pin,
        job.vdd,
        slew,
        load,
        rising_input,
        mc,
    );
    // Write the deck to a temp file and pass its path: `ngspice -b -` (deck on
    // stdin) is not portable across ngspice builds — some reject `-` as a
    // filename. A temp file also leaves the deck on disk for debugging. The
    // process cwd (set by the caller) still governs the PDK's relative includes.
    let n = DECK_SEQ.fetch_add(1, Ordering::Relaxed);
    let deck_path =
        std::env::temp_dir().join(format!("vyges_char_{}_{}.sp", std::process::id(), n));
    std::fs::write(&deck_path, d.as_bytes()).map_err(|e| CharError::Io(e.to_string()))?;
    let out = Command::new("ngspice")
        .arg("-b")
        .arg(&deck_path)
        .arg("--no-spiceinit")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| CharError::Io(e.to_string()))?;
    let _ = std::fs::remove_file(&deck_path);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let m = spice::parse_measures(&text);
    let delay = *m.get("prop_delay").ok_or_else(|| CharError::Sim("no prop_delay".into()))?;
    let oslew = *m.get("out_slew").ok_or_else(|| CharError::Sim("no out_slew".into()))?;
    Ok((delay, oslew))
}

/// The result of a characterization run: combinational arcs, or a sequential cell.
pub enum Characterized {
    Comb(Vec<Arc>),
    Seq(Box<liberty::SeqCell>),
}

/// Characterize a cell. Combinational jobs yield one `Arc` per `arc:` spec (the
/// renderer groups them into a single cell); sequential jobs yield a `SeqCell`
/// with setup/hold constraints and the CK->Q arc.
pub fn characterize(job: &CharJob) -> Result<Characterized, CharError> {
    if !ngspice_available() {
        return Err(CharError::NgspiceNotFound);
    }
    if job.seq {
        Ok(Characterized::Seq(Box::new(characterize_seq(job)?)))
    } else {
        let arcs = job.arcs.iter().map(|spec| characterize_arc(job, spec)).collect::<Result<_, _>>()?;
        Ok(Characterized::Comb(arcs))
    }
}

/// Characterize a single timing arc (in_pin -> out_pin, side inputs held).
fn characterize_arc(job: &CharJob, spec: &crate::job::ArcSpec) -> Result<Arc, CharError> {
    let instance = arc_wiring(job, spec)?;
    let (in_pin, out_pin) = (spec.in_pin.as_str(), spec.out_pin.as_str());
    let (ns, nl) = (job.slews.len(), job.loads.len());
    let mut arc = Arc {
        cell: job.cell.clone(),
        in_pin: spec.in_pin.clone(),
        out_pin: spec.out_pin.clone(),
        sense: spec.sense.clone(),
        cell_rise: Table::new(ns, nl),
        cell_fall: Table::new(ns, nl),
        rise_transition: Table::new(ns, nl),
        fall_transition: Table::new(ns, nl),
        sigma_rise: Table::new(ns, nl),
        sigma_fall: Table::new(ns, nl),
        ccs_rise: Vec::new(),
        ccs_fall: Vec::new(),
        recv_c1_rise: Table::new(ns, nl),
        recv_c2_rise: Table::new(ns, nl),
        recv_c1_fall: Table::new(ns, nl),
        recv_c2_fall: Table::new(ns, nl),
    };
    for (i, &slew) in job.slews.iter().enumerate() {
        for (j, &load) in job.loads.iter().enumerate() {
            // nominal point: falling input -> rising output (cell_rise), vice versa.
            let (dr, tr) = run_point(job, &instance, in_pin, out_pin, slew, load, false, None)?;
            arc.cell_rise.values[i][j] = dr * 1e9;
            arc.rise_transition.values[i][j] = tr * 1e9;
            let (df, tf) = run_point(job, &instance, in_pin, out_pin, slew, load, true, None)?;
            arc.cell_fall.values[i][j] = df * 1e9;
            arc.fall_transition.values[i][j] = tf * 1e9;

            // LVF: Monte-Carlo over mismatch -> per-edge delay sigma (ns).
            if job.montecarlo > 0 {
                let mut rise = Vec::with_capacity(job.montecarlo);
                let mut fall = Vec::with_capacity(job.montecarlo);
                for k in 0..job.montecarlo as u64 {
                    rise.push(
                        run_point(job, &instance, in_pin, out_pin, slew, load, false, Some(k))?.0
                            * 1e9,
                    );
                    fall.push(
                        run_point(job, &instance, in_pin, out_pin, slew, load, true, Some(k))?.0
                            * 1e9,
                    );
                }
                arc.sigma_rise.values[i][j] = stddev(&rise);
                arc.sigma_fall.values[i][j] = stddev(&fall);
            }

            // CCS: capture the driver output-current waveform per edge.
            if job.ccs {
                arc.ccs_rise.push(run_ccs_point(job, &instance, in_pin, out_pin, slew, load, false)?);
                arc.ccs_fall.push(run_ccs_point(job, &instance, in_pin, out_pin, slew, load, true)?);
            }

            // CCS receiver capacitance: integrate the input-pin current into the
            // two segments (C1 before / C2 after the input 50% crossing).
            if job.recv {
                let (c1r, c2r) = run_recv_point(job, &instance, in_pin, out_pin, slew, load, true)?;
                arc.recv_c1_rise.values[i][j] = c1r;
                arc.recv_c2_rise.values[i][j] = c2r;
                let (c1f, c2f) = run_recv_point(job, &instance, in_pin, out_pin, slew, load, false)?;
                arc.recv_c1_fall.values[i][j] = c1f;
                arc.recv_c2_fall.values[i][j] = c2f;
            }
        }
    }
    Ok(arc)
}

/// Capture one CCS output-current waveform via `deck_ccs` (wrdata to a temp file),
/// sub-sampled to a compact set of points; current is stored as magnitude (the
/// timing carrier) and `ref_time` is the input 50% crossing.
#[allow(clippy::too_many_arguments)]
fn run_ccs_point(
    job: &CharJob,
    subckt_call: &str,
    in_pin: &str,
    out_pin: &str,
    slew: f64,
    load: f64,
    rising_input: bool,
) -> Result<Waveform, CharError> {
    let includes: Vec<String> =
        std::iter::once(job.netlist.clone()).chain(job.models.iter().cloned()).collect();
    let n = DECK_SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let dat = std::env::temp_dir().join(format!("vyges_ccs_{pid}_{n}.dat"));
    let deck_path = std::env::temp_dir().join(format!("vyges_ccs_{pid}_{n}.sp"));
    let d = spice::deck_ccs(
        &format!("ccs {} slew={slew} load={load}", job.cell),
        &includes,
        &job.osdi,
        subckt_call,
        in_pin,
        out_pin,
        job.vdd,
        slew,
        load,
        rising_input,
        dat.to_string_lossy().as_ref(),
    );
    std::fs::write(&deck_path, d.as_bytes()).map_err(|e| CharError::Io(e.to_string()))?;
    let out = Command::new("ngspice")
        .arg("-b")
        .arg(&deck_path)
        .arg("--no-spiceinit")
        .output()
        .map_err(|e| CharError::Io(e.to_string()))?;
    let _ = std::fs::remove_file(&deck_path);
    let text = std::fs::read_to_string(&dat).map_err(|e| {
        CharError::Sim(format!(
            "no CCS waveform written ({e}); ngspice: {}",
            String::from_utf8_lossy(&out.stderr)
        ))
    })?;
    let _ = std::fs::remove_file(&dat);
    let samples = spice::parse_wrdata(&text);
    if samples.len() < 2 {
        return Err(CharError::Sim("CCS waveform has < 2 points".into()));
    }
    // sub-sample to ~24 points; time in ns, current magnitude in mA.
    let (time, current) = subsample(&samples, 48);
    Ok(Waveform { slew, load, ref_time: 1.0 + slew / 2.0, time, current })
}

/// Characterize one CCS receiver-capacitance point: drive the input pin through a
/// sense source, integrate the captured input current Q = ∫i·dt over the two halves
/// of the input ramp, and return `(C1, C2)` in pF. C1 = |Q| over [ramp start, input
/// 50%] / (Vdd/2); C2 = over [input 50%, ramp end] / (Vdd/2). C2 carries the Miller
/// inflation from the switching output, C1 is the static (pre-threshold) gate cap.
#[allow(clippy::too_many_arguments)]
fn run_recv_point(
    job: &CharJob,
    subckt_call: &str,
    in_pin: &str,
    out_pin: &str,
    slew: f64,
    load: f64,
    rising_input: bool,
) -> Result<(f64, f64), CharError> {
    let includes: Vec<String> =
        std::iter::once(job.netlist.clone()).chain(job.models.iter().cloned()).collect();
    let n = DECK_SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let dat = std::env::temp_dir().join(format!("vyges_recv_{pid}_{n}.dat"));
    let deck_path = std::env::temp_dir().join(format!("vyges_recv_{pid}_{n}.sp"));
    let d = spice::deck_recv(
        &format!("recv {} slew={slew} load={load}", job.cell),
        &includes,
        &job.osdi,
        subckt_call,
        in_pin,
        out_pin,
        job.vdd,
        slew,
        load,
        rising_input,
        dat.to_string_lossy().as_ref(),
    );
    std::fs::write(&deck_path, d.as_bytes()).map_err(|e| CharError::Io(e.to_string()))?;
    let out = Command::new("ngspice")
        .arg("-b")
        .arg(&deck_path)
        .arg("--no-spiceinit")
        .output()
        .map_err(|e| CharError::Io(e.to_string()))?;
    let _ = std::fs::remove_file(&deck_path);
    let text = std::fs::read_to_string(&dat).map_err(|e| {
        CharError::Sim(format!(
            "no receiver waveform written ({e}); ngspice: {}",
            String::from_utf8_lossy(&out.stderr)
        ))
    })?;
    let _ = std::fs::remove_file(&dat);
    let samples = spice::parse_wrdata(&text);
    if samples.len() < 2 {
        return Err(CharError::Sim("receiver waveform has < 2 points".into()));
    }
    // ramp window in seconds: starts at 1ns, 50% at 1+slew/2, ends at 1+slew.
    let (t_start, t_mid, t_end) =
        (1e-9, (1.0 + slew / 2.0) * 1e-9, (1.0 + slew) * 1e-9);
    let dv = job.vdd / 2.0; // voltage swing per segment
    let q1 = trapz_window(&samples, t_start, t_mid).abs();
    let q2 = trapz_window(&samples, t_mid, t_end).abs();
    // C = Q/ΔV (Coulombs/Volt = F); ×1e12 -> pF.
    Ok((q1 / dv * 1e12, q2 / dv * 1e12))
}

/// Trapezoidal integral of (time_s, current_A) samples over `[t0, t1]` (Coulombs).
/// Samples are assumed time-sorted; only the in-window interval contributions count.
fn trapz_window(samples: &[(f64, f64)], t0: f64, t1: f64) -> f64 {
    let mut q = 0.0;
    for w in samples.windows(2) {
        let (ta, ia) = w[0];
        let (tb, ib) = w[1];
        if tb <= t0 || ta >= t1 {
            continue;
        }
        // clip the segment to [t0, t1], linearly interpolating current at the clips.
        let (mut xa, mut ya) = (ta, ia);
        let (mut xb, mut yb) = (tb, ib);
        if xa < t0 {
            ya = lerp(ta, ia, tb, ib, t0);
            xa = t0;
        }
        if xb > t1 {
            yb = lerp(ta, ia, tb, ib, t1);
            xb = t1;
        }
        q += 0.5 * (ya + yb) * (xb - xa);
    }
    q
}

/// Linear interpolation of the current value at time `t` on the segment (ta,ia)-(tb,ib).
fn lerp(ta: f64, ia: f64, tb: f64, ib: f64, t: f64) -> f64 {
    if (tb - ta).abs() < f64::EPSILON {
        return ia;
    }
    ia + (ib - ia) * (t - ta) / (tb - ta)
}

/// Evenly sub-sample (time_s, current_A) samples to at most `n` points,
/// converting to (ns, |mA|).
fn subsample(samples: &[(f64, f64)], n: usize) -> (Vec<f64>, Vec<f64>) {
    let len = samples.len();
    let step = (len as f64 / n as f64).max(1.0);
    let mut time = Vec::new();
    let mut current = Vec::new();
    let mut k = 0.0;
    while (k as usize) < len {
        let (t, i) = samples[k as usize];
        time.push(t * 1e9); // s -> ns
        current.push((i * 1e3).abs()); // A -> mA, magnitude (timing carrier)
        k += step;
    }
    if *time.last().unwrap_or(&0.0) < samples[len - 1].0 * 1e9 {
        time.push(samples[len - 1].0 * 1e9);
        current.push((samples[len - 1].1 * 1e3).abs());
    }
    (time, current)
}

/// Sample standard deviation (n−1); 0.0 for fewer than two samples.
pub fn stddev(xs: &[f64]) -> f64 {
    let n = xs.len();
    if n < 2 {
        return 0.0;
    }
    let mean = xs.iter().sum::<f64>() / n as f64;
    let var = xs.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / (n - 1) as f64;
    var.sqrt()
}

/// The deck fragment wiring a sequential cell: clock/data/out pins keep their net
/// names, power/ground tie to VDD/VSS. Any other pin (e.g. async set/reset) is a
/// hard error — v1 sequential characterization handles the plain D-flop.
fn seq_wiring(job: &CharJob) -> Result<String, CharError> {
    let netlist = std::fs::read_to_string(&job.netlist)
        .map_err(|e| CharError::Netlist(format!("{}: {e}", job.netlist)))?;
    let pins = spice::parse_subckt_pins(&netlist, &job.cell).ok_or_else(|| {
        CharError::Netlist(format!("no `.subckt {}` found in {}", job.cell, job.netlist))
    })?;
    let mut nodes = Vec::with_capacity(pins.len());
    for pin in &pins {
        let node = if pin.eq_ignore_ascii_case(&job.clock_pin) {
            job.clock_pin.clone()
        } else if pin.eq_ignore_ascii_case(&job.data_pin) {
            job.data_pin.clone()
        } else if pin.eq_ignore_ascii_case(&job.out_pin) {
            job.out_pin.clone()
        } else if job.power.iter().any(|p| p.eq_ignore_ascii_case(pin)) {
            "VDD".to_string()
        } else if job.ground.iter().any(|p| p.eq_ignore_ascii_case(pin)) {
            "VSS".to_string()
        } else {
            return Err(CharError::Netlist(format!(
                "subckt pin {pin:?} of {} is not clock/data/out or power/ground; v1 \
                 sequential characterization handles the plain D-flop (no async set/reset)",
                job.cell
            )));
        };
        nodes.push(node);
    }
    Ok(format!("X1 {} {}", nodes.join(" "), job.cell))
}

/// Run one sequential deck and return `(CK->Q delay, Q transition)` in seconds, or
/// `None` for either if the measure failed (a missing `ckq` means the capture failed).
#[allow(clippy::too_many_arguments)]
fn run_seq(
    job: &CharJob,
    wiring: &str,
    clk_slew: f64,
    q_load: f64,
    rising_clock: bool,
    data_init: f64,
    data_slew: f64,
    edges: &[(f64, f64)],
    q_rise: bool,
) -> Result<(Option<f64>, Option<f64>), CharError> {
    let includes: Vec<String> =
        std::iter::once(job.netlist.clone()).chain(job.models.iter().cloned()).collect();
    let d = spice::deck_seq(
        &format!("seq {} clk={clk_slew}", job.cell),
        &includes,
        &job.osdi,
        wiring,
        &job.clock_pin,
        &job.data_pin,
        &job.out_pin,
        job.vdd,
        clk_slew,
        q_load,
        rising_clock,
        data_init,
        data_slew,
        edges,
        q_rise,
    );
    let n = DECK_SEQ.fetch_add(1, Ordering::Relaxed);
    let deck_path =
        std::env::temp_dir().join(format!("vyges_seq_{}_{}.sp", std::process::id(), n));
    std::fs::write(&deck_path, d.as_bytes()).map_err(|e| CharError::Io(e.to_string()))?;
    let out = Command::new("ngspice")
        .arg("-b")
        .arg(&deck_path)
        .arg("--no-spiceinit")
        .output()
        .map_err(|e| CharError::Io(e.to_string()))?;
    let _ = std::fs::remove_file(&deck_path);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let m = spice::parse_measures(&text);
    // a non-positive or absent ckq means the edge wasn't captured (failure).
    let ckq = m.get("ckq").copied().filter(|&v| v.is_finite() && v > 0.0);
    let slew = m.get("q_slew").copied().filter(|&v| v.is_finite() && v > 0.0);
    Ok((ckq, slew))
}

/// Capture-clock 50% time (ns) — the prime edge is at 2 ns, capture at 8 ns.
const T_CAPTURE: f64 = 8.0;

/// Bisection (pushout) search for a setup or hold constraint (ns). `measure(sep)`
/// returns the CK->Q delay for a data-to-clock separation `sep`; the constraint is
/// the smallest `sep` at which the delay is still within `1+thresh` of the stable
/// reference delay at `hi` (large separation = data far from the clock = easy).
fn find_constraint(
    measure: impl Fn(f64) -> Result<Option<f64>, CharError>,
    lo0: f64,
    hi0: f64,
    thresh: f64,
) -> Result<f64, CharError> {
    let d0 = match measure(hi0)? {
        Some(d) => d,
        None => return Ok(f64::NAN), // even the easy point failed -> uncharacterizable
    };
    let target = d0 * (1.0 + thresh);
    let (mut lo, mut hi) = (lo0, hi0);
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        let pass = matches!(measure(mid)?, Some(d) if d <= target);
        if pass {
            hi = mid; // can we move closer to the clock and still pass?
        } else {
            lo = mid;
        }
    }
    Ok(hi)
}

/// Characterize a sequential cell: setup/hold constraints (bisection per grid point)
/// and the CK->Q delay arc.
fn characterize_seq(job: &CharJob) -> Result<liberty::SeqCell, CharError> {
    let wiring = seq_wiring(job)?;
    let rising = !job.clock_edge.eq_ignore_ascii_case("falling");
    let (ns, nl) = (job.slews.len(), job.loads.len());
    let vdd = job.vdd;
    let mut cell = liberty::SeqCell {
        cell: job.cell.clone(),
        clock_pin: job.clock_pin.clone(),
        data_pin: job.data_pin.clone(),
        out_pin: job.out_pin.clone(),
        rising_edge: rising,
        setup_rise: Table::new(ns, ns),
        setup_fall: Table::new(ns, ns),
        hold_rise: Table::new(ns, ns),
        hold_fall: Table::new(ns, ns),
        ckq_rise: Table::new(ns, nl),
        ckq_fall: Table::new(ns, nl),
        ckq_rise_trans: Table::new(ns, nl),
        ckq_fall_trans: Table::new(ns, nl),
    };

    // CK->Q delay arc: sweep clock slew x Q load, data switched generously early.
    let fast_data = job.slews[0];
    for (i, &cs) in job.slews.iter().enumerate() {
        for (j, &load) in job.loads.iter().enumerate() {
            // rising Q: data 0->1 latched at capture; falling Q: data 1->0.
            let (ckr, sr) =
                run_seq(job, &wiring, cs, load, rising, 0.0, fast_data, &[(T_CAPTURE - 1.5, vdd)], true)?;
            let (ckf, sf) =
                run_seq(job, &wiring, cs, load, rising, vdd, fast_data, &[(T_CAPTURE - 1.5, 0.0)], false)?;
            cell.ckq_rise.values[i][j] = ckr.unwrap_or(0.0) * 1e9;
            cell.ckq_fall.values[i][j] = ckf.unwrap_or(0.0) * 1e9;
            cell.ckq_rise_trans.values[i][j] = sr.unwrap_or(0.0) * 1e9;
            cell.ckq_fall_trans.values[i][j] = sf.unwrap_or(0.0) * 1e9;
        }
    }

    // setup/hold: sweep clock slew (index_1) x data slew (index_2). Q load fixed.
    let q_load = job.loads[0];
    for (i, &cs) in job.slews.iter().enumerate() {
        for (k, &ds) in job.slews.iter().enumerate() {
            // setup, rising data (0->1, capture rising Q): data 50% at T-sep.
            cell.setup_rise.values[i][k] = find_constraint(
                |sep| Ok(run_seq(job, &wiring, cs, q_load, rising, 0.0, ds, &[(T_CAPTURE - sep, vdd)], true)?.0),
                -ds,
                3.0,
                0.10,
            )?;
            // setup, falling data (1->0, capture falling Q).
            cell.setup_fall.values[i][k] = find_constraint(
                |sep| Ok(run_seq(job, &wiring, cs, q_load, rising, vdd, ds, &[(T_CAPTURE - sep, 0.0)], false)?.0),
                -ds,
                3.0,
                0.10,
            )?;
            // hold, rising data: data 0->1 latched early, releases 1->0 at T+sep.
            cell.hold_rise.values[i][k] = find_constraint(
                |sep| {
                    Ok(run_seq(
                        job, &wiring, cs, q_load, rising, 0.0, ds,
                        &[(T_CAPTURE - 2.0, vdd), (T_CAPTURE + sep, 0.0)], true,
                    )?
                    .0)
                },
                -1.8,
                3.0,
                0.10,
            )?;
            // hold, falling data: data 1->0 latched early, releases 0->1 at T+sep.
            cell.hold_fall.values[i][k] = find_constraint(
                |sep| {
                    Ok(run_seq(
                        job, &wiring, cs, q_load, rising, vdd, ds,
                        &[(T_CAPTURE - 2.0, 0.0), (T_CAPTURE + sep, vdd)], false,
                    )?
                    .0)
                },
                -1.8,
                3.0,
                0.10,
            )?;
        }
    }
    Ok(cell)
}

/// Full run: characterize and render a `.lib`.
pub fn run_to_lib(job: &CharJob) -> Result<String, CharError> {
    let lib = format!("{}_char", job.cell);
    match characterize(job)? {
        Characterized::Comb(arcs) => {
            Ok(liberty::render(&lib, &Units::default(), &job.slews, &job.loads, &arcs))
        }
        Characterized::Seq(cell) => {
            Ok(liberty::render_seq(&lib, &Units::default(), &job.slews, &job.loads, &cell))
        }
    }
}
