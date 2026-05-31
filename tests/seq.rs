// Sequential characterization: the deck_seq prime+capture clock, parameterized data
// PWL, and render_seq emission of the ff group + setup/hold constraints + CK->Q arc
// (the shape vyges-sta-si parses for reg->reg timing).
use vyges_char::job::CharJob;
use vyges_char::liberty::{render_seq, SeqCell, Table, Units};
use vyges_char::spice::deck_seq;

#[test]
fn parses_sequential_job() {
    let job = CharJob::parse(
        "cell: DFF\nnetlist: dff.spice\nclock_pin: CLK\ndata_pin: D\nout_pin: Q\n\
         slews: 0.05, 0.2\nloads: 0.005\nvdd: 1.8\n",
        ".",
    )
    .unwrap();
    assert!(job.seq);
    assert_eq!(job.clock_pin, "CLK");
    assert_eq!(job.data_pin, "D");
    assert_eq!(job.out_pin, "Q");
    assert_eq!(job.clock_edge, "rising"); // default
    assert!(job.arcs.is_empty(), "sequential job has no combinational arcs");
}

#[test]
fn seq_job_requires_clock_and_data() {
    // seq:true without clock_pin must be rejected by validate
    let bad = "cell: DFF\nnetlist: dff.spice\nseq: true\nout_pin: Q\nslews: 0.05\nloads: 0.005\nvdd: 1.8\n";
    assert!(CharJob::parse(bad, ".").is_err());
}

#[test]
fn deck_seq_has_prime_and_capture_clock() {
    // rising-edge flop, capture rising Q; one data edge (setup)
    let d = deck_seq(
        "t", &["dff.spice".into()], &[], "X1 CLK D VGND VNB VPB VPWR Q DFF",
        "CLK", "D", "Q", 1.8, 0.05, 0.005, true, 0.0, 0.05, &[(6.5, 1.8)], true,
    );
    // sources drive through small series Rs (de-stiffen the flop's storage nodes)
    assert!(d.contains("RCK cks CLK 1"), "clock through series R");
    assert!(d.contains("RD ds D 1"), "data through series R");
    assert!(d.contains("RVDD") && d.contains("RVSS"), "power through series R");
    assert!(d.contains("CL Q 0 0.005p"), "Q loaded");
    // CK->Q measured at the 2nd (capture) clock rising edge, rising Q
    assert!(d.contains("TRIG v(CLK) VAL=0.9 RISE=2"), "capture = 2nd clock edge");
    assert!(d.contains("TARG v(Q) VAL=0.9 RISE=1"));
    assert!(d.contains("ckq") && d.contains("q_slew"));
}

fn tbl(v: f64) -> Table {
    Table { values: vec![vec![v; 2], vec![v; 2]] }
}

#[test]
fn render_seq_emits_ff_constraints_and_ckq() {
    let (slews, loads) = (vec![0.05, 0.2], vec![0.005]);
    let cell = SeqCell {
        cell: "DFF".into(),
        clock_pin: "CLK".into(),
        data_pin: "D".into(),
        out_pin: "Q".into(),
        rising_edge: true,
        setup_rise: tbl(0.10),
        setup_fall: tbl(0.12),
        hold_rise: tbl(0.02),
        hold_fall: tbl(0.03),
        ckq_rise: Table { values: vec![vec![0.25], vec![0.30]] },
        ckq_fall: Table { values: vec![vec![0.24], vec![0.29]] },
        ckq_rise_trans: Table { values: vec![vec![0.06], vec![0.07]] },
        ckq_fall_trans: Table { values: vec![vec![0.05], vec![0.06]] },
    };
    let lib = render_seq("L", &Units::default(), &slews, &loads, &cell);
    // ff group + clock pin
    assert!(lib.contains("ff (IQ, IQN)"), "ff group");
    assert!(lib.contains("clocked_on : \"CLK\""));
    assert!(lib.contains("next_state : \"D\""));
    assert!(lib.contains("clock : true"), "clock pin marked");
    // setup + hold constraint groups with rise/fall constraints
    assert!(lib.contains("timing_type : setup_rising;"));
    assert!(lib.contains("timing_type : hold_rising;"));
    assert!(lib.contains("rise_constraint (vyges_constraint)"));
    assert!(lib.contains("fall_constraint (vyges_constraint)"));
    // CK->Q edge-triggered delay arc
    assert!(lib.contains("timing_type : rising_edge;"));
    assert!(lib.contains("cell_rise (vyges_nldm)"));
    // constraint template is over related/constrained transitions, not load
    assert!(lib.contains("variable_1 : related_pin_transition;"));
    assert!(lib.contains("variable_2 : constrained_pin_transition;"));
}

#[test]
fn falling_edge_flop_inverts_clocked_on() {
    let (slews, loads) = (vec![0.05, 0.2], vec![0.005]);
    let mut cell = SeqCell {
        cell: "DFFN".into(),
        clock_pin: "CLKN".into(),
        data_pin: "D".into(),
        out_pin: "Q".into(),
        rising_edge: false,
        setup_rise: tbl(0.10),
        setup_fall: tbl(0.10),
        hold_rise: tbl(0.02),
        hold_fall: tbl(0.02),
        ckq_rise: Table { values: vec![vec![0.25], vec![0.30]] },
        ckq_fall: Table { values: vec![vec![0.25], vec![0.30]] },
        ckq_rise_trans: tbl(0.06),
        ckq_fall_trans: tbl(0.06),
    };
    let lib = render_seq("L", &Units::default(), &slews, &loads, &cell);
    assert!(lib.contains("clocked_on : \"!CLKN\""), "falling edge -> inverted clock");
    assert!(lib.contains("timing_type : setup_falling;"));
    assert!(lib.contains("timing_type : falling_edge;"));
    cell.rising_edge = true; // sanity: flips back
    let lib2 = render_seq("L", &Units::default(), &slews, &loads, &cell);
    assert!(lib2.contains("clocked_on : \"CLKN\"") && !lib2.contains("!CLKN"));
}
