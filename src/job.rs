//! Characterization job: the declarative description of what to characterize.
//!
//! A `.char` job is a tiny `key: value` file (std-only parser — no deps):
//!
//! ```text
//! cell:        sky130_fd_sc_hd__inv_1
//! netlist:     inv.spice
//! in_pin:      A
//! out_pin:     Y
//! sense:       negative_unate
//! slews:       0.01, 0.04, 0.16, 0.64      # ns
//! loads:       0.0005, 0.002, 0.008, 0.032 # pF
//! vdd:         1.8
//! temp:        25
//! models:      /pdk/sky130A/.../sky130.lib.spice
//! montecarlo:  100                        # LVF: MC samples for delay sigma (0/omit = NLDM only)
//! ```

use std::collections::BTreeMap;
use std::path::Path;

/// One timing arc to characterize: `in_pin -> out_pin` with a timing sense, and the
/// fixed logic state of every *other* input pin (the non-controlling "side" inputs)
/// while this arc is exercised. For a 2-input NAND `A -> Y`, B must be held at 1
/// (its non-controlling value) so Y actually responds to A.
#[derive(Debug, Clone)]
pub struct ArcSpec {
    pub in_pin: String,
    pub out_pin: String,
    pub sense: String,
    pub side: Vec<(String, bool)>, // (side-input pin, held-high?)
}

/// Parse an `arc:` line: `<in> <out> <sense> [side=0|1 ...]`,
/// e.g. `A Y negative_unate B=1`.
fn parse_arc_spec(s: &str) -> Result<ArcSpec, JobError> {
    let mut toks = s.split_whitespace();
    let in_pin = toks
        .next()
        .ok_or_else(|| JobError("arc: needs '<in> <out> <sense> [side=0|1 ...]'".into()))?
        .to_string();
    let out_pin =
        toks.next().ok_or_else(|| JobError(format!("arc {in_pin:?}: missing <out> pin")))?.to_string();
    let sense = toks.next().unwrap_or("negative_unate").to_string();
    let mut side = Vec::new();
    for t in toks {
        let (pin, lvl) = t
            .split_once('=')
            .ok_or_else(|| JobError(format!("arc side input must be pin=0|1, got {t:?}")))?;
        let high = match lvl {
            "1" | "high" | "H" | "h" => true,
            "0" | "low" | "L" | "l" => false,
            _ => return Err(JobError(format!("side level must be 0 or 1, got {t:?}"))),
        };
        side.push((pin.to_string(), high));
    }
    Ok(ArcSpec { in_pin, out_pin, sense, side })
}

#[derive(Debug, Clone)]
pub struct CharJob {
    pub cell: String,
    pub netlist: String,
    pub in_pin: String,
    pub out_pin: String,
    pub sense: String, // negative_unate | positive_unate | non_unate
    pub arcs: Vec<ArcSpec>, // one or more timing arcs (multi-input/multi-output cells)
    pub slews: Vec<f64>,
    pub loads: Vec<f64>,
    pub vdd: f64,
    pub temp: f64,
    pub models: Vec<String>,
    pub power: Vec<String>,  // subckt pins tied to VDD (e.g. VPWR, VPB)
    pub ground: Vec<String>, // subckt pins tied to VSS (e.g. VGND, VNB)
    pub osdi: Vec<String>,   // OSDI device-model files to pre_osdi-load (Verilog-A/OSDI PDKs)
    pub montecarlo: usize,   // LVF: Monte-Carlo samples for delay sigma (0 = NLDM only)
    pub ccs: bool,           // CCS: capture output-current waveforms (default false)
    pub recv: bool,          // CCS: characterize receiver capacitance on in_pin (default false)
    pub base_dir: String,
}

/// Defaults covering common open-PDK naming so the usual case needs no
/// `power:`/`ground:` keys: sky130 (`VPWR`/`VPB`, `VGND`/`VNB`) and gf180mcu
/// (`VDD`/`VNW` n-well tie, `VSS`/`VPW` p-well tie).
fn default_power() -> Vec<String> {
    ["VPWR", "VPB", "VNW", "VDD", "VCCD", "VCC"].iter().map(|s| s.to_string()).collect()
}
fn default_ground() -> Vec<String> {
    ["VGND", "VNB", "VPW", "VSS", "VSSD", "GND"].iter().map(|s| s.to_string()).collect()
}

fn names(s: &str) -> Vec<String> {
    s.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect()
}

#[derive(Debug)]
pub struct JobError(pub String);

impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "job error: {}", self.0)
    }
}
impl std::error::Error for JobError {}

fn floats(s: &str) -> Result<Vec<f64>, JobError> {
    s.split(',')
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .map(|t| t.parse::<f64>().map_err(|_| JobError(format!("not a number: {t:?}"))))
        .collect()
}

fn strip_comment(line: &str) -> &str {
    match line.find('#') {
        Some(i) => &line[..i],
        None => line,
    }
}

impl CharJob {
    pub fn parse(text: &str, base_dir: &str) -> Result<CharJob, JobError> {
        let mut kv: BTreeMap<String, String> = BTreeMap::new();
        // `arc:` may repeat (multi-arc cells); collect them outside the dedup map.
        let mut arc_lines: Vec<String> = Vec::new();
        for raw in text.lines() {
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            let (k, v) = line
                .split_once(':')
                .ok_or_else(|| JobError(format!("expected 'key: value', got {line:?}")))?;
            let key = k.trim().to_lowercase();
            if key == "arc" {
                arc_lines.push(v.trim().to_string());
            } else {
                kv.insert(key, v.trim().to_string());
            }
        }
        let get = |k: &str| -> Result<String, JobError> {
            kv.get(k).cloned().ok_or_else(|| JobError(format!("missing key: {k}")))
        };
        let num = |k: &str| -> Result<f64, JobError> {
            get(k)?.parse::<f64>().map_err(|_| JobError(format!("{k} is not a number")))
        };
        // Arcs: explicit `arc:` lines (multi-arc cells) win; otherwise synthesize a
        // single arc from in_pin/out_pin/sense (the back-compatible single-arc form).
        let arcs: Vec<ArcSpec> = if arc_lines.is_empty() {
            vec![ArcSpec {
                in_pin: get("in_pin")?,
                out_pin: get("out_pin")?,
                sense: kv.get("sense").cloned().unwrap_or_else(|| "negative_unate".into()),
                side: Vec::new(),
            }]
        } else {
            arc_lines.iter().map(|l| parse_arc_spec(l)).collect::<Result<_, _>>()?
        };
        let first = &arcs[0];
        let (in_pin, out_pin, sense) =
            (first.in_pin.clone(), first.out_pin.clone(), first.sense.clone());
        let job = CharJob {
            cell: get("cell")?,
            netlist: get("netlist")?,
            in_pin,
            out_pin,
            sense,
            arcs,
            slews: floats(&get("slews")?)?,
            loads: floats(&get("loads")?)?,
            vdd: num("vdd")?,
            temp: kv.get("temp").and_then(|t| t.parse().ok()).unwrap_or(25.0),
            models: kv
                .get("models")
                .map(|m| m.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
                .unwrap_or_default(),
            power: kv.get("power").map(|s| names(s)).unwrap_or_else(default_power),
            ground: kv.get("ground").map(|s| names(s)).unwrap_or_else(default_ground),
            osdi: kv.get("osdi").map(|s| names(s)).unwrap_or_default(),
            montecarlo: kv.get("montecarlo").and_then(|s| s.parse().ok()).unwrap_or(0),
            ccs: kv.get("ccs").map(|s| s == "true" || s == "1").unwrap_or(false),
            recv: kv.get("recv").map(|s| s == "true" || s == "1").unwrap_or(false),
            base_dir: base_dir.to_string(),
        };
        job.validate()?;
        Ok(job)
    }

    pub fn load(path: &str) -> Result<CharJob, JobError> {
        let text = std::fs::read_to_string(path).map_err(|e| JobError(format!("{path}: {e}")))?;
        let base = Path::new(path).parent().and_then(|p| p.to_str()).unwrap_or(".");
        CharJob::parse(&text, base)
    }

    pub fn validate(&self) -> Result<(), JobError> {
        if self.cell.is_empty() || self.out_pin.is_empty() || self.in_pin.is_empty() {
            return Err(JobError("cell, in_pin and out_pin are required".into()));
        }
        if self.slews.is_empty() || self.loads.is_empty() {
            return Err(JobError("slews and loads must be non-empty".into()));
        }
        if self.vdd <= 0.0 {
            return Err(JobError("vdd must be > 0".into()));
        }
        Ok(())
    }
}
