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

/// Characterize every arc of the cell into NLDM tables (delays/transitions in ns).
/// One `Arc` per `arc:` spec; the renderer groups them into a single cell.
pub fn characterize(job: &CharJob) -> Result<Vec<Arc>, CharError> {
    if !ngspice_available() {
        return Err(CharError::NgspiceNotFound);
    }
    job.arcs.iter().map(|spec| characterize_arc(job, spec)).collect()
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

/// Full run: characterize and render a `.lib`.
pub fn run_to_lib(job: &CharJob) -> Result<String, CharError> {
    let arcs = characterize(job)?;
    Ok(liberty::render(&format!("{}_char", job.cell), &Units::default(), &job.slews, &job.loads, &arcs))
}
