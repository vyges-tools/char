//! Liberty (`.lib`) NLDM emitter.
//!
//! Emits a `table_lookup` (NLDM) timing model: a `lu_table_template` over
//! (input_net_transition, total_output_net_capacitance) plus per-arc
//! `cell_rise` / `cell_fall` / `rise_transition` / `fall_transition` tables.
//! Pure std — no simulator needed — so it is fully unit-tested offline.

/// A 2-D NLDM table: values[i][j] indexed by (slew_i, load_j).
#[derive(Debug, Clone)]
pub struct Table {
    pub values: Vec<Vec<f64>>,
}

impl Table {
    pub fn new(rows: usize, cols: usize) -> Table {
        Table { values: vec![vec![0.0; cols]; rows] }
    }
    /// True if any entry is non-zero — gates whether a sigma table is emitted.
    pub fn any_nonzero(&self) -> bool {
        self.values.iter().any(|r| r.iter().any(|&v| v != 0.0))
    }
}

/// A CCS output-current waveform at one (slew, load) grid point: the driver's
/// output current I(t) over time, plus the reference (input-crossing) time.
#[derive(Debug, Clone)]
pub struct Waveform {
    pub slew: f64,
    pub load: f64,
    pub ref_time: f64,
    pub time: Vec<f64>,    // ns
    pub current: Vec<f64>, // mA
}

/// One timing arc (in_pin -> out_pin) with its four NLDM tables, optional LVF
/// delay-sigma tables (Monte-Carlo over mismatch), and optional CCS output-current
/// waveforms. Empty sigma -> no LVF; empty ccs -> no CCS.
#[derive(Debug, Clone)]
pub struct Arc {
    pub cell: String,
    pub in_pin: String,
    pub out_pin: String,
    pub sense: String,
    pub cell_rise: Table,
    pub cell_fall: Table,
    pub rise_transition: Table,
    pub fall_transition: Table,
    pub sigma_rise: Table, // LVF: 1-sigma of cell_rise delay (ns)
    pub sigma_fall: Table, // LVF: 1-sigma of cell_fall delay (ns)
    pub ccs_rise: Vec<Waveform>, // CCS output_current_rise, one per (slew,load)
    pub ccs_fall: Vec<Waveform>, // CCS output_current_fall
}

#[derive(Debug, Clone)]
pub struct Units {
    pub time: String,        // e.g. "1ns"
    pub cap: String,         // e.g. "1pf"
    pub voltage: String,     // e.g. "1V"
}

impl Default for Units {
    fn default() -> Self {
        Units { time: "1ns".into(), cap: "1pf".into(), voltage: "1V".into() }
    }
}

fn fmt_index(vals: &[f64]) -> String {
    vals.iter().map(|v| format!("{v}")).collect::<Vec<_>>().join(", ")
}

fn fmt_csv(vals: &[f64]) -> String {
    vals.iter().map(|v| format!("{v:.6}")).collect::<Vec<_>>().join(", ")
}

fn fmt_table(t: &Table, indent: &str) -> String {
    let rows: Vec<String> = t
        .values
        .iter()
        .map(|row| {
            let cells = row.iter().map(|v| format!("{v:.6}")).collect::<Vec<_>>().join(", ");
            format!("{indent}    \"{cells}\"")
        })
        .collect();
    rows.join(", \\\n")
}

/// Machine-readable characterization summary (std-only, no deps).
pub fn render_json(library: &str, slews: &[f64], loads: &[f64], arcs: &[Arc]) -> String {
    let arr = |v: &[f64]| {
        v.iter().map(|x| format!("{x:.6}")).collect::<Vec<_>>().join(",")
    };
    let table = |t: &Table| {
        t.values
            .iter()
            .map(|row| format!("[{}]", arr(row)))
            .collect::<Vec<_>>()
            .join(",")
    };
    let mut s = String::new();
    s.push_str(&format!("{{\"library\":{library:?},"));
    s.push_str(&format!("\"slews\":[{}],\"loads\":[{}],", arr(slews), arr(loads)));
    s.push_str("\"arcs\":[");
    for (i, a) in arcs.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!(
            "{{\"cell\":{:?},\"in_pin\":{:?},\"out_pin\":{:?},\"sense\":{:?},",
            a.cell, a.in_pin, a.out_pin, a.sense
        ));
        s.push_str(&format!("\"cell_rise\":[{}],", table(&a.cell_rise)));
        s.push_str(&format!("\"cell_fall\":[{}],", table(&a.cell_fall)));
        s.push_str(&format!("\"rise_transition\":[{}],", table(&a.rise_transition)));
        s.push_str(&format!("\"fall_transition\":[{}]}}", table(&a.fall_transition)));
    }
    s.push_str("]}\n");
    s
}

/// Render a complete single-library `.lib` for the given arcs.
pub fn render(
    library: &str,
    units: &Units,
    slews: &[f64],
    loads: &[f64],
    arcs: &[Arc],
) -> String {
    let tmpl = "vyges_nldm";
    let mut s = String::new();
    s.push_str(&format!("library ({library}) {{\n"));
    s.push_str("  delay_model : table_lookup;\n");
    s.push_str(&format!("  time_unit : \"{}\";\n", units.time));
    s.push_str(&format!("  capacitive_load_unit (1, \"{}\");\n", units.cap.trim_end_matches(|c: char| c.is_alphabetic())));
    s.push_str(&format!("  voltage_unit : \"{}\";\n", units.voltage));
    s.push_str("  nom_process : 1.0;\n  nom_temperature : 25.0;\n  nom_voltage : 1.8;\n\n");

    // Lookup-table template shared by all arcs.
    s.push_str(&format!("  lu_table_template ({tmpl}) {{\n"));
    s.push_str("    variable_1 : input_net_transition;\n");
    s.push_str("    variable_2 : total_output_net_capacitance;\n");
    s.push_str(&format!("    index_1 (\"{}\");\n", fmt_index(slews)));
    s.push_str(&format!("    index_2 (\"{}\");\n", fmt_index(loads)));
    s.push_str("  }\n\n");

    // CCS time-vector template (declared only if any arc carries current waveforms).
    if arcs.iter().any(|a| !a.ccs_rise.is_empty() || !a.ccs_fall.is_empty()) {
        s.push_str("  lu_table_template (ccs_tmpl) {\n");
        s.push_str("    variable_1 : input_net_transition;\n");
        s.push_str("    variable_2 : total_output_net_capacitance;\n");
        s.push_str("    variable_3 : time;\n");
        s.push_str("  }\n\n");
    }

    for arc in arcs {
        s.push_str(&format!("  cell ({}) {{\n", arc.cell));
        s.push_str(&format!("    pin ({}) {{\n      direction : input;\n    }}\n", arc.in_pin));
        s.push_str(&format!("    pin ({}) {{\n      direction : output;\n", arc.out_pin));
        s.push_str("      timing () {\n");
        s.push_str(&format!("        related_pin : \"{}\";\n", arc.in_pin));
        s.push_str(&format!("        timing_sense : {};\n", arc.sense));
        for (name, t) in [
            ("cell_rise", &arc.cell_rise),
            ("cell_fall", &arc.cell_fall),
            ("rise_transition", &arc.rise_transition),
            ("fall_transition", &arc.fall_transition),
        ] {
            s.push_str(&format!("        {name} ({tmpl}) {{\n"));
            s.push_str(&format!("          index_1 (\"{}\");\n", fmt_index(slews)));
            s.push_str(&format!("          index_2 (\"{}\");\n", fmt_index(loads)));
            s.push_str("          values ( \\\n");
            s.push_str(&fmt_table(t, "        "));
            s.push_str(" );\n        }\n");
        }
        // LVF: per-(slew,load) delay sigma tables, emitted only when characterized.
        if arc.sigma_rise.any_nonzero() || arc.sigma_fall.any_nonzero() {
            for (name, t) in
                [("ocv_sigma_cell_rise", &arc.sigma_rise), ("ocv_sigma_cell_fall", &arc.sigma_fall)]
            {
                s.push_str(&format!("        {name} ({tmpl}) {{\n"));
                s.push_str("          sigma_type : \"early_and_late\";\n");
                s.push_str(&format!("          index_1 (\"{}\");\n", fmt_index(slews)));
                s.push_str(&format!("          index_2 (\"{}\");\n", fmt_index(loads)));
                s.push_str("          values ( \\\n");
                s.push_str(&fmt_table(t, "        "));
                s.push_str(" );\n        }\n");
            }
        }
        // CCS: output-current waveforms (one `vector` per (slew,load) grid point).
        for (group, wfs) in
            [("output_current_rise", &arc.ccs_rise), ("output_current_fall", &arc.ccs_fall)]
        {
            if wfs.is_empty() {
                continue;
            }
            s.push_str(&format!("        {group} () {{\n"));
            for w in wfs {
                s.push_str("          vector (ccs_tmpl) {\n");
                s.push_str(&format!("            reference_time : {:.6};\n", w.ref_time));
                s.push_str(&format!("            index_1 (\"{:.6}\");\n", w.slew));
                s.push_str(&format!("            index_2 (\"{:.6}\");\n", w.load));
                s.push_str(&format!("            index_3 (\"{}\");\n", fmt_csv(&w.time)));
                s.push_str(&format!("            values (\"{}\");\n", fmt_csv(&w.current)));
                s.push_str("          }\n");
            }
            s.push_str("        }\n");
        }
        s.push_str("      }\n    }\n  }\n");
    }
    s.push_str("}\n");
    s
}
