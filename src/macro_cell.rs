//! Liberty for a hard macro (an SRAM, a register file, any block with a clocked interface), from
//! its measured interface timing.
//!
//! A standard cell is characterized here end to end (SPICE in, Liberty out). A macro is not: its
//! interface numbers come from simulations of the whole block (the access time, the shortest
//! period that still works) and of the cells at its pins (the capture flops' setup and hold with
//! the block's own clock tree, the output stage's delay over load). Those measurements arrive as
//! a JSON file (schema `vyges-char-macro/1`, below); this module validates them and writes the
//! Liberty view, so every macro gets the same, checked Liberty however it was measured.
//!
//! What it writes, and why, as the timer reads it (OpenSTA's Liberty reader at the programme pin,
//! `LibertyReader.cc`; the Rust reader in `vyges-loom` is a transcription of it and the tests here
//! read the output with it):
//! - **Each pin group names its own clock edge.** An SRAM captures its inputs on CLK's rising edge
//!   and, self-timed, drives Q from the falling edge (OpenRAM's Liberty does the same:
//!   `setup_rising` / `hold_rising` on the inputs, `falling_edge` on `dout`). A flop's single
//!   edge (`SeqCell::rising_edge`) cannot say that.
//! - **Checks on the clock pin as attributes:** `min_period` (read as the port's minimum period)
//!   and `min_pulse_width_high` / `_low` (read as width checks, and turned into check arcs on the
//!   pin itself). OpenRAM writes the same numbers as `minimum_period` / `min_pulse_width` timing
//!   groups; the reader takes either.
//! - **`is_macro_cell : true`** and a **`memory` group** when the block is a memory: the reader
//!   marks the cell a macro and a memory (power reports put it under macros). OpenRAM writes the
//!   `memory` group but not `is_macro_cell`.
//! - **No `min_clock_tree_path` / `max_clock_tree_path` arcs.** With an ideal clock the timer
//!   subtracts a macro's clock-tree delay from CLK->Q and adds it to its checks
//!   (`Search.cc`, `PathEnd.cc`, `clkTreeDelay`). The setup, hold and CLK->Q numbers this module
//!   takes are measured **at the macro's CLK pin**, internal clock buffer included, so declaring
//!   the tree as well would count it twice.
//! - Constraint tables are indexed (index_1 = clock transition, index_2 = data transition) — the
//!   template's `related_pin_transition`, `constrained_pin_transition`, as OpenRAM's
//!   `CONSTRAINT_TABLE` is. Output tables are over load only (one row) or clock transition × load.
//!
//! Units are fixed: ns, pF, V, °C. Slew thresholds are required (no default): a table measured
//! 10–90 % and declared 20–80 % is wrong by a quarter without any error to show it.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use vyges_loom::json::Value;

pub const SCHEMA: &str = "vyges-char-macro/1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Rising,
    Falling,
}

impl Edge {
    fn word(self) -> &'static str {
        match self {
            Edge::Rising => "rising",
            Edge::Falling => "falling",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Memory {
    pub kind: String, // ram | rom
    pub address_width: u32,
    pub word_width: u32,
}

#[derive(Debug, Clone)]
pub struct PgPin {
    pub name: String,
    pub pg_type: String, // primary_power | primary_ground
}

#[derive(Debug, Clone)]
pub struct ClockPin {
    pub pin: String,
    pub capacitance: f64,
    pub min_period: f64,
    pub min_pulse_width_high: f64,
    pub min_pulse_width_low: f64,
}

/// Inputs captured by the clock: setup and hold, rise and fall of the data, each a table over
/// (clock transition, data transition).
#[derive(Debug, Clone)]
pub struct InputGroup {
    pub pins: Vec<String>,
    pub capacitance: f64,
    pub edge: Edge,
    pub setup_rise: Vec<Vec<f64>>,
    pub setup_fall: Vec<Vec<f64>>,
    pub hold_rise: Vec<Vec<f64>>,
    pub hold_fall: Vec<Vec<f64>>,
}

/// Outputs launched by the clock: delay and transition, each a table over load (one row) or over
/// (clock transition, load).
#[derive(Debug, Clone)]
pub struct OutputGroup {
    pub pins: Vec<String>,
    pub edge: Edge,
    pub max_capacitance: f64,
    pub cell_rise: Vec<Vec<f64>>,
    pub cell_fall: Vec<Vec<f64>>,
    pub rise_transition: Vec<Vec<f64>>,
    pub fall_transition: Vec<Vec<f64>>,
}

#[derive(Debug, Clone)]
pub struct MacroCell {
    pub library: String,
    pub cell: String,
    pub comment: String,
    pub area: f64,
    pub nom_voltage: f64,
    pub nom_temperature: f64,
    pub nom_process: f64,
    pub slew_thresholds_pct: (f64, f64),
    pub memory: Option<Memory>,
    pub pg_pins: Vec<PgPin>,
    pub slews: Vec<f64>,
    pub loads: Vec<f64>,
    pub clock: ClockPin,
    pub inputs: Vec<InputGroup>,
    pub outputs: Vec<OutputGroup>,
}

fn obj<'a>(v: &'a Value, what: &str) -> Result<&'a Value, String> {
    match v {
        Value::Obj(_) => Ok(v),
        _ => Err(format!("{what}: not an object")),
    }
}

fn field<'a>(o: &'a Value, k: &str, what: &str) -> Result<&'a Value, String> {
    o.get(k).ok_or_else(|| format!("{what}: missing `{k}`"))
}

fn num(v: &Value, what: &str) -> Result<f64, String> {
    match v {
        Value::Num(n) if n.is_finite() => Ok(*n),
        _ => Err(format!("{what}: not a finite number")),
    }
}

fn num_at(o: &Value, k: &str, what: &str) -> Result<f64, String> {
    num(field(o, k, what)?, &format!("{what}.{k}"))
}

fn text(o: &Value, k: &str, what: &str) -> Result<String, String> {
    field(o, k, what)?
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("{what}.{k}: not a string"))
}

fn vec1(v: &Value, what: &str) -> Result<Vec<f64>, String> {
    let a = v.as_array().ok_or_else(|| format!("{what}: not an array"))?;
    a.iter().enumerate().map(|(i, x)| num(x, &format!("{what}[{i}]"))).collect()
}

fn vec2(v: &Value, what: &str) -> Result<Vec<Vec<f64>>, String> {
    let a = v.as_array().ok_or_else(|| format!("{what}: not an array of rows"))?;
    a.iter().enumerate().map(|(i, r)| vec1(r, &format!("{what}[{i}]"))).collect()
}

fn names(v: &Value, what: &str) -> Result<Vec<String>, String> {
    let a = v.as_array().ok_or_else(|| format!("{what}: not an array"))?;
    let out: Vec<String> = a
        .iter()
        .map(|x| x.as_str().map(str::to_string).ok_or_else(|| format!("{what}: a pin name is not a string")))
        .collect::<Result<_, _>>()?;
    if out.is_empty() {
        return Err(format!("{what}: no pins"));
    }
    Ok(out)
}

fn edge(o: &Value, what: &str) -> Result<Edge, String> {
    match text(o, "clock_edge", what)?.as_str() {
        "rising" => Ok(Edge::Rising),
        "falling" => Ok(Edge::Falling),
        e => Err(format!("{what}.clock_edge: `{e}` (rising | falling)")),
    }
}

fn increasing(v: &[f64], what: &str) -> Result<(), String> {
    if v.is_empty() {
        return Err(format!("{what}: empty"));
    }
    if v.iter().any(|x| *x < 0.0) || v.windows(2).any(|w| w[1] <= w[0]) {
        return Err(format!("{what}: must be non-negative and strictly increasing"));
    }
    Ok(())
}

fn shape(t: &[Vec<f64>], rows: &[usize], cols: usize, what: &str) -> Result<(), String> {
    if !rows.contains(&t.len()) || t.iter().any(|r| r.len() != cols) {
        let want = rows.iter().map(|r| format!("{r}×{cols}")).collect::<Vec<_>>().join(" or ");
        let got = format!("{}×{}", t.len(), t.first().map_or(0, |r| r.len()));
        return Err(format!("{what}: table is {got}, want {want}"));
    }
    Ok(())
}

impl MacroCell {
    /// Parse and validate a `vyges-char-macro/1` document.
    pub fn from_json(text_in: &str) -> Result<MacroCell, String> {
        let v = vyges_loom::json::parse(text_in).map_err(|e| format!("JSON: {e}"))?;
        let o = obj(&v, "document")?;
        let schema = text(o, "schema", "document")?;
        if schema != SCHEMA {
            return Err(format!("schema `{schema}`, this version reads `{SCHEMA}`"));
        }
        let slews = vec1(field(o, "slews", "document")?, "slews")?;
        let loads = vec1(field(o, "loads", "document")?, "loads")?;
        increasing(&slews, "slews")?;
        increasing(&loads, "loads")?;
        let th = vec1(field(o, "slew_thresholds_pct", "document")?, "slew_thresholds_pct")?;
        if th.len() != 2 || !(0.0 < th[0] && th[0] < th[1] && th[1] < 100.0) {
            return Err("slew_thresholds_pct: want [lower, upper], 0 < lower < upper < 100".into());
        }
        let memory = match o.get("memory") {
            None => None,
            Some(m) => {
                let mo = obj(m, "memory")?;
                let kind = text(mo, "type", "memory")?;
                if kind != "ram" && kind != "rom" {
                    return Err(format!("memory.type: `{kind}` (ram | rom)"));
                }
                let w = |k: &str| -> Result<u32, String> {
                    let x = num_at(mo, k, "memory")?;
                    if x < 1.0 || x.fract() != 0.0 {
                        return Err(format!("memory.{k}: a positive integer"));
                    }
                    Ok(x as u32)
                };
                Some(Memory { kind, address_width: w("address_width")?, word_width: w("word_width")? })
            }
        };
        let mut pg_pins = Vec::new();
        if let Some(p) = o.get("pg_pins") {
            for (i, g) in p.as_array().ok_or("pg_pins: not an array")?.iter().enumerate() {
                let go = obj(g, &format!("pg_pins[{i}]"))?;
                let pg_type = text(go, "pg_type", &format!("pg_pins[{i}]"))?;
                if pg_type != "primary_power" && pg_type != "primary_ground" {
                    return Err(format!("pg_pins[{i}].pg_type: `{pg_type}` (primary_power | primary_ground)"));
                }
                pg_pins.push(PgPin { name: text(go, "name", &format!("pg_pins[{i}]"))?, pg_type });
            }
        }
        let co = obj(field(o, "clock", "document")?, "clock")?;
        let clock = ClockPin {
            pin: text(co, "pin", "clock")?,
            capacitance: num_at(co, "capacitance", "clock")?,
            min_period: num_at(co, "min_period", "clock")?,
            min_pulse_width_high: num_at(co, "min_pulse_width_high", "clock")?,
            min_pulse_width_low: num_at(co, "min_pulse_width_low", "clock")?,
        };
        if clock.min_period <= 0.0 || clock.capacitance < 0.0 {
            return Err("clock: min_period must be positive, capacitance non-negative".into());
        }
        for (k, w) in [("high", clock.min_pulse_width_high), ("low", clock.min_pulse_width_low)] {
            if w <= 0.0 || w > clock.min_period {
                return Err(format!("clock.min_pulse_width_{k}: must be in (0, min_period]"));
            }
        }
        if clock.min_pulse_width_high + clock.min_pulse_width_low > clock.min_period + 1e-9 {
            return Err("clock: the two pulse widths add up to more than min_period".into());
        }
        let n = slews.len();
        let mut inputs = Vec::new();
        for (i, g) in field(o, "inputs", "document")?.as_array().ok_or("inputs: not an array")?.iter().enumerate() {
            let w = format!("inputs[{i}]");
            let go = obj(g, &w)?;
            let t = |k: &str| -> Result<Vec<Vec<f64>>, String> {
                let t = vec2(field(go, k, &w)?, &format!("{w}.{k}"))?;
                shape(&t, &[n], n, &format!("{w}.{k}"))?;
                Ok(t)
            };
            let cap = num_at(go, "capacitance", &w)?;
            if cap < 0.0 {
                return Err(format!("{w}.capacitance: negative"));
            }
            inputs.push(InputGroup {
                pins: names(field(go, "pins", &w)?, &format!("{w}.pins"))?,
                capacitance: cap,
                edge: edge(go, &w)?,
                setup_rise: t("setup_rise")?,
                setup_fall: t("setup_fall")?,
                hold_rise: t("hold_rise")?,
                hold_fall: t("hold_fall")?,
            });
        }
        let mut outputs = Vec::new();
        for (i, g) in field(o, "outputs", "document")?.as_array().ok_or("outputs: not an array")?.iter().enumerate() {
            let w = format!("outputs[{i}]");
            let go = obj(g, &w)?;
            let t = |k: &str| -> Result<Vec<Vec<f64>>, String> {
                let t = vec2(field(go, k, &w)?, &format!("{w}.{k}"))?;
                shape(&t, &[1, n], loads.len(), &format!("{w}.{k}"))?;
                Ok(t)
            };
            let g = OutputGroup {
                pins: names(field(go, "pins", &w)?, &format!("{w}.pins"))?,
                edge: edge(go, &w)?,
                max_capacitance: num_at(go, "max_capacitance", &w)?,
                cell_rise: t("cell_rise")?,
                cell_fall: t("cell_fall")?,
                rise_transition: t("rise_transition")?,
                fall_transition: t("fall_transition")?,
            };
            let rows = g.cell_rise.len();
            if [&g.cell_fall, &g.rise_transition, &g.fall_transition].iter().any(|x| x.len() != rows) {
                return Err(format!("{w}: delay and transition tables must all be over load, or all over clock transition × load"));
            }
            if g.max_capacitance <= 0.0 || g.max_capacitance > loads[loads.len() - 1] + 1e-12 {
                return Err(format!("{w}.max_capacitance: must be positive and within the characterized loads"));
            }
            if [&g.cell_rise, &g.cell_fall].iter().any(|x| x.iter().flatten().any(|v| *v <= 0.0)) {
                return Err(format!("{w}: a non-positive delay (an output launched before its clock edge)"));
            }
            if [&g.rise_transition, &g.fall_transition].iter().any(|x| x.iter().flatten().any(|v| *v <= 0.0)) {
                return Err(format!("{w}: a non-positive transition"));
            }
            // A launch that lasts a whole period means the period was measured without this
            // output (OpenRAM's delay check rejects a read that outlasts the half period it has
            // from the falling edge, for the same reason).
            if [&g.cell_rise, &g.cell_fall].iter().any(|x| x.iter().flatten().any(|v| *v >= clock.min_period)) {
                return Err(format!("{w}: a clock-to-output delay of min_period or more: min_period cannot hold"));
            }
            outputs.push(g);
        }
        if inputs.is_empty() && outputs.is_empty() {
            return Err("no inputs and no outputs: nothing to time".into());
        }
        // every signal pin once, and none of them the clock or a supply
        let mut seen = BTreeSet::new();
        seen.insert(clock.pin.clone());
        for p in &pg_pins {
            if !seen.insert(p.name.clone()) {
                return Err(format!("pin `{}` declared twice", p.name));
            }
        }
        for p in inputs.iter().flat_map(|g| &g.pins).chain(outputs.iter().flat_map(|g| &g.pins)) {
            if !seen.insert(p.clone()) {
                return Err(format!("pin `{p}` declared twice (or is the clock or a supply)"));
            }
        }
        let area = num_at(o, "area", "document")?;
        if area <= 0.0 {
            return Err("area: must be positive".into());
        }
        Ok(MacroCell {
            library: text(o, "library", "document")?,
            cell: text(o, "cell", "document")?,
            comment: o.get("comment").and_then(|c| c.as_str()).unwrap_or("").to_string(),
            area,
            nom_voltage: num_at(o, "nom_voltage", "document")?,
            nom_temperature: num_at(o, "nom_temperature", "document")?,
            nom_process: o.get("nom_process").map(|p| num(p, "nom_process")).transpose()?.unwrap_or(1.0),
            slew_thresholds_pct: (th[0], th[1]),
            memory,
            pg_pins,
            slews,
            loads,
            clock,
            inputs,
            outputs,
        })
    }
}

fn list(v: &[f64]) -> String {
    v.iter().map(|x| format!("{x}")).collect::<Vec<_>>().join(", ")
}

fn values(t: &[Vec<f64>], indent: &str) -> String {
    let rows: Vec<String> = t
        .iter()
        .map(|r| format!("\"{}\"", r.iter().map(|v| format!("{v:.6}")).collect::<Vec<_>>().join(", ")))
        .collect();
    format!("values ( \\\n{indent}  {} );", rows.join(format!(", \\\n{indent}  ").as_str()))
}

const T_CONS: &str = "vyges_macro_constraint";
const T_DELAY: &str = "vyges_macro_delay";
const T_LOAD: &str = "vyges_macro_load";

/// Render the Liberty view.
pub fn render(m: &MacroCell) -> String {
    let mut s = String::new();
    let (lo, hi) = m.slew_thresholds_pct;
    if !m.comment.is_empty() {
        let _ = writeln!(s, "/* {} */", m.comment.replace("*/", "* /"));
    }
    let _ = writeln!(s, "library ({}) {{", m.library);
    for l in [
        "technology (cmos);".to_string(),
        "delay_model : table_lookup;".into(),
        "time_unit : \"1ns\";".into(),
        "voltage_unit : \"1V\";".into(),
        "capacitive_load_unit (1, pf);".into(),
        "input_threshold_pct_rise : 50;".into(),
        "input_threshold_pct_fall : 50;".into(),
        "output_threshold_pct_rise : 50;".into(),
        "output_threshold_pct_fall : 50;".into(),
        format!("slew_lower_threshold_pct_rise : {lo};"),
        format!("slew_upper_threshold_pct_rise : {hi};"),
        format!("slew_lower_threshold_pct_fall : {lo};"),
        format!("slew_upper_threshold_pct_fall : {hi};"),
        "slew_derate_from_library : 1;".into(),
        format!("nom_process : {};", m.nom_process),
        format!("nom_voltage : {};", m.nom_voltage),
        format!("nom_temperature : {};", m.nom_temperature),
    ] {
        let _ = writeln!(s, "  {l}");
    }
    for p in &m.pg_pins {
        let v = if p.pg_type == "primary_power" { m.nom_voltage } else { 0.0 };
        let _ = writeln!(s, "  voltage_map ({}, {v});", p.name);
    }
    let _ = writeln!(
        s,
        "  lu_table_template ({T_CONS}) {{\n    variable_1 : related_pin_transition;\n    variable_2 : constrained_pin_transition;\n    index_1 (\"{0}\");\n    index_2 (\"{0}\");\n  }}",
        list(&m.slews)
    );
    let _ = writeln!(
        s,
        "  lu_table_template ({T_DELAY}) {{\n    variable_1 : input_net_transition;\n    variable_2 : total_output_net_capacitance;\n    index_1 (\"{}\");\n    index_2 (\"{}\");\n  }}",
        list(&m.slews),
        list(&m.loads)
    );
    let _ = writeln!(
        s,
        "  lu_table_template ({T_LOAD}) {{\n    variable_1 : total_output_net_capacitance;\n    index_1 (\"{}\");\n  }}",
        list(&m.loads)
    );
    let _ = writeln!(s, "  cell ({}) {{", m.cell);
    let _ = writeln!(s, "    area : {:.4};", m.area);
    for l in ["is_macro_cell : true;", "interface_timing : true;", "dont_touch : true;", "dont_use : true;"] {
        let _ = writeln!(s, "    {l}");
    }
    if let Some(mem) = &m.memory {
        let _ = writeln!(
            s,
            "    memory () {{\n      type : {};\n      address_width : {};\n      word_width : {};\n    }}",
            mem.kind, mem.address_width, mem.word_width
        );
    }
    for p in &m.pg_pins {
        let _ = writeln!(s, "    pg_pin ({0}) {{\n      voltage_name : {0};\n      pg_type : {1};\n    }}", p.name, p.pg_type);
    }
    // a pin's supplies, when the macro has exactly one of each
    let one = |t: &str| {
        let v: Vec<&PgPin> = m.pg_pins.iter().filter(|p| p.pg_type == t).collect();
        (v.len() == 1).then(|| v[0].name.clone())
    };
    let related = match (one("primary_power"), one("primary_ground")) {
        (Some(p), Some(g)) => format!("      related_power_pin : {p};\n      related_ground_pin : {g};\n"),
        _ => String::new(),
    };
    let c = &m.clock;
    let _ = write!(
        s,
        "    pin ({}) {{\n      direction : input;\n      clock : true;\n      capacitance : {:.6};\n{related}      min_period : {:.4};\n      min_pulse_width_high : {:.4};\n      min_pulse_width_low : {:.4};\n    }}\n",
        c.pin, c.capacitance, c.min_period, c.min_pulse_width_high, c.min_pulse_width_low
    );
    let ind = "          ";
    for g in &m.inputs {
        for p in &g.pins {
            let _ = write!(s, "    pin ({p}) {{\n      direction : input;\n      capacitance : {:.6};\n{related}", g.capacitance);
            for (kind, rise, fall) in [("setup", &g.setup_rise, &g.setup_fall), ("hold", &g.hold_rise, &g.hold_fall)] {
                let _ = write!(
                    s,
                    "      timing () {{\n        related_pin : \"{}\";\n        timing_type : {kind}_{};\n        rise_constraint ({T_CONS}) {{\n{ind}{}\n        }}\n        fall_constraint ({T_CONS}) {{\n{ind}{}\n        }}\n      }}\n",
                    c.pin,
                    g.edge.word(),
                    values(rise, ind),
                    values(fall, ind)
                );
            }
            s.push_str("    }\n");
        }
    }
    for g in &m.outputs {
        let tmpl = if g.cell_rise.len() == 1 { T_LOAD } else { T_DELAY };
        for p in &g.pins {
            let _ = write!(
                s,
                "    pin ({p}) {{\n      direction : output;\n      max_capacitance : {};\n{related}      timing () {{\n        related_pin : \"{}\";\n        timing_type : {}_edge;\n        timing_sense : non_unate;\n",
                g.max_capacitance,
                c.pin,
                g.edge.word()
            );
            for (k, t) in [
                ("cell_rise", &g.cell_rise),
                ("rise_transition", &g.rise_transition),
                ("cell_fall", &g.cell_fall),
                ("fall_transition", &g.fall_transition),
            ] {
                let t1: Vec<Vec<f64>>;
                let t = if tmpl == T_LOAD {
                    t1 = vec![t[0].clone()];
                    &t1
                } else {
                    t
                };
                let _ = writeln!(s, "        {k} ({tmpl}) {{\n{ind}{}\n        }}", values(t, ind));
            }
            s.push_str("      }\n    }\n");
        }
    }
    s.push_str("  }\n}\n");
    s
}
