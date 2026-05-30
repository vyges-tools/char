//! Characterization engine: sweep slew x load, simulate, fill NLDM tables.
//!
//! Shells out to `ngspice` per measurement point (the file/CLI boundary). When
//! `ngspice` is absent (off the EDA host) `run` returns `NgspiceNotFound` so the
//! pure pieces (deck gen, parsing, Liberty emit) can still be exercised offline.

use std::process::Command;

use crate::job::CharJob;
use crate::liberty::{self, Arc, Table, Units};
use crate::spice;

#[derive(Debug)]
pub enum CharError {
    NgspiceNotFound,
    Sim(String),
    Io(String),
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
        }
    }
}
impl std::error::Error for CharError {}

fn ngspice_available() -> bool {
    Command::new("ngspice").arg("-v").output().is_ok()
}

/// The instance line wiring the cell under test. v0 assumes the conventional
/// `X1 <in> <out> VVDD VVSS <cellname>` order; real cells use the pin order
/// from the subckt definition (a refinement once we parse the netlist header).
fn subckt_call(job: &CharJob) -> String {
    format!("X1 {} {} VDD VSS {}", job.in_pin, job.out_pin, job.cell)
}

fn run_point(
    job: &CharJob,
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
        &subckt_call(job),
        &job.in_pin,
        &job.out_pin,
        job.vdd,
        slew,
        load,
        rising_input,
    );
    let out = Command::new("ngspice")
        .args(["-b", "-"])
        .arg("--no-spiceinit")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(d.as_bytes())?;
            child.wait_with_output()
        })
        .map_err(|e| CharError::Io(e.to_string()))?;
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
            let (dr, tr) = run_point(job, slew, load, false)?;
            arc.cell_rise.values[i][j] = dr * 1e9;
            arc.rise_transition.values[i][j] = tr * 1e9;
            let (df, tf) = run_point(job, slew, load, true)?;
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
