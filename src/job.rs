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

/// A PVT corner to characterize: a named (process models, supply, temperature)
/// operating point. A job with `corner:` lines is swept across all of them, one
/// `.lib` per corner — the per-corner library set sta-si's MCMM consumes.
#[derive(Debug, Clone)]
pub struct Corner {
    pub name: String,
    pub models: Vec<String>,
    pub vdd: f64,
    pub temp: f64,
}

/// Parse a `corner:` line: `name | models(csv) | vdd [| temp]`,
/// e.g. `ss_n40C_1v60 | params_ss.spice, corners/ss.spice | 1.60 | -40`.
fn parse_corner(s: &str, default_temp: f64) -> Result<Corner, JobError> {
    let parts: Vec<&str> = s.split('|').map(|p| p.trim()).collect();
    if parts.len() < 3 {
        return Err(JobError(format!(
            "corner needs 'name | models | vdd [| temp]', got {s:?}"
        )));
    }
    let models: Vec<String> =
        parts[1].split(',').map(|m| m.trim().to_string()).filter(|m| !m.is_empty()).collect();
    let vdd =
        parts[2].parse::<f64>().map_err(|_| JobError(format!("corner vdd not a number: {:?}", parts[2])))?;
    let temp = match parts.get(3) {
        Some(t) => t.parse::<f64>().map_err(|_| JobError(format!("corner temp not a number: {t:?}")))?,
        None => default_temp,
    };
    Ok(Corner { name: parts[0].to_string(), models, vdd, temp })
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
    // Sequential (flip-flop) constraint characterization. When `seq` is set, the
    // job characterizes setup/hold on `data_pin` vs `clock_pin` plus the CK->Q delay
    // arc on `out_pin`, instead of combinational arcs.
    pub seq: bool,
    pub clock_pin: String,
    pub data_pin: String,
    pub clock_edge: String,  // "rising" | "falling"
    // Async set/reset flops: `tie` holds the named pins at a fixed level (their
    // inactive state) during setup/hold/CK->Q characterization; `reset_pin` (if set)
    // additionally emits the `ff` clear attribute and characterizes the async
    // reset->Q delay arc. `reset_active_low` is inferred from the pin name (_B/_N).
    pub tie: Vec<(String, bool)>, // (pin, held-high?)
    pub reset_pin: String,
    pub reset_active_low: bool,
    pub corners: Vec<Corner>, // PVT corners to sweep (empty = single run from models/vdd/temp)
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

/// Parse a `pin=0|1, pin=0|1` held-level list (for `tie:`).
fn tie_levels(s: &str) -> Result<Vec<(String, bool)>, JobError> {
    s.split(',')
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .map(|t| {
            let (pin, lvl) = t
                .split_once('=')
                .ok_or_else(|| JobError(format!("tie entry must be pin=0|1, got {t:?}")))?;
            let high = match lvl.trim() {
                "1" | "high" | "H" | "h" => true,
                "0" | "low" | "L" | "l" => false,
                _ => return Err(JobError(format!("tie level must be 0 or 1, got {t:?}"))),
            };
            Ok((pin.trim().to_string(), high))
        })
        .collect()
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
        // `arc:`/`corner:` may repeat; collect them outside the dedup map.
        let mut arc_lines: Vec<String> = Vec::new();
        let mut corner_lines: Vec<String> = Vec::new();
        for raw in text.lines() {
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            let (k, v) = line
                .split_once(':')
                .ok_or_else(|| JobError(format!("expected 'key: value', got {line:?}")))?;
            let key = k.trim().to_lowercase();
            match key.as_str() {
                "arc" => arc_lines.push(v.trim().to_string()),
                "corner" => corner_lines.push(v.trim().to_string()),
                _ => {
                    kv.insert(key, v.trim().to_string());
                }
            }
        }
        let get = |k: &str| -> Result<String, JobError> {
            kv.get(k).cloned().ok_or_else(|| JobError(format!("missing key: {k}")))
        };
        let num = |k: &str| -> Result<f64, JobError> {
            get(k)?.parse::<f64>().map_err(|_| JobError(format!("{k} is not a number")))
        };
        let is_seq = kv.get("seq").map(|s| s == "true" || s == "1").unwrap_or(false)
            || kv.contains_key("clock_pin");
        // Arcs: explicit `arc:` lines (multi-arc cells) win; otherwise synthesize a
        // single arc from in_pin/out_pin/sense (the back-compatible single-arc form).
        // Sequential jobs don't use combinational arcs (data_pin/clock_pin drive the
        // setup/hold + CK->Q characterization instead), so the arc list stays empty.
        let arcs: Vec<ArcSpec> = if is_seq {
            Vec::new()
        } else if arc_lines.is_empty() {
            vec![ArcSpec {
                in_pin: get("in_pin")?,
                out_pin: get("out_pin")?,
                sense: kv.get("sense").cloned().unwrap_or_else(|| "negative_unate".into()),
                side: Vec::new(),
            }]
        } else {
            arc_lines.iter().map(|l| parse_arc_spec(l)).collect::<Result<_, _>>()?
        };
        let (in_pin, out_pin, sense) = match arcs.first() {
            Some(a) => (a.in_pin.clone(), a.out_pin.clone(), a.sense.clone()),
            None => (kv.get("data_pin").cloned().unwrap_or_default(), get("out_pin")?, String::new()),
        };
        let default_temp = kv.get("temp").and_then(|t| t.parse().ok()).unwrap_or(25.0);
        let corners: Vec<Corner> =
            corner_lines.iter().map(|l| parse_corner(l, default_temp)).collect::<Result<_, _>>()?;
        // Top-level models/vdd default the single-run case; with corners present they
        // fall back to the first corner so the struct is always well-formed.
        let models: Vec<String> = kv
            .get("models")
            .map(|m| m.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
            .or_else(|| corners.first().map(|c| c.models.clone()))
            .unwrap_or_default();
        let vdd = match kv.get("vdd") {
            Some(_) => num("vdd")?,
            None => corners.first().map(|c| c.vdd).ok_or_else(|| JobError("missing key: vdd".into()))?,
        };
        let job = CharJob {
            cell: get("cell")?,
            netlist: get("netlist")?,
            in_pin,
            out_pin,
            sense,
            arcs,
            slews: floats(&get("slews")?)?,
            loads: floats(&get("loads")?)?,
            vdd,
            temp: default_temp,
            models,
            power: kv.get("power").map(|s| names(s)).unwrap_or_else(default_power),
            ground: kv.get("ground").map(|s| names(s)).unwrap_or_else(default_ground),
            osdi: kv.get("osdi").map(|s| names(s)).unwrap_or_default(),
            montecarlo: kv.get("montecarlo").and_then(|s| s.parse().ok()).unwrap_or(0),
            ccs: kv.get("ccs").map(|s| s == "true" || s == "1").unwrap_or(false),
            recv: kv.get("recv").map(|s| s == "true" || s == "1").unwrap_or(false),
            seq: kv.get("seq").map(|s| s == "true" || s == "1").unwrap_or(false)
                || kv.contains_key("clock_pin"),
            clock_pin: kv.get("clock_pin").cloned().unwrap_or_default(),
            data_pin: kv.get("data_pin").cloned().unwrap_or_default(),
            clock_edge: kv.get("clock_edge").cloned().unwrap_or_else(|| "rising".into()),
            tie: kv.get("tie").map(|s| tie_levels(s)).transpose()?.unwrap_or_default(),
            reset_pin: kv.get("reset_pin").cloned().unwrap_or_default(),
            reset_active_low: {
                let rp = kv.get("reset_pin").cloned().unwrap_or_default();
                match kv.get("reset_active").map(|s| s.as_str()) {
                    Some("low") | Some("0") => true,
                    Some("high") | Some("1") => false,
                    // infer from the pin name: active-low if it ends in _B / _N.
                    _ => rp.ends_with("_B") || rp.ends_with("_N"),
                }
            },
            corners,
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
        if self.seq {
            if self.cell.is_empty()
                || self.clock_pin.is_empty()
                || self.data_pin.is_empty()
                || self.out_pin.is_empty()
            {
                return Err(JobError(
                    "sequential job needs cell, clock_pin, data_pin and out_pin".into(),
                ));
            }
        } else if self.cell.is_empty() || self.out_pin.is_empty() || self.in_pin.is_empty() {
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
