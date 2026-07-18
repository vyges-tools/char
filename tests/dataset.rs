//! Dataset export is a pure flatten over characterization structs — exercised here
//! offline on synthetic `Arc` / `SeqCell` data (no ngspice).

use vyges_char::dataset::{self, Ctx, Format};
use vyges_char::liberty::{Arc, AsyncCtl, SeqCell, Table};

const SLEWS: [f64; 2] = [0.01, 0.04];
const LOADS: [f64; 2] = [0.001, 0.004];

/// A 2x2 table whose [i][j] = base + i + 0.1*j (distinct, easy to assert).
fn tbl(base: f64) -> Table {
    let mut t = Table::new(2, 2);
    for i in 0..2 {
        for j in 0..2 {
            t.values[i][j] = base + i as f64 + 0.1 * j as f64;
        }
    }
    t
}

fn base_arc() -> Arc {
    Arc {
        cell: "INV".into(),
        in_pin: "A".into(),
        out_pin: "Y".into(),
        sense: "negative_unate".into(),
        cell_rise: tbl(1.0),
        cell_fall: tbl(2.0),
        rise_transition: tbl(3.0),
        fall_transition: tbl(4.0),
        sigma_rise: Table::new(2, 2),
        sigma_fall: Table::new(2, 2),
        ccs_rise: vec![],
        ccs_fall: vec![],
        recv_c1_rise: Table::new(2, 2),
        recv_c2_rise: Table::new(2, 2),
        recv_c1_fall: Table::new(2, 2),
        recv_c2_fall: Table::new(2, 2),
        int_rise: Table::new(2, 2),
        int_fall: Table::new(2, 2),
        leakage: vec![],
    }
}

fn ctx() -> Ctx<'static> {
    Ctx {
        corner: "tt",
        vdd: 1.8,
        temp: 25.0,
        slews: &SLEWS,
        loads: &LOADS,
    }
}

#[test]
fn comb_core_tables_only_yields_four_metrics_per_grid_point() {
    let rows = dataset::rows_comb(&ctx(), std::slice::from_ref(&base_arc()));
    // 4 core tables (cell_rise/fall, rise/fall_transition) x 2x2 grid = 16.
    assert_eq!(rows.len(), 16);
    assert!(rows
        .iter()
        .all(|r| r.cell == "INV" && r.arc == "A->Y" && r.corner == "tt"));
    // every core metric is present, nothing optional leaked in.
    for m in [
        "cell_rise",
        "cell_fall",
        "rise_transition",
        "fall_transition",
    ] {
        assert_eq!(
            rows.iter().filter(|r| r.metric == m).count(),
            4,
            "metric {m}"
        );
    }
    assert!(rows
        .iter()
        .all(|r| r.metric != "sigma_rise" && r.metric != "recv_c1_rise"));
}

#[test]
fn comb_grid_indices_and_value_line_up() {
    let rows = dataset::rows_comb(&ctx(), std::slice::from_ref(&base_arc()));
    // cell_rise at (slew=0.01 -> i=0, load=0.004 -> j=1) = 1.0 + 0 + 0.1 = 1.1.
    let r = rows
        .iter()
        .find(|r| r.metric == "cell_rise" && r.index1 == "0.01" && r.index2 == "0.004")
        .expect("cell_rise[0][1] row");
    assert_eq!(r.axis1, "input_slew_ns");
    assert_eq!(r.axis2, "output_load_pf");
    assert_eq!(r.unit, "ns");
    assert!((r.value - 1.1).abs() < 1e-12);
}

#[test]
fn optional_metrics_gate_on_characterized_data() {
    let mut a = base_arc();
    a.sigma_rise = tbl(0.01); // LVF present
    a.int_rise = tbl(0.5); // power present
    a.recv_c1_rise = tbl(0.002); // receiver cap present (any_nonzero -> has_recv)
    a.leakage = vec![("A".into(), 1.5), ("!A".into(), 3.0)];
    let rows = dataset::rows_comb(&ctx(), std::slice::from_ref(&a));
    assert_eq!(rows.iter().filter(|r| r.metric == "sigma_rise").count(), 4);
    assert_eq!(
        rows.iter()
            .filter(|r| r.metric == "int_rise" && r.unit == "pJ")
            .count(),
        4
    );
    // has_recv emits all four recv segments (rise C1 set -> the group turns on).
    assert_eq!(rows.iter().filter(|r| r.group == "recv").count(), 16);
    // leakage is cell-level: one row per state, with the when-expr as index1.
    let leak: Vec<_> = rows.iter().filter(|r| r.metric == "leakage").collect();
    assert_eq!(leak.len(), 2);
    assert!(leak
        .iter()
        .any(|r| r.index1 == "!A" && (r.value - 3.0).abs() < 1e-12 && r.unit == "nW"));
}

#[test]
fn csv_has_header_and_row_count() {
    let rows = dataset::rows_comb(&ctx(), std::slice::from_ref(&base_arc()));
    let csv = dataset::render(&rows, Format::Csv);
    let mut lines = csv.lines();
    assert_eq!(lines.next().unwrap(), dataset::COLUMNS.join(","));
    assert_eq!(lines.count(), 16); // one data line per row, no trailing blank counted
}

#[test]
fn jsonl_one_object_per_row() {
    let rows = dataset::rows_comb(&ctx(), std::slice::from_ref(&base_arc()));
    let jsonl = dataset::render(&rows, Format::Jsonl);
    let lines: Vec<&str> = jsonl.lines().collect();
    assert_eq!(lines.len(), 16);
    assert!(lines.iter().all(|l| l.starts_with('{') && l.ends_with('}')));
    assert!(lines
        .iter()
        .any(|l| l.contains("\"metric\":\"cell_rise\"") && l.contains("\"unit\":\"ns\"")));
}

#[test]
fn non_finite_value_is_blank_in_csv_null_in_json() {
    let mut a = base_arc();
    a.cell_rise.values[0][0] = f64::NAN; // an uncharacterizable point
    let rows = dataset::rows_comb(&ctx(), std::slice::from_ref(&a));
    let csv = dataset::render(&rows, Format::Csv);
    // the NaN cell_rise row has an EMPTY value field and an empty (non-negative) flag:
    // `<index2>,,ns,` — i.e. ",,ns," before the newline.
    assert!(
        csv.contains(",,ns,\n"),
        "expected blank CSV value for non-finite"
    );
    let json = dataset::render(&rows, Format::Jsonl);
    assert!(
        json.contains("\"value\":null"),
        "expected null JSON value for non-finite"
    );
}

#[test]
fn negative_values_are_flagged_and_cleanable() {
    let mut a = base_arc();
    a.cell_fall.values[1][1] = -0.02; // a non-physical negative delay (artifact)
    let rows = dataset::rows_comb(&ctx(), std::slice::from_ref(&a));
    let neg: Vec<_> = rows.iter().filter(|r| r.flag == "negative").collect();
    assert_eq!(neg.len(), 1);
    assert_eq!(neg[0].metric, "cell_fall");
    assert!(neg[0].value < 0.0);
    // every other row is unflagged (the positive values).
    assert!(rows.iter().filter(|r| r.flag.is_empty()).count() == rows.len() - 1);
    // --clean drops exactly the flagged row, raw count otherwise preserved.
    let n = rows.len();
    let (kept, dropped) = dataset::without_flagged(rows);
    assert_eq!(dropped, 1);
    assert_eq!(kept.len(), n - 1);
    assert!(kept.iter().all(|r| r.flag.is_empty()));
}

#[test]
fn seq_cell_emits_ckq_and_constraints() {
    let cell = SeqCell {
        cell: "DFF".into(),
        clock_pin: "CLK".into(),
        data_pin: "D".into(),
        out_pin: "Q".into(),
        rising_edge: true,
        setup_rise: tbl(0.05),
        setup_fall: tbl(0.06),
        hold_rise: tbl(0.02),
        hold_fall: tbl(0.03),
        ckq_rise: tbl(0.2),
        ckq_fall: tbl(0.25),
        ckq_rise_trans: tbl(0.04),
        ckq_fall_trans: tbl(0.05),
        asyncs: vec![AsyncCtl {
            pin: "RST_B".into(),
            expr: "!RST_B".into(),
            sets_high: false,
            active_low: true,
            q: tbl(0.3),
            q_trans: tbl(0.06),
            recovery: tbl(0.01),
            removal: tbl(0.015),
        }],
    };
    let rows = dataset::rows_seq(&ctx(), &cell);
    assert!(rows
        .iter()
        .any(|r| r.metric == "ckq_rise" && r.arc == "CLK->Q"));
    assert!(rows
        .iter()
        .any(|r| r.metric == "setup_rise" && r.arc == "D vs CLK" && r.axis2 == "data_slew_ns"));
    assert!(rows
        .iter()
        .any(|r| r.metric == "async_q" && r.arc == "RST_B->Q"));
    assert!(rows
        .iter()
        .any(|r| r.metric == "recovery" && r.arc == "RST_B vs CLK"));
}

#[test]
fn format_parse() {
    assert_eq!(Format::parse("csv"), Some(Format::Csv));
    assert_eq!(Format::parse("CSV"), Some(Format::Csv));
    assert_eq!(Format::parse("jsonl"), Some(Format::Jsonl));
    assert_eq!(Format::parse("ndjson"), Some(Format::Jsonl));
    assert_eq!(Format::parse("yaml"), None);
}
