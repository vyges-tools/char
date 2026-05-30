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
use crate::liberty::{self, Arc, Table, Units};
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

/// The instance line wiring the cell under test, in the cell's real port order.
///
/// Reads the `.subckt` port list from the netlist and maps each pin to a deck
/// node: the input/output pins keep their net names (`in_pin`/`out_pin`, which
/// the deck's source and load drive), power pins tie to `VDD`, ground pins tie
/// to `VSS`. A port that is none of these is a hard error — the caller must
/// declare it under `power:`/`ground:` rather than have it silently floated.
fn subckt_call(job: &CharJob) -> Result<String, CharError> {
    let netlist = std::fs::read_to_string(&job.netlist)
        .map_err(|e| CharError::Netlist(format!("{}: {e}", job.netlist)))?;
    let pins = spice::parse_subckt_pins(&netlist, &job.cell).ok_or_else(|| {
        CharError::Netlist(format!("no `.subckt {}` found in {}", job.cell, job.netlist))
    })?;
    let mut nodes = Vec::with_capacity(pins.len());
    for pin in &pins {
        let node = if pin.eq_ignore_ascii_case(&job.in_pin) {
            job.in_pin.clone()
        } else if pin.eq_ignore_ascii_case(&job.out_pin) {
            job.out_pin.clone()
        } else if job.power.iter().any(|p| p.eq_ignore_ascii_case(pin)) {
            "VDD".to_string()
        } else if job.ground.iter().any(|p| p.eq_ignore_ascii_case(pin)) {
            "VSS".to_string()
        } else {
            return Err(CharError::Netlist(format!(
                "subckt pin {pin:?} of {} is neither in/out nor a known power/ground pin; \
                 add it under power:/ground:",
                job.cell
            )));
        };
        nodes.push(node);
    }
    Ok(format!("X1 {} {}", nodes.join(" "), job.cell))
}

fn run_point(
    job: &CharJob,
    subckt_call: &str,
    slew: f64,
    load: f64,
    rising_input: bool,
) -> Result<(f64, f64), CharError> {
    let includes: Vec<String> = std::iter::once(job.netlist.clone())
        .chain(job.models.iter().cloned())
        .collect();
    let d = spice::deck(
        &format!("char {} slew={slew} load={load}", job.cell),
        &includes,
        subckt_call,
        &job.in_pin,
        &job.out_pin,
        job.vdd,
        slew,
        load,
        rising_input,
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

/// Characterize one arc into NLDM tables (delays/transitions in ns).
pub fn characterize(job: &CharJob) -> Result<Arc, CharError> {
    if !ngspice_available() {
        return Err(CharError::NgspiceNotFound);
    }
    let instance = subckt_call(job)?;
    let (ns, nl) = (job.slews.len(), job.loads.len());
    let mut arc = Arc {
        cell: job.cell.clone(),
        in_pin: job.in_pin.clone(),
        out_pin: job.out_pin.clone(),
        sense: job.sense.clone(),
        cell_rise: Table::new(ns, nl),
        cell_fall: Table::new(ns, nl),
        rise_transition: Table::new(ns, nl),
        fall_transition: Table::new(ns, nl),
    };
    for (i, &slew) in job.slews.iter().enumerate() {
        for (j, &load) in job.loads.iter().enumerate() {
            // falling input -> rising output (cell_rise), and vice versa.
            let (dr, tr) = run_point(job, &instance, slew, load, false)?;
            arc.cell_rise.values[i][j] = dr * 1e9;
            arc.rise_transition.values[i][j] = tr * 1e9;
            let (df, tf) = run_point(job, &instance, slew, load, true)?;
            arc.cell_fall.values[i][j] = df * 1e9;
            arc.fall_transition.values[i][j] = tf * 1e9;
        }
    }
    Ok(arc)
}

/// Full run: characterize and render a `.lib`.
pub fn run_to_lib(job: &CharJob) -> Result<String, CharError> {
    let arc = characterize(job)?;
    Ok(liberty::render(
        &format!("{}_char", job.cell),
        &Units::default(),
        &job.slews,
        &job.loads,
        &[arc],
    ))
}
