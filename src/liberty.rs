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
/// delay-sigma tables (Monte-Carlo over mismatch), optional CCS output-current
/// waveforms, and optional CCS receiver-capacitance tables (input-pin model).
/// Empty sigma -> no LVF; empty ccs -> no CCS; zero recv tables -> no receiver model.
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
    // CCS receiver capacitance on `in_pin` (pF): the two-segment input-pin load a
    // driver sees. C1 = effective cap over the first half of the input transition
    // (before the delay threshold, mostly static gate cap); C2 = over the second
    // half (after the threshold, inflated by Miller from the switching output).
    pub recv_c1_rise: Table,
    pub recv_c2_rise: Table,
    pub recv_c1_fall: Table,
    pub recv_c2_fall: Table,
}

impl Arc {
    /// True if any receiver-capacitance segment was characterized.
    pub fn has_recv(&self) -> bool {
        self.recv_c1_rise.any_nonzero()
            || self.recv_c2_rise.any_nonzero()
            || self.recv_c1_fall.any_nonzero()
            || self.recv_c2_fall.any_nonzero()
    }

    /// Conventional single-number input `capacitance` (pF): the mean static
    /// (pre-switching) segment over the grid, i.e. the C1 lanes — what a NLDM-only
    /// tool reads. The receiver_capacitance group carries the fuller C1/C2 split.
    pub fn nominal_cap(&self) -> f64 {
        let mean = |t: &Table| {
            let (mut sum, mut n) = (0.0, 0usize);
            for row in &t.values {
                for &v in row {
                    sum += v;
                    n += 1;
                }
            }
            if n == 0 { 0.0 } else { sum / n as f64 }
        };
        (mean(&self.recv_c1_rise) + mean(&self.recv_c1_fall)) / 2.0
    }
}

/// A characterized sequential (flip-flop) cell: setup/hold constraints on the data
/// pin vs the clock, plus the CK->Q delay arc. Setup/hold tables are indexed by
/// (clock transition, data transition); the CK->Q arc by (clock transition, load).
#[derive(Debug, Clone)]
pub struct SeqCell {
    pub cell: String,
    pub clock_pin: String,
    pub data_pin: String,
    pub out_pin: String,
    pub rising_edge: bool, // clocked on the rising (true) or falling edge
    pub setup_rise: Table, // setup for a rising data edge
    pub setup_fall: Table,
    pub hold_rise: Table,
    pub hold_fall: Table,
    pub ckq_rise: Table,       // CK->Q delay, rising Q
    pub ckq_fall: Table,       // CK->Q delay, falling Q
    pub ckq_rise_trans: Table, // Q rise transition
    pub ckq_fall_trans: Table, // Q fall transition
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

    // Group arcs into cells (first-seen order). Each cell emits every input pin
    // once (with its receiver model) and every output pin once, the latter carrying
    // a `timing ()` group per arc that targets it — so a multi-input gate (A->Y,
    // B->Y) or a multi-output cell (A->{Y,Z}) renders as one well-formed cell.
    let mut cell_order: Vec<&str> = Vec::new();
    for a in arcs {
        if !cell_order.iter().any(|c| *c == a.cell) {
            cell_order.push(&a.cell);
        }
    }
    for cell in cell_order {
        let cell_arcs: Vec<&Arc> = arcs.iter().filter(|a| a.cell == cell).collect();
        s.push_str(&format!("  cell ({cell}) {{\n"));
        // input pins, unique by name, first-seen
        let mut seen_in: Vec<&str> = Vec::new();
        for a in &cell_arcs {
            if !seen_in.iter().any(|p| *p == a.in_pin) {
                seen_in.push(&a.in_pin);
                emit_input_pin(&mut s, a, tmpl, slews, loads);
            }
        }
        // output pins, unique by name, first-seen; each gathers its timing arcs
        let mut seen_out: Vec<&str> = Vec::new();
        for a in &cell_arcs {
            if seen_out.iter().any(|p| *p == a.out_pin) {
                continue;
            }
            seen_out.push(&a.out_pin);
            s.push_str(&format!("    pin ({}) {{\n      direction : output;\n", a.out_pin));
            for b in cell_arcs.iter().filter(|b| b.out_pin == a.out_pin) {
                emit_timing(&mut s, b, tmpl, slews, loads);
            }
            s.push_str("    }\n");
        }
        s.push_str("  }\n");
    }
    s.push_str("}\n");
    s
}

/// Emit one input `pin` group: bare `direction : input` unless receiver caps were
/// characterized, in which case add the conventional `capacitance` + CCS receiver model.
fn emit_input_pin(s: &mut String, arc: &Arc, tmpl: &str, slews: &[f64], loads: &[f64]) {
    if arc.has_recv() {
        s.push_str(&format!("    pin ({}) {{\n      direction : input;\n", arc.in_pin));
        s.push_str(&format!("      capacitance : {:.6};\n", arc.nominal_cap()));
        s.push_str("      receiver_capacitance () {\n");
        for (name, t) in [
            ("receiver_capacitance1_rise", &arc.recv_c1_rise),
            ("receiver_capacitance1_fall", &arc.recv_c1_fall),
            ("receiver_capacitance2_rise", &arc.recv_c2_rise),
            ("receiver_capacitance2_fall", &arc.recv_c2_fall),
        ] {
            s.push_str(&format!("        {name} ({tmpl}) {{\n"));
            s.push_str(&format!("          index_1 (\"{}\");\n", fmt_index(slews)));
            s.push_str(&format!("          index_2 (\"{}\");\n", fmt_index(loads)));
            s.push_str("          values ( \\\n");
            s.push_str(&fmt_table(t, "        "));
            s.push_str(" );\n        }\n");
        }
        s.push_str("      }\n    }\n");
    } else {
        s.push_str(&format!("    pin ({}) {{\n      direction : input;\n    }}\n", arc.in_pin));
    }
}

/// Emit one `timing ()` arc group (NLDM tables + optional LVF sigma + CCS current).
fn emit_timing(s: &mut String, arc: &Arc, tmpl: &str, slews: &[f64], loads: &[f64]) {
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
    s.push_str("      }\n");
}

/// Render a single-library `.lib` for a characterized sequential cell: the `ff`
/// group, the clock pin, the data pin's setup/hold constraint timing groups, and
/// the Q pin's CK->Q delay arc. The constraint tables use a template over
/// (related_pin / clock transition, constrained_pin / data transition); the CK->Q
/// arc uses the usual (input_net_transition, total_output_net_capacitance) template.
pub fn render_seq(
    library: &str,
    units: &Units,
    slews: &[f64],
    loads: &[f64],
    cell: &SeqCell,
) -> String {
    let nldm = "vyges_nldm";
    let cons = "vyges_constraint";
    let mut s = String::new();
    s.push_str(&format!("library ({library}) {{\n"));
    s.push_str("  delay_model : table_lookup;\n");
    s.push_str(&format!("  time_unit : \"{}\";\n", units.time));
    s.push_str(&format!(
        "  capacitive_load_unit (1, \"{}\");\n",
        units.cap.trim_end_matches(|c: char| c.is_alphabetic())
    ));
    s.push_str(&format!("  voltage_unit : \"{}\";\n", units.voltage));
    s.push_str("  nom_process : 1.0;\n  nom_temperature : 25.0;\n  nom_voltage : 1.8;\n\n");

    // CK->Q delay template (slew x load) + constraint template (clk x data slew).
    s.push_str(&format!("  lu_table_template ({nldm}) {{\n"));
    s.push_str("    variable_1 : input_net_transition;\n");
    s.push_str("    variable_2 : total_output_net_capacitance;\n");
    s.push_str(&format!("    index_1 (\"{}\");\n", fmt_index(slews)));
    s.push_str(&format!("    index_2 (\"{}\");\n", fmt_index(loads)));
    s.push_str("  }\n\n");
    s.push_str(&format!("  lu_table_template ({cons}) {{\n"));
    s.push_str("    variable_1 : related_pin_transition;\n");
    s.push_str("    variable_2 : constrained_pin_transition;\n");
    s.push_str(&format!("    index_1 (\"{}\");\n", fmt_index(slews)));
    s.push_str(&format!("    index_2 (\"{}\");\n", fmt_index(slews)));
    s.push_str("  }\n\n");

    let named = |s: &mut String, name: &str, tmpl: &str, i1: &[f64], i2: &[f64], t: &Table| {
        s.push_str(&format!("        {name} ({tmpl}) {{\n"));
        s.push_str(&format!("          index_1 (\"{}\");\n", fmt_index(i1)));
        s.push_str(&format!("          index_2 (\"{}\");\n", fmt_index(i2)));
        s.push_str("          values ( \\\n");
        s.push_str(&fmt_table(t, "        "));
        s.push_str(" );\n        }\n");
    };

    s.push_str(&format!("  cell ({}) {{\n", cell.cell));
    let clocked = if cell.rising_edge {
        cell.clock_pin.clone()
    } else {
        format!("!{}", cell.clock_pin)
    };
    s.push_str("    ff (IQ, IQN) {\n");
    s.push_str(&format!("      next_state : \"{}\";\n", cell.data_pin));
    s.push_str(&format!("      clocked_on : \"{clocked}\";\n"));
    s.push_str("    }\n");

    // clock pin
    s.push_str(&format!("    pin ({}) {{\n      direction : input;\n      clock : true;\n    }}\n", cell.clock_pin));

    // data pin: setup + hold constraint groups (rise/fall constraints)
    s.push_str(&format!("    pin ({}) {{\n      direction : input;\n", cell.data_pin));
    let edge = if cell.rising_edge { "rising" } else { "falling" };
    for (ty, rise, fall) in [
        (format!("setup_{edge}"), &cell.setup_rise, &cell.setup_fall),
        (format!("hold_{edge}"), &cell.hold_rise, &cell.hold_fall),
    ] {
        s.push_str("      timing () {\n");
        s.push_str(&format!("        related_pin : \"{}\";\n", cell.clock_pin));
        s.push_str(&format!("        timing_type : {ty};\n"));
        named(&mut s, "rise_constraint", cons, slews, slews, rise);
        named(&mut s, "fall_constraint", cons, slews, slews, fall);
        s.push_str("      }\n");
    }
    s.push_str("    }\n");

    // Q pin: CK->Q delay arc (edge-triggered)
    s.push_str(&format!("    pin ({}) {{\n      direction : output;\n", cell.out_pin));
    s.push_str("      timing () {\n");
    s.push_str(&format!("        related_pin : \"{}\";\n", cell.clock_pin));
    s.push_str(&format!("        timing_type : {edge}_edge;\n"));
    named(&mut s, "cell_rise", nldm, slews, loads, &cell.ckq_rise);
    named(&mut s, "cell_fall", nldm, slews, loads, &cell.ckq_fall);
    named(&mut s, "rise_transition", nldm, slews, loads, &cell.ckq_rise_trans);
    named(&mut s, "fall_transition", nldm, slews, loads, &cell.ckq_fall_trans);
    s.push_str("      }\n    }\n");

    s.push_str("  }\n}\n");
    s
}

/// Machine-readable summary of a characterized sequential cell (std-only, no deps).
pub fn render_seq_json(library: &str, slews: &[f64], loads: &[f64], cell: &SeqCell) -> String {
    let arr = |v: &[f64]| v.iter().map(|x| format!("{x:.6}")).collect::<Vec<_>>().join(",");
    let table = |t: &Table| {
        t.values.iter().map(|row| format!("[{}]", arr(row))).collect::<Vec<_>>().join(",")
    };
    let mut s = String::new();
    s.push_str(&format!("{{\"library\":{library:?},\"cell\":{:?},", cell.cell));
    s.push_str(&format!(
        "\"clock_pin\":{:?},\"data_pin\":{:?},\"out_pin\":{:?},\"rising_edge\":{},",
        cell.clock_pin, cell.data_pin, cell.out_pin, cell.rising_edge
    ));
    s.push_str(&format!("\"slews\":[{}],\"loads\":[{}],", arr(slews), arr(loads)));
    s.push_str(&format!("\"setup_rise\":[{}],", table(&cell.setup_rise)));
    s.push_str(&format!("\"setup_fall\":[{}],", table(&cell.setup_fall)));
    s.push_str(&format!("\"hold_rise\":[{}],", table(&cell.hold_rise)));
    s.push_str(&format!("\"hold_fall\":[{}],", table(&cell.hold_fall)));
    s.push_str(&format!("\"ckq_rise\":[{}],", table(&cell.ckq_rise)));
    s.push_str(&format!("\"ckq_fall\":[{}]}}\n", table(&cell.ckq_fall)));
    s
}
