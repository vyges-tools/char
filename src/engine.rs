//! Characterization engine: sweep slew x load, simulate, fill NLDM tables.
//!
//! Shells out to `ngspice` per measurement point (the file/CLI boundary). When
//! `ngspice` is absent (off the EDA host) `run` returns `NgspiceNotFound` so the
//! pure pieces (deck gen, parsing, Liberty emit) can still be exercised offline.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::job::{AutoCfg, CharJob, SparseCfg};

/// Per-process counter for unique temp deck filenames.
static DECK_SEQ: AtomicUsize = AtomicUsize::new(0);
use crate::liberty::{self, Arc, Table, Units, Waveform};
use crate::sparse::{ArcModels, ArcPoint};
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
    out_rises: bool,
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
        out_rises,
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

/// Run an internal-power deck; returns the integrated VDD supply charge (Coulombs)
/// over the switching event (`qvdd` = ∫i(VVDD)dt).
#[allow(clippy::too_many_arguments)]
fn run_power_arc(
    job: &CharJob,
    subckt_call: &str,
    in_pin: &str,
    out_pin: &str,
    slew: f64,
    load: f64,
    rising_input: bool,
) -> Result<f64, CharError> {
    let includes: Vec<String> =
        std::iter::once(job.netlist.clone()).chain(job.models.iter().cloned()).collect();
    let d = spice::deck_power_arc(
        &format!("pwr {} slew={slew} load={load}", job.cell),
        &includes,
        &job.osdi,
        subckt_call,
        in_pin,
        out_pin,
        job.vdd,
        slew,
        load,
        rising_input,
    );
    Ok(run_deck(&d, "qvdd")?.0.unwrap_or(0.0))
}

/// Run a leakage deck; returns the quiescent VDD current (A) at the held state.
fn run_leakage(job: &CharJob, wiring: &str) -> Result<f64, CharError> {
    let includes: Vec<String> =
        std::iter::once(job.netlist.clone()).chain(job.models.iter().cloned()).collect();
    let d = spice::deck_leakage(&format!("leak {}", job.cell), &includes, &job.osdi, wiring, job.vdd);
    Ok(run_deck(&d, "ileak")?.0.unwrap_or(0.0))
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
        if job.auto.is_some() {
            return Err(CharError::Sim(
                "--auto is not supported for sequential cells (v1); use --sparse or a dense run".into(),
            ));
        }
        if let Some(cfg) = &job.sparse {
            if !job.reset_pin.is_empty() || !job.set_pin.is_empty() {
                return Err(CharError::Sim(
                    "--sparse for sequential cells supports plain D-flops only (v1); async \
                     set/reset must be characterized dense"
                        .into(),
                ));
            }
            return Ok(Characterized::Seq(Box::new(characterize_seq_sparse(job, cfg)?)));
        }
        Ok(Characterized::Seq(Box::new(characterize_seq(job)?)))
    } else if let Some(cfg) = &job.auto {
        // Self-tuning active sampling: grow the simulated set to a target accuracy.
        let arcs: Vec<Arc> = job
            .arcs
            .iter()
            .map(|spec| characterize_arc_auto(job, spec, cfg))
            .collect::<Result<_, _>>()?;
        Ok(Characterized::Comb(arcs))
    } else if let Some(cfg) = &job.sparse {
        // Sparse-sweep: simulate a coarse grid per arc, fit a surrogate, fill the dense
        // grid. NLDM-only (the CLI rejects --sparse with power/ccs/recv/LVF).
        let arcs: Vec<Arc> = job
            .arcs
            .iter()
            .map(|spec| characterize_arc_sparse(job, spec, cfg))
            .collect::<Result<_, _>>()?;
        Ok(Characterized::Comb(arcs))
    } else {
        let mut arcs: Vec<Arc> =
            job.arcs.iter().map(|spec| characterize_arc(job, spec)).collect::<Result<_, _>>()?;
        // Leakage is per-cell (per input state); compute once and carry it on every
        // arc (the renderer reads it from the first arc of each cell).
        if job.power_char {
            let leak = characterize_leakage(job)?;
            for a in &mut arcs {
                a.leakage = leak.clone();
            }
        }
        Ok(Characterized::Comb(arcs))
    }
}

/// The cell's combinational input pins: the union of every arc's in_pin plus the
/// side inputs, in first-seen order.
fn cell_inputs(job: &CharJob) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for spec in &job.arcs {
        if !v.iter().any(|p| p.eq_ignore_ascii_case(&spec.in_pin)) {
            v.push(spec.in_pin.clone());
        }
        for (sp, _) in &spec.side {
            if !v.iter().any(|p| p.eq_ignore_ascii_case(sp)) {
                v.push(sp.clone());
            }
        }
    }
    v
}

/// Wire the cell for leakage: every input pin held at the bit pattern `bits`
/// (LSB = inputs[0]), outputs floated to their own nets, power/ground tied.
fn leak_wiring(job: &CharJob, inputs: &[String], bits: usize) -> Result<String, CharError> {
    let netlist = std::fs::read_to_string(&job.netlist)
        .map_err(|e| CharError::Netlist(format!("{}: {e}", job.netlist)))?;
    let pins = spice::parse_subckt_pins(&netlist, &job.cell).ok_or_else(|| {
        CharError::Netlist(format!("no `.subckt {}` found in {}", job.cell, job.netlist))
    })?;
    let mut sources = String::new();
    let mut nodes = Vec::with_capacity(pins.len());
    for pin in &pins {
        let node = if let Some(idx) = inputs.iter().position(|p| p.eq_ignore_ascii_case(pin)) {
            let v = if (bits >> idx) & 1 == 1 { job.vdd } else { 0.0 };
            sources.push_str(&format!("V{pin} {pin} 0 {v}\n"));
            pin.clone()
        } else if job.power.iter().any(|p| p.eq_ignore_ascii_case(pin)) {
            "VDD".to_string()
        } else if job.ground.iter().any(|p| p.eq_ignore_ascii_case(pin)) {
            "VSS".to_string()
        } else {
            pin.clone() // output (or other) — float to its own net
        };
        nodes.push(node);
    }
    Ok(format!("{sources}X1 {} {}", nodes.join(" "), job.cell))
}

/// Characterize per-input-state static leakage (nW). Enumerates all 2^n input
/// states for small cells (n <= 4), else samples the all-low and all-high states.
/// Returns `(when_expr, nW)` per state.
fn characterize_leakage(job: &CharJob) -> Result<Vec<(String, f64)>, CharError> {
    let inputs = cell_inputs(job);
    if inputs.is_empty() {
        return Ok(Vec::new());
    }
    let n = inputs.len();
    let states: Vec<usize> =
        if n <= 4 { (0..(1usize << n)).collect() } else { vec![0, (1usize << n) - 1] };
    let mut out = Vec::with_capacity(states.len());
    for bits in states {
        let wiring = leak_wiring(job, &inputs, bits)?;
        let ileak = run_leakage(job, &wiring)?;
        let nw = ileak.abs() * job.vdd * 1e9; // W -> nW
        let when = inputs
            .iter()
            .enumerate()
            .map(|(i, p)| if (bits >> i) & 1 == 1 { p.clone() } else { format!("!{p}") })
            .collect::<Vec<_>>()
            .join("&");
        out.push((when, nw));
    }
    Ok(out)
}

/// Run `f` over `items`, preserving order and propagating the first error. Uses up to
/// `threads` worker threads (work-stealing via a shared index); `threads <= 1` runs
/// sequentially with no thread overhead. The simulator points are independent
/// subprocesses (unique temp decks via `DECK_SEQ`), so this is safe to parallelize.
fn parallel_try<I, T>(
    items: &[I],
    threads: usize,
    f: impl Fn(&I) -> Result<T, CharError> + Sync,
) -> Result<Vec<T>, CharError>
where
    I: Sync,
    T: Send,
{
    if threads <= 1 || items.len() <= 1 {
        return items.iter().map(&f).collect();
    }
    let next = AtomicUsize::new(0);
    let slots: Vec<std::sync::Mutex<Option<Result<T, CharError>>>> =
        (0..items.len()).map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|sc| {
        for _ in 0..threads.min(items.len()) {
            sc.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= items.len() {
                    break;
                }
                let r = f(&items[i]);
                *slots[i].lock().unwrap() = Some(r);
            });
        }
    });
    let mut out = Vec::with_capacity(items.len());
    for s in slots {
        match s.into_inner().unwrap() {
            Some(Ok(t)) => out.push(t),
            Some(Err(e)) => return Err(e),
            None => return Err(CharError::Sim("parallel: missing result".into())),
        }
    }
    Ok(out)
}

/// Everything measured at one (slew, load) grid point of an arc.
struct PointData {
    cell_rise: f64,
    rise_transition: f64,
    cell_fall: f64,
    fall_transition: f64,
    sigma_rise: f64,
    sigma_fall: f64,
    ccs_rise: Option<Waveform>,
    ccs_fall: Option<Waveform>,
    recv: Option<(f64, f64, f64, f64)>, // c1_rise, c2_rise, c1_fall, c2_fall
    int: Option<(f64, f64)>,            // internal energy rise, fall (pJ)
}

/// Measure all enabled metrics at one (slew, load) point — the per-point work unit the
/// sweep parallelizes over.
fn measure_point(
    job: &CharJob,
    instance: &str,
    in_pin: &str,
    out_pin: &str,
    slew: f64,
    load: f64,
    positive: bool,
) -> Result<PointData, CharError> {
    // cell_rise: output rises; cell_fall: output falls.
    let (dr, tr) = run_point(job, instance, in_pin, out_pin, slew, load, positive, true, None)?;
    let (df, tf) = run_point(job, instance, in_pin, out_pin, slew, load, !positive, false, None)?;

    // LVF: Monte-Carlo over mismatch -> per-edge delay sigma (ns).
    let (mut sigma_rise, mut sigma_fall) = (0.0, 0.0);
    if job.montecarlo > 0 {
        let mut rise = Vec::with_capacity(job.montecarlo);
        let mut fall = Vec::with_capacity(job.montecarlo);
        for k in 0..job.montecarlo as u64 {
            rise.push(run_point(job, instance, in_pin, out_pin, slew, load, positive, true, Some(k))?.0 * 1e9);
            fall.push(run_point(job, instance, in_pin, out_pin, slew, load, !positive, false, Some(k))?.0 * 1e9);
        }
        sigma_rise = stddev(&rise);
        sigma_fall = stddev(&fall);
    }

    // CCS: driver output-current waveform per edge.
    let (ccs_rise, ccs_fall) = if job.ccs {
        (
            Some(run_ccs_point(job, instance, in_pin, out_pin, slew, load, false)?),
            Some(run_ccs_point(job, instance, in_pin, out_pin, slew, load, true)?),
        )
    } else {
        (None, None)
    };

    // CCS receiver capacitance (C1 before / C2 after the input 50% crossing).
    let recv = if job.recv {
        let (c1r, c2r) = run_recv_point(job, instance, in_pin, out_pin, slew, load, true)?;
        let (c1f, c2f) = run_recv_point(job, instance, in_pin, out_pin, slew, load, false)?;
        Some((c1r, c2r, c1f, c2f))
    } else {
        None
    };

    // Internal switching energy (pJ): supply energy minus the load-charging part on rise.
    let int = if job.power_char {
        let half_cv2 = 0.5 * (load * 1e-12) * job.vdd * job.vdd; // J
        let qr = run_power_arc(job, instance, in_pin, out_pin, slew, load, false)?;
        let er = (job.vdd * qr.abs() - half_cv2).max(0.0) * 1e12; // pJ
        let qf = run_power_arc(job, instance, in_pin, out_pin, slew, load, true)?;
        Some((er, (job.vdd * qf.abs()) * 1e12))
    } else {
        None
    };

    Ok(PointData {
        cell_rise: dr * 1e9,
        rise_transition: tr * 1e9,
        cell_fall: df * 1e9,
        fall_transition: tf * 1e9,
        sigma_rise,
        sigma_fall,
        ccs_rise,
        ccs_fall,
        recv,
        int,
    })
}

/// Characterize a single timing arc (in_pin -> out_pin, side inputs held). The
/// per-(slew,load) points run across `job.threads` workers; results scatter back in
/// grid order, so the output is identical to a sequential sweep.
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
        int_rise: Table::new(ns, nl),
        int_fall: Table::new(ns, nl),
        leakage: Vec::new(),
    };
    // cell_rise/cell_fall are keyed by the OUTPUT edge. The input edge that drives a
    // rising output depends on unateness (negative-unate rises on a falling input).
    let positive = spec.sense == "positive_unate";
    // (i, j, slew, load) in grid (row-major) order; parallel-mapped, scattered back.
    let pts: Vec<(usize, usize, f64, f64)> = job
        .slews
        .iter()
        .enumerate()
        .flat_map(|(i, &s)| job.loads.iter().enumerate().map(move |(j, &l)| (i, j, s, l)))
        .collect();
    let data = parallel_try(&pts, job.threads, |&(_, _, slew, load)| {
        measure_point(job, &instance, in_pin, out_pin, slew, load, positive)
    })?;
    for (&(i, j, _, _), d) in pts.iter().zip(&data) {
        arc.cell_rise.values[i][j] = d.cell_rise;
        arc.rise_transition.values[i][j] = d.rise_transition;
        arc.cell_fall.values[i][j] = d.cell_fall;
        arc.fall_transition.values[i][j] = d.fall_transition;
        arc.sigma_rise.values[i][j] = d.sigma_rise;
        arc.sigma_fall.values[i][j] = d.sigma_fall;
        if let Some((c1r, c2r, c1f, c2f)) = d.recv {
            arc.recv_c1_rise.values[i][j] = c1r;
            arc.recv_c2_rise.values[i][j] = c2r;
            arc.recv_c1_fall.values[i][j] = c1f;
            arc.recv_c2_fall.values[i][j] = c2f;
        }
        if let Some((er, ef)) = d.int {
            arc.int_rise.values[i][j] = er;
            arc.int_fall.values[i][j] = ef;
        }
    }
    // CCS waveforms are positional Vecs — push in grid order.
    if job.ccs {
        for d in data {
            if let (Some(r), Some(f)) = (d.ccs_rise, d.ccs_fall) {
                arc.ccs_rise.push(r);
                arc.ccs_fall.push(f);
            }
        }
    }
    Ok(arc)
}

/// Simulate the four NLDM values (cell_rise/fall, rise/fall_transition, ns) at each
/// `(slew, load)` in `pts` for one arc — the measurement primitive the sparse sweep uses.
fn measure_arc_points(
    job: &CharJob,
    spec: &crate::job::ArcSpec,
    pts: &[(f64, f64)],
) -> Result<Vec<ArcPoint>, CharError> {
    let instance = arc_wiring(job, spec)?;
    let (in_pin, out_pin) = (spec.in_pin.as_str(), spec.out_pin.as_str());
    let positive = spec.sense == "positive_unate";
    parallel_try(pts, job.threads, |&(slew, load)| {
        let (dr, tr) = run_point(job, &instance, in_pin, out_pin, slew, load, positive, true, None)?;
        let (df, tf) = run_point(job, &instance, in_pin, out_pin, slew, load, !positive, false, None)?;
        Ok(ArcPoint {
            slew,
            load,
            cell_rise: dr * 1e9,
            cell_fall: df * 1e9,
            rise_transition: tr * 1e9,
            fall_transition: tf * 1e9,
        })
    })
}

/// Sparse-sweep + surrogate fill for one arc: simulate `cfg.sim_slews × cfg.sim_loads`,
/// fit a log surrogate per metric, and predict the job's full dense grid. With
/// `cfg.verify > 0`, re-simulate a few un-fitted dense points and report measured-vs-
/// predicted error (the honest accuracy of this run).
fn characterize_arc_sparse(
    job: &CharJob,
    spec: &crate::job::ArcSpec,
    cfg: &SparseCfg,
) -> Result<Arc, CharError> {
    let sim_pts: Vec<(f64, f64)> =
        cfg.sim_slews.iter().flat_map(|&s| cfg.sim_loads.iter().map(move |&l| (s, l))).collect();
    let measured = measure_arc_points(job, spec, &sim_pts)?;
    let models = ArcModels::fit(&measured, cfg.degree).ok_or_else(|| {
        CharError::Sim("surrogate fit failed (too few usable sparse points)".into())
    })?;
    let (cell_rise, cell_fall, rise_transition, fall_transition) =
        models.fill(&job.slews, &job.loads);
    let (ns, nl) = (job.slews.len(), job.loads.len());
    let arc = Arc {
        cell: job.cell.clone(),
        in_pin: spec.in_pin.clone(),
        out_pin: spec.out_pin.clone(),
        sense: spec.sense.clone(),
        cell_rise,
        cell_fall,
        rise_transition,
        fall_transition,
        sigma_rise: Table::new(ns, nl),
        sigma_fall: Table::new(ns, nl),
        ccs_rise: Vec::new(),
        ccs_fall: Vec::new(),
        recv_c1_rise: Table::new(ns, nl),
        recv_c2_rise: Table::new(ns, nl),
        recv_c1_fall: Table::new(ns, nl),
        recv_c2_fall: Table::new(ns, nl),
        int_rise: Table::new(ns, nl),
        int_fall: Table::new(ns, nl),
        leakage: Vec::new(),
    };
    eprintln!(
        "sparse {}->{}: simulated {} point(s), predicted {} (of {} dense)",
        spec.in_pin,
        spec.out_pin,
        sim_pts.len(),
        ns * nl - sim_pts.len().min(ns * nl),
        ns * nl
    );
    if cfg.verify > 0 {
        verify_sparse(job, spec, cfg, &models)?;
    }
    Ok(arc)
}

/// One NLDM metric accessor on a measured point (for the verify error loop).
type MetricFn = fn(&ArcPoint) -> f64;

/// Re-simulate up to `cfg.verify` dense points NOT in the sparse fit set and report
/// measured-vs-predicted error per metric (% of that metric's peak |value|).
fn verify_sparse(
    job: &CharJob,
    spec: &crate::job::ArcSpec,
    cfg: &SparseCfg,
    models: &ArcModels,
) -> Result<(), CharError> {
    let near = |x: f64, xs: &[f64]| xs.iter().any(|&y| (x - y).abs() <= 1e-12 * x.abs().max(1.0));
    let candidates: Vec<(f64, f64)> = job
        .slews
        .iter()
        .flat_map(|&s| job.loads.iter().map(move |&l| (s, l)))
        .filter(|&(s, l)| !(near(s, &cfg.sim_slews) && near(l, &cfg.sim_loads)))
        .collect();
    if candidates.is_empty() {
        return Ok(());
    }
    let step = (candidates.len() as f64 / cfg.verify as f64).max(1.0);
    let mut vpts = Vec::new();
    let mut k = 0.0;
    while (k as usize) < candidates.len() && vpts.len() < cfg.verify {
        vpts.push(candidates[k as usize]);
        k += step;
    }
    let measured = measure_arc_points(job, spec, &vpts)?;
    let metrics: [(&str, MetricFn); 4] = [
        ("cell_rise", |p| p.cell_rise),
        ("cell_fall", |p| p.cell_fall),
        ("rise_transition", |p| p.rise_transition),
        ("fall_transition", |p| p.fall_transition),
    ];
    for (name, f) in metrics {
        let (mut max_abs, mut sumsq, mut scale) = (0.0f64, 0.0f64, 0.0f64);
        for p in &measured {
            let (cr, cf, rt, ft) = models.predict(p.slew, p.load);
            let pred = match name {
                "cell_rise" => cr,
                "cell_fall" => cf,
                "rise_transition" => rt,
                _ => ft,
            };
            let e = (pred - f(p)).abs();
            max_abs = max_abs.max(e);
            sumsq += e * e;
            scale = scale.max(f(p).abs());
        }
        let n = measured.len().max(1) as f64;
        let den = if scale > 1e-12 { scale } else { 1.0 };
        eprintln!(
            "verify {}->{} {name}: {} pts, max {:.2}% / rms {:.2}% of peak",
            spec.in_pin,
            spec.out_pin,
            measured.len(),
            max_abs / den * 100.0,
            (sumsq / n).sqrt() / den * 100.0
        );
    }
    Ok(())
}

/// `m` evenly-spread indices over `0..n` (endpoints included), deduplicated.
fn stride_idx(n: usize, m: usize) -> Vec<usize> {
    let m = m.clamp(1, n.max(1));
    if m == 1 {
        return vec![0];
    }
    let mut v: Vec<usize> = (0..m).map(|k| (k * (n - 1)) / (m - 1)).collect();
    v.dedup();
    v
}

/// Self-tuning active sampling for one arc: seed a coarse subset of the dense grid, then
/// repeatedly simulate the point in the biggest gap (maximin in log space) until the
/// leave-one-out CV error is within `cfg.target_pct` (% of peak) or `cfg.max_points` are
/// simulated. Fill the dense grid from the final surrogate, splicing the exact measured
/// values back into the points that were actually simulated.
fn characterize_arc_auto(job: &CharJob, spec: &crate::job::ArcSpec, cfg: &AutoCfg) -> Result<Arc, CharError> {
    let (ns, nl) = (job.slews.len(), job.loads.len());
    let coord = |i: usize, j: usize| (job.slews[i], job.loads[j]);

    // Seed: a spread RxC subset of the dense grid.
    let seed_ij: Vec<(usize, usize)> = stride_idx(ns, cfg.seed.0)
        .into_iter()
        .flat_map(|i| stride_idx(nl, cfg.seed.1).into_iter().map(move |j| (i, j)))
        .collect();
    let mut sampled_ij = seed_ij.clone();
    let seed_pts: Vec<(f64, f64)> = seed_ij.iter().map(|&(i, j)| coord(i, j)).collect();
    let mut measured = measure_arc_points(job, spec, &seed_pts)?;

    let max_points = cfg.max_points.min(ns * nl);
    loop {
        let cv = crate::sparse::loo_cv_rms_pct(&measured, cfg.degree);
        if measured.len() >= max_points || cv <= cfg.target_pct {
            break;
        }
        // candidates = dense points not yet sampled; pick the maximin (biggest-gap) one.
        let cand_ij: Vec<(usize, usize)> = (0..ns)
            .flat_map(|i| (0..nl).map(move |j| (i, j)))
            .filter(|ij| !sampled_ij.contains(ij))
            .collect();
        if cand_ij.is_empty() {
            break;
        }
        let cand_pts: Vec<(f64, f64)> = cand_ij.iter().map(|&(i, j)| coord(i, j)).collect();
        let sampled_pts: Vec<(f64, f64)> = sampled_ij.iter().map(|&(i, j)| coord(i, j)).collect();
        let Some(next) = crate::sparse::maximin_next(&sampled_pts, &cand_pts) else {
            break;
        };
        let nij = cand_ij[cand_pts.iter().position(|&p| p == next).unwrap()];
        measured.extend(measure_arc_points(job, spec, &[next])?);
        sampled_ij.push(nij);
    }

    let models = ArcModels::fit(&measured, cfg.degree)
        .ok_or_else(|| CharError::Sim("surrogate fit failed (too few usable points)".into()))?;
    let (mut cell_rise, mut cell_fall, mut rise_transition, mut fall_transition) =
        models.fill(&job.slews, &job.loads);
    // Splice the exact measured values back into the simulated cells.
    for (&(i, j), p) in sampled_ij.iter().zip(&measured) {
        cell_rise.values[i][j] = p.cell_rise;
        cell_fall.values[i][j] = p.cell_fall;
        rise_transition.values[i][j] = p.rise_transition;
        fall_transition.values[i][j] = p.fall_transition;
    }
    let final_cv = crate::sparse::loo_cv_rms_pct(&measured, cfg.degree);
    eprintln!(
        "auto {}->{}: simulated {} of {} dense point(s), CV ~{:.2}% of peak (target {:.2}%)",
        spec.in_pin,
        spec.out_pin,
        measured.len(),
        ns * nl,
        final_cv,
        cfg.target_pct
    );
    Ok(Arc {
        cell: job.cell.clone(),
        in_pin: spec.in_pin.clone(),
        out_pin: spec.out_pin.clone(),
        sense: spec.sense.clone(),
        cell_rise,
        cell_fall,
        rise_transition,
        fall_transition,
        sigma_rise: Table::new(ns, nl),
        sigma_fall: Table::new(ns, nl),
        ccs_rise: Vec::new(),
        ccs_fall: Vec::new(),
        recv_c1_rise: Table::new(ns, nl),
        recv_c2_rise: Table::new(ns, nl),
        recv_c1_fall: Table::new(ns, nl),
        recv_c2_fall: Table::new(ns, nl),
        int_rise: Table::new(ns, nl),
        int_fall: Table::new(ns, nl),
        leakage: Vec::new(),
    })
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
/// names, power/ground tie to VDD/VSS, and any declared async/tie pin (set/reset,
/// scan controls) keeps its own net (driven by a fixed or PWL source in the deck).
/// A pin that is none of these is a hard error — declare it under `tie:`/`reset_pin:`.
fn seq_wiring(job: &CharJob) -> Result<String, CharError> {
    let netlist = std::fs::read_to_string(&job.netlist)
        .map_err(|e| CharError::Netlist(format!("{}: {e}", job.netlist)))?;
    let pins = spice::parse_subckt_pins(&netlist, &job.cell).ok_or_else(|| {
        CharError::Netlist(format!("no `.subckt {}` found in {}", job.cell, job.netlist))
    })?;
    let is_signal = |pin: &str| {
        job.tie.iter().any(|(p, _)| p.eq_ignore_ascii_case(pin))
            || (!job.reset_pin.is_empty() && job.reset_pin.eq_ignore_ascii_case(pin))
            || (!job.set_pin.is_empty() && job.set_pin.eq_ignore_ascii_case(pin))
    };
    let mut nodes = Vec::with_capacity(pins.len());
    for pin in &pins {
        let node = if pin.eq_ignore_ascii_case(&job.clock_pin) {
            job.clock_pin.clone()
        } else if pin.eq_ignore_ascii_case(&job.data_pin) {
            job.data_pin.clone()
        } else if pin.eq_ignore_ascii_case(&job.out_pin) {
            job.out_pin.clone()
        } else if is_signal(pin) {
            pin.clone() // own net, sourced by the deck (tie level or reset PWL)
        } else if job.power.iter().any(|p| p.eq_ignore_ascii_case(pin)) {
            "VDD".to_string()
        } else if job.ground.iter().any(|p| p.eq_ignore_ascii_case(pin)) {
            "VSS".to_string()
        } else {
            return Err(CharError::Netlist(format!(
                "subckt pin {pin:?} of {} is not clock/data/out or power/ground; declare \
                 async/unused inputs under `tie: {pin}=0|1` (or `reset_pin:`)",
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
    ties: &[(String, bool)],
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
        ties,
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
    // Bisect to ~1 ps precision with early exit (capped): a ~5 ns range needs only
    // ~13 halvings to reach 1 ps, vs a fixed 24 — roughly halving the ngspice runs.
    for _ in 0..20 {
        if hi - lo < TOL_NS {
            break;
        }
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

/// Bisection precision for constraint searches (1 ps).
const TOL_NS: f64 = 0.001;

/// Find the async-release time (ns) at which the settled Q crosses mid-rail — the
/// boundary between "the clock captured the data" and "the async value held". The
/// `[lo0, hi0]` range must **bracket** the boundary (one end captures, the other
/// holds); returns `None` if it doesn't (so the caller can leave the constraint
/// uncharacterized rather than pin to a bound).
fn find_release_boundary(
    measure: impl Fn(f64) -> Result<Option<f64>, CharError>,
    lo0: f64,
    hi0: f64,
    half: f64,
) -> Result<Option<f64>, CharError> {
    let captured = |q: Option<f64>| matches!(q, Some(v) if v > half);
    let lo_cap = captured(measure(lo0)?);
    let hi_cap = captured(measure(hi0)?);
    if lo_cap == hi_cap {
        return Ok(None); // boundary not bracketed
    }
    let (mut lo, mut hi) = (lo0, hi0);
    for _ in 0..20 {
        if hi - lo < TOL_NS {
            break;
        }
        let mid = 0.5 * (lo + hi);
        if captured(measure(mid)?) == lo_cap {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Ok(Some(0.5 * (lo + hi)))
}

/// Characterize a sequential cell: setup/hold constraints (bisection per grid point)
/// and the CK->Q delay arc.
fn characterize_seq(job: &CharJob) -> Result<liberty::SeqCell, CharError> {
    let wiring = seq_wiring(job)?;
    let rising = !job.clock_edge.eq_ignore_ascii_case("falling");
    let (ns, nl) = (job.slews.len(), job.loads.len());
    let vdd = job.vdd;
    // Async controls held inactive during setup/hold/CK->Q: the user's tie list plus
    // the set/reset pins at their inactive (de-asserted) level (active-low -> high).
    let mut inactive_ties = job.tie.clone();
    for (p, al) in [(&job.reset_pin, job.reset_active_low), (&job.set_pin, job.set_active_low)] {
        if !p.is_empty() && !inactive_ties.iter().any(|(q, _)| q == p) {
            inactive_ties.push((p.clone(), al));
        }
    }
    let ties = inactive_ties.as_slice();
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
        asyncs: Vec::new(),
    };

    // CK->Q delay arc: sweep clock slew x Q load, data switched generously early.
    // Points are independent -> run across job.threads, scatter back in grid order.
    let fast_data = job.slews[0];
    let ckq_pts: Vec<(usize, usize, f64, f64)> = job
        .slews
        .iter()
        .enumerate()
        .flat_map(|(i, &cs)| job.loads.iter().enumerate().map(move |(j, &load)| (i, j, cs, load)))
        .collect();
    let ckq = parallel_try(&ckq_pts, job.threads, |&(_, _, cs, load)| {
        let (ckr, sr) =
            run_seq(job, &wiring, cs, load, rising, 0.0, fast_data, &[(T_CAPTURE - 1.5, vdd)], true, ties)?;
        let (ckf, sf) =
            run_seq(job, &wiring, cs, load, rising, vdd, fast_data, &[(T_CAPTURE - 1.5, 0.0)], false, ties)?;
        Ok((
            ckr.unwrap_or(0.0) * 1e9,
            ckf.unwrap_or(0.0) * 1e9,
            sr.unwrap_or(0.0) * 1e9,
            sf.unwrap_or(0.0) * 1e9,
        ))
    })?;
    for (&(i, j, _, _), &(cr, cf, srt, sft)) in ckq_pts.iter().zip(&ckq) {
        cell.ckq_rise.values[i][j] = cr;
        cell.ckq_fall.values[i][j] = cf;
        cell.ckq_rise_trans.values[i][j] = srt;
        cell.ckq_fall_trans.values[i][j] = sft;
    }

    // setup/hold: sweep clock slew (index_1) x data slew (index_2). Q load fixed. Each
    // (cs, ds) point is an independent bisection -> parallelize across threads.
    let q_load = job.loads[0];
    let sh_pts: Vec<(usize, usize, f64, f64)> = job
        .slews
        .iter()
        .enumerate()
        .flat_map(|(i, &cs)| job.slews.iter().enumerate().map(move |(k, &ds)| (i, k, cs, ds)))
        .collect();
    let sh = parallel_try(&sh_pts, job.threads, |&(_, _, cs, ds)| {
        let setup_rise = find_constraint(
            |sep| Ok(run_seq(job, &wiring, cs, q_load, rising, 0.0, ds, &[(T_CAPTURE - sep, vdd)], true, ties)?.0),
            -ds,
            3.0,
            0.10,
        )?;
        let setup_fall = find_constraint(
            |sep| Ok(run_seq(job, &wiring, cs, q_load, rising, vdd, ds, &[(T_CAPTURE - sep, 0.0)], false, ties)?.0),
            -ds,
            3.0,
            0.10,
        )?;
        let hold_rise = find_constraint(
            |sep| {
                Ok(run_seq(
                    job, &wiring, cs, q_load, rising, 0.0, ds,
                    &[(T_CAPTURE - 2.0, vdd), (T_CAPTURE + sep, 0.0)], true, ties,
                )?
                .0)
            },
            -1.8,
            3.0,
            0.10,
        )?;
        let hold_fall = find_constraint(
            |sep| {
                Ok(run_seq(
                    job, &wiring, cs, q_load, rising, vdd, ds,
                    &[(T_CAPTURE - 2.0, 0.0), (T_CAPTURE + sep, vdd)], false, ties,
                )?
                .0)
            },
            -1.8,
            3.0,
            0.10,
        )?;
        Ok((setup_rise, setup_fall, hold_rise, hold_fall))
    })?;
    for (&(i, k, _, _), &(sr, sf, hr, hf)) in sh_pts.iter().zip(&sh) {
        cell.setup_rise.values[i][k] = sr;
        cell.setup_fall.values[i][k] = sf;
        cell.hold_rise.values[i][k] = hr;
        cell.hold_fall.values[i][k] = hf;
    }

    // Async set/reset: for each declared control, emit the `ff` preset/clear attribute
    // and characterize its ->Q delay arc + recovery/removal constraints.
    let mut controls: Vec<(String, bool, bool)> = Vec::new(); // (pin, active_low, sets_high)
    if !job.reset_pin.is_empty() {
        controls.push((job.reset_pin.clone(), job.reset_active_low, false));
    }
    if !job.set_pin.is_empty() {
        controls.push((job.set_pin.clone(), job.set_active_low, true));
    }
    for (pin, active_low, sets_high) in controls {
        let expr = if active_low { format!("!{pin}") } else { pin.clone() };
        let mut ctl = liberty::AsyncCtl {
            pin: pin.clone(),
            expr,
            sets_high,
            active_low,
            q: Table::new(ns, nl),
            q_trans: Table::new(ns, nl),
            recovery: Table::new(ns, ns),
            removal: Table::new(ns, ns),
        };
        // the control under test is driven, so tie everything *except* it inactive.
        let other: Vec<(String, bool)> =
            inactive_ties.iter().filter(|(p, _)| p != &pin).cloned().collect();
        // ->Q delay arc (sweep control transition x Q load) — parallel across threads.
        let aq_pts: Vec<(usize, usize, f64, f64)> = job
            .slews
            .iter()
            .enumerate()
            .flat_map(|(i, &asl)| job.loads.iter().enumerate().map(move |(j, &load)| (i, j, asl, load)))
            .collect();
        let aq = parallel_try(&aq_pts, job.threads, |&(_, _, asl, load)| {
            let (aq, sl) =
                run_async_q(job, &wiring, &pin, active_low, sets_high, asl, load, rising, &other)?;
            Ok((aq.unwrap_or(0.0) * 1e9, sl.unwrap_or(0.0) * 1e9))
        })?;
        for (&(i, j, _, _), &(q, qt)) in aq_pts.iter().zip(&aq) {
            ctl.q.values[i][j] = q;
            ctl.q_trans.values[i][j] = qt;
        }
        // recovery/removal: find the single async-release boundary (relative to the
        // clock) where Q flips between "captured the data" and "held the async value",
        // per (clock slew, async slew). recovery = lead margin (clock - boundary),
        // removal = lag margin (boundary - clock); both signed (a flop that samples
        // slightly after the clock 50% tolerates a late release -> negative recovery).
        let half = job.vdd / 2.0;
        let q_load = job.loads[0];
        let rr_pts: Vec<(usize, usize, f64, f64)> = job
            .slews
            .iter()
            .enumerate()
            .flat_map(|(i, &cs)| job.slews.iter().enumerate().map(move |(k, &asl)| (i, k, cs, asl)))
            .collect();
        let rr = parallel_try(&rr_pts, job.threads, |&(_, _, cs, asl)| {
            find_release_boundary(
                |rel| {
                    run_async_constraint(
                        job, &wiring, &pin, active_low, sets_high, cs, asl, q_load, rising, rel, &other,
                    )
                },
                T_CAPTURE - 2.0,
                T_CAPTURE + 2.0,
                half,
            )
        })?;
        for (&(i, k, _, _), &t_star) in rr_pts.iter().zip(&rr) {
            if let Some(t) = t_star {
                ctl.recovery.values[i][k] = T_CAPTURE - t;
                ctl.removal.values[i][k] = t - T_CAPTURE;
            }
        }
        cell.asyncs.push(ctl);
    }
    Ok(cell)
}

fn seq_fit_err() -> CharError {
    CharError::Sim("seq surrogate fit failed (too few usable coarse points)".into())
}

/// Sparse-sweep + surrogate fill for a plain D-flop: simulate a coarse grid for the
/// CK->Q arc (clock slew × Q load, log) and setup/hold (clock slew × data slew, linear —
/// they can be negative), then predict the full dense tables. The seq analogue of
/// `characterize_arc_sparse`; async controls are rejected upstream (dense-only in v1).
fn characterize_seq_sparse(job: &CharJob, cfg: &SparseCfg) -> Result<liberty::SeqCell, CharError> {
    let wiring = seq_wiring(job)?;
    let rising = !job.clock_edge.eq_ignore_ascii_case("falling");
    let (ns, nl) = (job.slews.len(), job.loads.len());
    let vdd = job.vdd;
    let ties = job.tie.as_slice();
    let fast_data = job.slews[0];
    let q_load = job.loads[0];
    let (ss, sld) = (&cfg.sim_slews, &cfg.sim_loads);

    // CK->Q coarse grid (clock slew × Q load).
    let ckq_pts: Vec<(f64, f64)> =
        ss.iter().flat_map(|&cs| sld.iter().map(move |&load| (cs, load))).collect();
    let ckq = parallel_try(&ckq_pts, job.threads, |&(cs, load)| {
        let (ckr, sr) =
            run_seq(job, &wiring, cs, load, rising, 0.0, fast_data, &[(T_CAPTURE - 1.5, vdd)], true, ties)?;
        let (ckf, sf) =
            run_seq(job, &wiring, cs, load, rising, vdd, fast_data, &[(T_CAPTURE - 1.5, 0.0)], false, ties)?;
        Ok((
            cs,
            load,
            ckr.unwrap_or(0.0) * 1e9,
            ckf.unwrap_or(0.0) * 1e9,
            sr.unwrap_or(0.0) * 1e9,
            sf.unwrap_or(0.0) * 1e9,
        ))
    })?;
    type Row6 = (f64, f64, f64, f64, f64, f64);
    let ckq_col = |f: fn(&Row6) -> f64| -> Vec<(f64, f64, f64)> {
        ckq.iter().map(|p| (p.0, p.1, f(p))).collect()
    };
    let ckq_rise = crate::sparse::fill_one(&ckq_col(|p| p.2), &job.slews, &job.loads, cfg.degree, true).ok_or_else(seq_fit_err)?;
    let ckq_fall = crate::sparse::fill_one(&ckq_col(|p| p.3), &job.slews, &job.loads, cfg.degree, true).ok_or_else(seq_fit_err)?;
    let ckq_rise_trans = crate::sparse::fill_one(&ckq_col(|p| p.4), &job.slews, &job.loads, cfg.degree, true).ok_or_else(seq_fit_err)?;
    let ckq_fall_trans = crate::sparse::fill_one(&ckq_col(|p| p.5), &job.slews, &job.loads, cfg.degree, true).ok_or_else(seq_fit_err)?;

    // setup/hold coarse grid (clock slew × data slew); values may be negative -> linear fit.
    let sh_pts: Vec<(f64, f64)> =
        ss.iter().flat_map(|&cs| ss.iter().map(move |&ds| (cs, ds))).collect();
    let sh = parallel_try(&sh_pts, job.threads, |&(cs, ds)| {
        let sr = find_constraint(|sep| Ok(run_seq(job, &wiring, cs, q_load, rising, 0.0, ds, &[(T_CAPTURE - sep, vdd)], true, ties)?.0), -ds, 3.0, 0.10)?;
        let sf = find_constraint(|sep| Ok(run_seq(job, &wiring, cs, q_load, rising, vdd, ds, &[(T_CAPTURE - sep, 0.0)], false, ties)?.0), -ds, 3.0, 0.10)?;
        let hr = find_constraint(|sep| Ok(run_seq(job, &wiring, cs, q_load, rising, 0.0, ds, &[(T_CAPTURE - 2.0, vdd), (T_CAPTURE + sep, 0.0)], true, ties)?.0), -1.8, 3.0, 0.10)?;
        let hf = find_constraint(|sep| Ok(run_seq(job, &wiring, cs, q_load, rising, vdd, ds, &[(T_CAPTURE - 2.0, 0.0), (T_CAPTURE + sep, vdd)], false, ties)?.0), -1.8, 3.0, 0.10)?;
        Ok((cs, ds, sr, sf, hr, hf))
    })?;
    let sh_col = |f: fn(&Row6) -> f64| -> Vec<(f64, f64, f64)> {
        sh.iter().map(|p| (p.0, p.1, f(p))).collect()
    };
    let setup_rise = crate::sparse::fill_one(&sh_col(|p| p.2), &job.slews, &job.slews, cfg.degree, false).ok_or_else(seq_fit_err)?;
    let setup_fall = crate::sparse::fill_one(&sh_col(|p| p.3), &job.slews, &job.slews, cfg.degree, false).ok_or_else(seq_fit_err)?;
    let hold_rise = crate::sparse::fill_one(&sh_col(|p| p.4), &job.slews, &job.slews, cfg.degree, false).ok_or_else(seq_fit_err)?;
    let hold_fall = crate::sparse::fill_one(&sh_col(|p| p.5), &job.slews, &job.slews, cfg.degree, false).ok_or_else(seq_fit_err)?;

    eprintln!(
        "sparse seq {}: CK->Q {} of {} pts, setup/hold {} of {} pts",
        job.cell,
        ckq_pts.len(),
        ns * nl,
        sh_pts.len(),
        ns * ns
    );

    Ok(liberty::SeqCell {
        cell: job.cell.clone(),
        clock_pin: job.clock_pin.clone(),
        data_pin: job.data_pin.clone(),
        out_pin: job.out_pin.clone(),
        rising_edge: rising,
        setup_rise,
        setup_fall,
        hold_rise,
        hold_fall,
        ckq_rise,
        ckq_fall,
        ckq_rise_trans,
        ckq_fall_trans,
        asyncs: Vec::new(),
    })
}

/// Run one async control->Q deck; returns `(control->Q delay, Q transition)` in seconds.
#[allow(clippy::too_many_arguments)]
fn run_async_q(
    job: &CharJob,
    wiring: &str,
    async_pin: &str,
    active_low: bool,
    sets_high: bool,
    async_slew: f64,
    q_load: f64,
    rising_clock: bool,
    ties: &[(String, bool)],
) -> Result<(Option<f64>, Option<f64>), CharError> {
    let includes: Vec<String> =
        std::iter::once(job.netlist.clone()).chain(job.models.iter().cloned()).collect();
    let d = spice::deck_async_q(
        &format!("aq {} as={async_slew}", job.cell),
        &includes,
        &job.osdi,
        wiring,
        &job.clock_pin,
        &job.data_pin,
        &job.out_pin,
        async_pin,
        job.vdd,
        job.slews[0],
        async_slew,
        q_load,
        rising_clock,
        active_low,
        sets_high,
        ties,
    );
    let m = run_deck(&d, "aq")?;
    let aq = m.0;
    let sl = m.1;
    Ok((aq, sl))
}

/// Run one async recovery/removal deck; returns the settled Q level (V), or None.
#[allow(clippy::too_many_arguments)]
fn run_async_constraint(
    job: &CharJob,
    wiring: &str,
    async_pin: &str,
    active_low: bool,
    sets_high: bool,
    clk_slew: f64,
    async_slew: f64,
    q_load: f64,
    rising_clock: bool,
    release_50: f64,
    ties: &[(String, bool)],
) -> Result<Option<f64>, CharError> {
    let includes: Vec<String> =
        std::iter::once(job.netlist.clone()).chain(job.models.iter().cloned()).collect();
    let d = spice::deck_async_constraint(
        &format!("rr {} rel={release_50}", job.cell),
        &includes,
        &job.osdi,
        wiring,
        &job.clock_pin,
        &job.data_pin,
        &job.out_pin,
        async_pin,
        job.vdd,
        clk_slew,
        async_slew,
        q_load,
        rising_clock,
        active_low,
        sets_high,
        release_50,
        ties,
    );
    Ok(run_deck(&d, "qfinal")?.0)
}

/// Write a deck, run ngspice, and pull `<key>` and `q_slew` from the measures.
fn run_deck(deck: &str, key: &str) -> Result<(Option<f64>, Option<f64>), CharError> {
    let n = DECK_SEQ.fetch_add(1, Ordering::Relaxed);
    let deck_path = std::env::temp_dir().join(format!("vyges_seq_{}_{}.sp", std::process::id(), n));
    std::fs::write(&deck_path, deck.as_bytes()).map_err(|e| CharError::Io(e.to_string()))?;
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
    let primary = m.get(key).copied().filter(|v| v.is_finite());
    let slew = m.get("q_slew").copied().filter(|&v| v.is_finite() && v > 0.0);
    Ok((primary, slew))
}

/// Render a characterized result to a `.lib` (or JSON) string for one corner's Units.
fn render_result(
    job: &CharJob,
    libname: &str,
    units: &Units,
    result: &Characterized,
    json: bool,
) -> String {
    match result {
        Characterized::Comb(arcs) => {
            if json {
                liberty::render_json(libname, &job.slews, &job.loads, arcs)
            } else {
                liberty::render(libname, units, &job.slews, &job.loads, arcs)
            }
        }
        Characterized::Seq(cell) => {
            if json {
                liberty::render_seq_json(libname, &job.slews, &job.loads, cell)
            } else {
                liberty::render_seq(libname, units, &job.slews, &job.loads, cell)
            }
        }
    }
}

/// Full run: characterize and render a `.lib` (single default corner).
pub fn run_to_lib(job: &CharJob) -> Result<String, CharError> {
    let lib = format!("{}_char", job.cell);
    Ok(render_result(job, &lib, &Units::default(), &characterize(job)?, false))
}

/// A characterized result for one PVT corner, with the corner's identity and
/// operating point — the structured form the `.lib`/JSON renderers and the dataset
/// exporter both consume. The corner name is empty for the single default-corner case.
pub struct CornerResult {
    pub name: String,
    pub vdd: f64,
    pub temp: f64,
    pub result: Characterized,
}

/// Characterize across every declared corner (or once with the top-level
/// models/vdd/temp if none), returning the structured per-corner results.
pub fn characterize_corners(job: &CharJob) -> Result<Vec<CornerResult>, CharError> {
    let corners = if job.corners.is_empty() {
        vec![crate::job::Corner {
            name: String::new(),
            models: job.models.clone(),
            vdd: job.vdd,
            temp: job.temp,
        }]
    } else {
        job.corners.clone()
    };
    let mut out = Vec::with_capacity(corners.len());
    for c in &corners {
        // a per-corner view of the job overrides the process models, supply and temp.
        let mut jc = job.clone();
        jc.models = c.models.clone();
        jc.vdd = c.vdd;
        jc.temp = c.temp;
        jc.corners = Vec::new(); // avoid recursion if anyone re-enters
        let result = characterize(&jc)?;
        out.push(CornerResult { name: c.name.clone(), vdd: c.vdd, temp: c.temp, result });
    }
    Ok(out)
}

/// Characterize across every declared corner, returning `(corner_name, rendered_lib)`
/// per corner. The corner name is empty for the single default-corner case.
pub fn run_corners(job: &CharJob, json: bool) -> Result<Vec<(String, String)>, CharError> {
    let results = characterize_corners(job)?;
    let mut out = Vec::with_capacity(results.len());
    for cr in &results {
        // a per-corner view governs the rendered Units (nominal V/temp).
        let mut jc = job.clone();
        jc.vdd = cr.vdd;
        jc.temp = cr.temp;
        let units = Units { nom_voltage: cr.vdd, nom_temp: cr.temp, ..Units::default() };
        let libname = if cr.name.is_empty() {
            format!("{}_char", job.cell)
        } else {
            format!("{}__{}", job.cell, cr.name)
        };
        out.push((cr.name.clone(), render_result(&jc, &libname, &units, &cr.result, json)));
    }
    Ok(out)
}
