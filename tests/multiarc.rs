// Multi-arc cells: `arc:` lines (with held side inputs) parse into a CharJob, and
// the renderer groups arcs of one cell into a single well-formed `cell {}` — one
// input pin per name, one output pin gathering all its timing() arcs.
use vyges_char::job::CharJob;
use vyges_char::liberty::{render, Arc, Table, Units};

const NAND2_JOB: &str = "\
cell:    NAND2
netlist: nand2.spice
slews:   0.01, 0.04
loads:   0.001, 0.004
vdd:     1.8
arc:     A Y negative_unate B=1
arc:     B Y negative_unate A=1
";

#[test]
fn parses_multiple_arcs_with_side_inputs() {
    let job = CharJob::parse(NAND2_JOB, ".").unwrap();
    assert_eq!(job.arcs.len(), 2);
    // back-compat fields mirror the first arc
    assert_eq!(job.in_pin, "A");
    assert_eq!(job.out_pin, "Y");
    let a = &job.arcs[0];
    assert_eq!(
        (a.in_pin.as_str(), a.out_pin.as_str(), a.sense.as_str()),
        ("A", "Y", "negative_unate")
    );
    assert_eq!(a.side, vec![("B".to_string(), true)]); // B held high (non-controlling for NAND)
    let b = &job.arcs[1];
    assert_eq!(b.in_pin, "B");
    assert_eq!(b.side, vec![("A".to_string(), true)]);
}

#[test]
fn single_arc_job_is_back_compatible() {
    let job = CharJob::parse(
        "cell: INV\nnetlist: inv.spice\nin_pin: A\nout_pin: Y\nslews: 0.01\nloads: 0.001\nvdd: 1.8\n",
        ".",
    )
    .unwrap();
    assert_eq!(job.arcs.len(), 1);
    assert_eq!(job.arcs[0].in_pin, "A");
    assert!(job.arcs[0].side.is_empty());
}

#[test]
fn bad_side_level_is_rejected() {
    let bad = "cell: NAND2\nnetlist: n.spice\nslews: 0.01\nloads: 0.001\nvdd: 1.8\narc: A Y negative_unate B=x\n";
    assert!(CharJob::parse(bad, ".").is_err());
}

fn arc(in_pin: &str, out_pin: &str) -> Arc {
    Arc {
        cell: "NAND2".into(),
        in_pin: in_pin.into(),
        out_pin: out_pin.into(),
        sense: "negative_unate".into(),
        cell_rise: Table {
            values: vec![vec![0.10, 0.20], vec![0.30, 0.40]],
        },
        cell_fall: Table {
            values: vec![vec![0.11, 0.21], vec![0.31, 0.41]],
        },
        rise_transition: Table::new(2, 2),
        fall_transition: Table::new(2, 2),
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

#[test]
fn render_groups_arcs_into_one_cell() {
    let (slews, loads) = (vec![0.01, 0.04], vec![0.001, 0.004]);
    // NAND2: A->Y and B->Y -> ONE cell, two input pins, one output pin, two timings.
    let lib = render(
        "L",
        &Units::default(),
        &slews,
        &loads,
        &[arc("A", "Y"), arc("B", "Y")],
    );
    assert_eq!(
        lib.matches("cell (NAND2)").count(),
        1,
        "arcs of one cell merge into one cell"
    );
    assert_eq!(lib.matches("pin (A)").count(), 1);
    assert_eq!(lib.matches("pin (B)").count(), 1);
    assert_eq!(lib.matches("pin (Y)").count(), 1, "single output pin");
    assert_eq!(
        lib.matches("timing ()").count(),
        2,
        "one timing arc per input"
    );
    assert_eq!(lib.matches("related_pin : \"A\"").count(), 1);
    assert_eq!(lib.matches("related_pin : \"B\"").count(), 1);
}

#[test]
fn render_handles_multi_output_cell() {
    let (slews, loads) = (vec![0.01, 0.04], vec![0.001, 0.004]);
    // A drives two outputs Y and Z -> one input pin, two output pins.
    let lib = render(
        "L",
        &Units::default(),
        &slews,
        &loads,
        &[arc("A", "Y"), arc("A", "Z")],
    );
    assert_eq!(lib.matches("pin (A)").count(), 1, "input pin emitted once");
    assert_eq!(lib.matches("pin (Y)").count(), 1);
    assert_eq!(lib.matches("pin (Z)").count(), 1);
    assert_eq!(lib.matches("timing ()").count(), 2);
}
