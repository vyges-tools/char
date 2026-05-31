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
//! ```

use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct CharJob {
    pub cell: String,
    pub netlist: String,
    pub in_pin: String,
    pub out_pin: String,
    pub sense: String, // negative_unate | positive_unate | non_unate
    pub slews: Vec<f64>,
    pub loads: Vec<f64>,
    pub vdd: f64,
    pub temp: f64,
    pub models: Vec<String>,
    pub power: Vec<String>,  // subckt pins tied to VDD (e.g. VPWR, VPB)
    pub ground: Vec<String>, // subckt pins tied to VSS (e.g. VGND, VNB)
    pub osdi: Vec<String>,   // OSDI device-model files to pre_osdi-load (Verilog-A/OSDI PDKs)
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
        for raw in text.lines() {
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            let (k, v) = line
                .split_once(':')
                .ok_or_else(|| JobError(format!("expected 'key: value', got {line:?}")))?;
            kv.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
        let get = |k: &str| -> Result<String, JobError> {
            kv.get(k).cloned().ok_or_else(|| JobError(format!("missing key: {k}")))
        };
        let num = |k: &str| -> Result<f64, JobError> {
            get(k)?.parse::<f64>().map_err(|_| JobError(format!("{k} is not a number")))
        };
        let job = CharJob {
            cell: get("cell")?,
            netlist: get("netlist")?,
            in_pin: get("in_pin")?,
            out_pin: get("out_pin")?,
            sense: kv.get("sense").cloned().unwrap_or_else(|| "negative_unate".into()),
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
