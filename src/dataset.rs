//! Dataset export: flatten characterization results into a tidy, long-format table
//! — one row per measured scalar — for analysis and as **training data for fast,
//! CPU-side timing/power surrogate models** (the open, fast inner-loop direction).
//!
//! Every characterized number (delay, transition, LVF sigma, internal energy, leakage,
//! receiver cap, and the sequential setup/hold/CK->Q/recovery/removal points) becomes
//! one row carrying its features — `cell`, `arc`, PVT `corner`/`vdd`/`temp`, and the two
//! grid axes (`axis1`/`index1`, `axis2`/`index2`) — plus `value` and `unit`. To recover
//! any NLDM table, filter by `metric` and pivot on `(index1, index2)`.
//!
//! Pure std (no simulator): the flatten + serialize path is unit-tested offline on
//! synthetic `Arc`/`SeqCell` data. CCS output-current *waveforms* are intentionally not
//! exported here (v0 targets the scalar grid the first surrogate trains on).

use crate::liberty::{Arc, SeqCell, Table};

// Grid-axis labels (units encoded in the name so the table is self-describing).
const IN_SLEW: &str = "input_slew_ns";
const OUT_LOAD: &str = "output_load_pf";
const CLK_SLEW: &str = "clock_slew_ns";
const DATA_SLEW: &str = "data_slew_ns";
const ASYNC_SLEW: &str = "async_slew_ns";

/// Output format for the dataset.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Format {
    /// Comma-separated, one header row + one row per record.
    Csv,
    /// JSON Lines (newline-delimited JSON objects).
    Jsonl,
}

impl Format {
    /// Parse a `--format` value; `None` for an unrecognized format.
    pub fn parse(s: &str) -> Option<Format> {
        match s.to_ascii_lowercase().as_str() {
            "csv" => Some(Format::Csv),
            "jsonl" | "ndjson" | "json" => Some(Format::Jsonl),
            _ => None,
        }
    }
}

/// One measured scalar with its features and value. The tidy "long" shape: filter by
/// `metric` and pivot on `(index1, index2)` to recover any 2-D characterization table.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub cell: String,
    pub arc: String,
    pub group: &'static str,
    pub metric: &'static str,
    pub corner: String,
    pub vdd: f64,
    pub temp: f64,
    pub axis1: &'static str,
    pub index1: String,
    pub axis2: &'static str,
    pub index2: String,
    pub value: f64,
    pub unit: &'static str,
    /// Quality flag: `"negative"` for a negative value of an inherently non-negative
    /// metric (a non-physical measurement artifact, e.g. a 50%-50% negative delay at an
    /// extreme slew/load corner); empty otherwise. Raw values are kept either way.
    pub flag: &'static str,
}

/// Classify a value: every char metric (delay, transition, cap, power, leakage, sigma) is
/// physically non-negative, so a negative value is a measurement artifact worth flagging.
fn flag_of(v: f64) -> &'static str {
    if v < 0.0 {
        "negative"
    } else {
        ""
    }
}

/// Partition rows into (physical, flagged-out). Used by `dataset --clean`.
pub fn without_flagged(rows: Vec<Row>) -> (Vec<Row>, usize) {
    let total = rows.len();
    let kept: Vec<Row> = rows.into_iter().filter(|r| r.flag.is_empty()).collect();
    let dropped = total - kept.len();
    (kept, dropped)
}

/// The per-corner context shared by every row from one characterization view.
pub struct Ctx<'a> {
    pub corner: &'a str,
    pub vdd: f64,
    pub temp: f64,
    pub slews: &'a [f64],
    pub loads: &'a [f64],
}

/// CSV / JSONL column order (also the CSV header).
pub const COLUMNS: &[&str] = &[
    "cell", "arc", "group", "metric", "corner", "vdd", "temp", "axis1", "index1", "axis2",
    "index2", "value", "unit", "flag",
];

/// Flatten combinational arcs (one `Arc` per timing arc) to rows. Always emits the four
/// NLDM tables (cell_rise/fall, rise/fall_transition); LVF sigma, internal energy, and
/// receiver-cap rows are emitted only when those were characterized. Cell-level leakage
/// is emitted once (read from the first arc that carries it).
pub fn rows_comb(ctx: &Ctx, arcs: &[Arc]) -> Vec<Row> {
    let mut rows = Vec::new();
    for a in arcs {
        let arc = format!("{}->{}", a.in_pin, a.out_pin);
        let g = TableRef {
            ctx,
            cell: &a.cell,
            arc: &arc,
        };
        g.push(
            &mut rows,
            "delay",
            "cell_rise",
            IN_SLEW,
            ctx.slews,
            OUT_LOAD,
            ctx.loads,
            &a.cell_rise,
            "ns",
        );
        g.push(
            &mut rows,
            "delay",
            "cell_fall",
            IN_SLEW,
            ctx.slews,
            OUT_LOAD,
            ctx.loads,
            &a.cell_fall,
            "ns",
        );
        g.push(
            &mut rows,
            "transition",
            "rise_transition",
            IN_SLEW,
            ctx.slews,
            OUT_LOAD,
            ctx.loads,
            &a.rise_transition,
            "ns",
        );
        g.push(
            &mut rows,
            "transition",
            "fall_transition",
            IN_SLEW,
            ctx.slews,
            OUT_LOAD,
            ctx.loads,
            &a.fall_transition,
            "ns",
        );
        if a.sigma_rise.any_nonzero() {
            g.push(
                &mut rows,
                "lvf",
                "sigma_rise",
                IN_SLEW,
                ctx.slews,
                OUT_LOAD,
                ctx.loads,
                &a.sigma_rise,
                "ns",
            );
        }
        if a.sigma_fall.any_nonzero() {
            g.push(
                &mut rows,
                "lvf",
                "sigma_fall",
                IN_SLEW,
                ctx.slews,
                OUT_LOAD,
                ctx.loads,
                &a.sigma_fall,
                "ns",
            );
        }
        if a.int_rise.any_nonzero() {
            g.push(
                &mut rows,
                "power",
                "int_rise",
                IN_SLEW,
                ctx.slews,
                OUT_LOAD,
                ctx.loads,
                &a.int_rise,
                "pJ",
            );
        }
        if a.int_fall.any_nonzero() {
            g.push(
                &mut rows,
                "power",
                "int_fall",
                IN_SLEW,
                ctx.slews,
                OUT_LOAD,
                ctx.loads,
                &a.int_fall,
                "pJ",
            );
        }
        if a.has_recv() {
            g.push(
                &mut rows,
                "recv",
                "recv_c1_rise",
                IN_SLEW,
                ctx.slews,
                OUT_LOAD,
                ctx.loads,
                &a.recv_c1_rise,
                "pF",
            );
            g.push(
                &mut rows,
                "recv",
                "recv_c2_rise",
                IN_SLEW,
                ctx.slews,
                OUT_LOAD,
                ctx.loads,
                &a.recv_c2_rise,
                "pF",
            );
            g.push(
                &mut rows,
                "recv",
                "recv_c1_fall",
                IN_SLEW,
                ctx.slews,
                OUT_LOAD,
                ctx.loads,
                &a.recv_c1_fall,
                "pF",
            );
            g.push(
                &mut rows,
                "recv",
                "recv_c2_fall",
                IN_SLEW,
                ctx.slews,
                OUT_LOAD,
                ctx.loads,
                &a.recv_c2_fall,
                "pF",
            );
        }
    }
    // Leakage is cell-level (carried on every arc); emit once from the first arc with it.
    if let Some(a) = arcs.iter().find(|a| !a.leakage.is_empty()) {
        for (when, nw) in &a.leakage {
            rows.push(Row {
                cell: a.cell.clone(),
                arc: "-".into(),
                group: "power",
                metric: "leakage",
                corner: ctx.corner.into(),
                vdd: ctx.vdd,
                temp: ctx.temp,
                axis1: "state",
                index1: when.clone(),
                axis2: "",
                index2: String::new(),
                value: *nw,
                unit: "nW",
                flag: flag_of(*nw),
            });
        }
    }
    rows
}

/// Flatten a sequential cell: CK->Q delay/transition (clock slew × load), setup/hold
/// (clock slew × data slew), and any async set/reset ->Q arcs + recovery/removal.
pub fn rows_seq(ctx: &Ctx, c: &SeqCell) -> Vec<Row> {
    let mut rows = Vec::new();
    let ckq = format!("{}->{}", c.clock_pin, c.out_pin);
    let g = TableRef {
        ctx,
        cell: &c.cell,
        arc: &ckq,
    };
    g.push(
        &mut rows,
        "delay",
        "ckq_rise",
        CLK_SLEW,
        ctx.slews,
        OUT_LOAD,
        ctx.loads,
        &c.ckq_rise,
        "ns",
    );
    g.push(
        &mut rows,
        "delay",
        "ckq_fall",
        CLK_SLEW,
        ctx.slews,
        OUT_LOAD,
        ctx.loads,
        &c.ckq_fall,
        "ns",
    );
    g.push(
        &mut rows,
        "transition",
        "ckq_rise_trans",
        CLK_SLEW,
        ctx.slews,
        OUT_LOAD,
        ctx.loads,
        &c.ckq_rise_trans,
        "ns",
    );
    g.push(
        &mut rows,
        "transition",
        "ckq_fall_trans",
        CLK_SLEW,
        ctx.slews,
        OUT_LOAD,
        ctx.loads,
        &c.ckq_fall_trans,
        "ns",
    );

    // setup/hold: clock slew (index_1) × data slew (index_2).
    let dvc = format!("{} vs {}", c.data_pin, c.clock_pin);
    let s = TableRef {
        ctx,
        cell: &c.cell,
        arc: &dvc,
    };
    s.push(
        &mut rows,
        "constraint",
        "setup_rise",
        CLK_SLEW,
        ctx.slews,
        DATA_SLEW,
        ctx.slews,
        &c.setup_rise,
        "ns",
    );
    s.push(
        &mut rows,
        "constraint",
        "setup_fall",
        CLK_SLEW,
        ctx.slews,
        DATA_SLEW,
        ctx.slews,
        &c.setup_fall,
        "ns",
    );
    s.push(
        &mut rows,
        "constraint",
        "hold_rise",
        CLK_SLEW,
        ctx.slews,
        DATA_SLEW,
        ctx.slews,
        &c.hold_rise,
        "ns",
    );
    s.push(
        &mut rows,
        "constraint",
        "hold_fall",
        CLK_SLEW,
        ctx.slews,
        DATA_SLEW,
        ctx.slews,
        &c.hold_fall,
        "ns",
    );

    for ctl in &c.asyncs {
        let aq = format!("{}->{}", ctl.pin, c.out_pin);
        let a = TableRef {
            ctx,
            cell: &c.cell,
            arc: &aq,
        };
        a.push(
            &mut rows, "delay", "async_q", ASYNC_SLEW, ctx.slews, OUT_LOAD, ctx.loads, &ctl.q, "ns",
        );
        a.push(
            &mut rows,
            "transition",
            "async_q_trans",
            ASYNC_SLEW,
            ctx.slews,
            OUT_LOAD,
            ctx.loads,
            &ctl.q_trans,
            "ns",
        );
        let avc = format!("{} vs {}", ctl.pin, c.clock_pin);
        let r = TableRef {
            ctx,
            cell: &c.cell,
            arc: &avc,
        };
        if ctl.recovery.any_nonzero() {
            r.push(
                &mut rows,
                "constraint",
                "recovery",
                CLK_SLEW,
                ctx.slews,
                ASYNC_SLEW,
                ctx.slews,
                &ctl.recovery,
                "ns",
            );
        }
        if ctl.removal.any_nonzero() {
            r.push(
                &mut rows,
                "constraint",
                "removal",
                CLK_SLEW,
                ctx.slews,
                ASYNC_SLEW,
                ctx.slews,
                &ctl.removal,
                "ns",
            );
        }
    }
    rows
}

/// Render rows in the chosen format (CSV includes the header; JSONL does not).
pub fn render(rows: &[Row], fmt: Format) -> String {
    match fmt {
        Format::Csv => to_csv(rows),
        Format::Jsonl => to_jsonl(rows),
    }
}

fn to_csv(rows: &[Row]) -> String {
    let mut s = String::new();
    s.push_str(&COLUMNS.join(","));
    s.push('\n');
    for r in rows {
        let fields = [
            r.cell.clone(),
            r.arc.clone(),
            r.group.to_string(),
            r.metric.to_string(),
            r.corner.clone(),
            fnum(r.vdd),
            fnum(r.temp),
            r.axis1.to_string(),
            r.index1.clone(),
            r.axis2.to_string(),
            r.index2.clone(),
            fnum(r.value),
            r.unit.to_string(),
            r.flag.to_string(),
        ];
        let line: Vec<String> = fields.iter().map(|f| csv_field(f)).collect();
        s.push_str(&line.join(","));
        s.push('\n');
    }
    s
}

fn to_jsonl(rows: &[Row]) -> String {
    let mut s = String::new();
    for r in rows {
        s.push_str(&format!(
            "{{\"cell\":{},\"arc\":{},\"group\":{},\"metric\":{},\"corner\":{},\"vdd\":{},\"temp\":{},\"axis1\":{},\"index1\":{},\"axis2\":{},\"index2\":{},\"value\":{},\"unit\":{},\"flag\":{}}}\n",
            jstr(&r.cell),
            jstr(&r.arc),
            jstr(r.group),
            jstr(r.metric),
            jstr(&r.corner),
            jnum(r.vdd),
            jnum(r.temp),
            jstr(r.axis1),
            jstr(&r.index1),
            jstr(r.axis2),
            jstr(&r.index2),
            jnum(r.value),
            jstr(r.unit),
            jstr(r.flag),
        ));
    }
    s
}

/// A reusable (ctx, cell, arc) handle that pushes one 2-D table as grid rows.
struct TableRef<'a> {
    ctx: &'a Ctx<'a>,
    cell: &'a str,
    arc: &'a str,
}

impl TableRef<'_> {
    #[allow(clippy::too_many_arguments)]
    fn push(
        &self,
        rows: &mut Vec<Row>,
        group: &'static str,
        metric: &'static str,
        axis1: &'static str,
        idx1: &[f64],
        axis2: &'static str,
        idx2: &[f64],
        t: &Table,
        unit: &'static str,
    ) {
        for (i, &a) in idx1.iter().enumerate() {
            if i >= t.values.len() {
                break;
            }
            for (j, &b) in idx2.iter().enumerate() {
                if j >= t.values[i].len() {
                    break;
                }
                rows.push(Row {
                    cell: self.cell.to_string(),
                    arc: self.arc.to_string(),
                    group,
                    metric,
                    corner: self.ctx.corner.to_string(),
                    vdd: self.ctx.vdd,
                    temp: self.ctx.temp,
                    axis1,
                    index1: fnum(a),
                    axis2,
                    index2: fnum(b),
                    value: t.values[i][j],
                    unit,
                    flag: flag_of(t.values[i][j]),
                });
            }
        }
    }
}

/// Format a number for CSV: shortest round-trippable form; empty for non-finite
/// (an uncharacterizable point — visible as a blank cell, never a fake 0).
fn fnum(v: f64) -> String {
    if v.is_finite() {
        format!("{v}")
    } else {
        String::new()
    }
}

/// Format a number for JSON: `null` for non-finite (NaN/inf aren't valid JSON).
fn jnum(v: f64) -> String {
    if v.is_finite() {
        format!("{v}")
    } else {
        "null".into()
    }
}

/// Quote a CSV field if it contains a comma, quote, or newline (doubling quotes).
fn csv_field(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Minimal JSON string literal (quote + escape the few chars that matter).
fn jstr(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}
