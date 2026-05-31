// Async set/reset flops: tie:/reset_pin: parse, the reset->Q delay deck, and
// render_seq emission of the ff `clear` attribute + the async reset->Q timing arc.
use vyges_char::job::CharJob;
use vyges_char::liberty::{render_seq, SeqCell, Table, Units};
use vyges_char::spice::deck_reset_q;

#[test]
fn parses_tie_and_reset_pin() {
    let job = CharJob::parse(
        "cell: DFRTP\nnetlist: dff.spice\nclock_pin: CLK\ndata_pin: D\nout_pin: Q\n\
         reset_pin: RESET_B\nslews: 0.05\nloads: 0.005\nvdd: 1.8\n",
        ".",
    )
    .unwrap();
    assert!(job.seq);
    assert_eq!(job.reset_pin, "RESET_B");
    assert!(job.reset_active_low, "RESET_B name -> active-low inferred");
}

#[test]
fn tie_levels_parse_and_reject_bad() {
    let job = CharJob::parse(
        "cell: SDFF\nnetlist: dff.spice\nclock_pin: CLK\ndata_pin: D\nout_pin: Q\n\
         tie: SCE=0, SCD=1\nslews: 0.05\nloads: 0.005\nvdd: 1.8\n",
        ".",
    )
    .unwrap();
    assert_eq!(job.tie, vec![("SCE".into(), false), ("SCD".into(), true)]);
    let bad = "cell: X\nnetlist: d.spice\nclock_pin: CLK\ndata_pin: D\nout_pin: Q\ntie: SCE=x\nslews: 0.05\nloads: 0.005\nvdd: 1.8\n";
    assert!(CharJob::parse(bad, ".").is_err());
}

#[test]
fn reset_active_high_when_no_b_suffix() {
    let job = CharJob::parse(
        "cell: DFF\nnetlist: dff.spice\nclock_pin: CLK\ndata_pin: D\nout_pin: Q\n\
         reset_pin: RST\nreset_active: high\nslews: 0.05\nloads: 0.005\nvdd: 1.8\n",
        ".",
    )
    .unwrap();
    assert!(!job.reset_active_low);
}

#[test]
fn deck_reset_q_asserts_reset_and_measures_q_fall() {
    // active-low reset: held high, asserted low at 6ns; Q falls.
    let d = deck_reset_q(
        "t", &["dff.spice".into()], &[], "X1 CLK D RESET_B VGND VNB VPB VPWR Q DFRTP",
        "CLK", "D", "Q", "RESET_B", 1.8, 0.05, 0.05, 0.005, true, true, &[],
    );
    assert!(d.contains("RRST rsts RESET_B 1"), "reset driven through series R");
    // reset starts high (inactive) and goes to 0 (asserted)
    assert!(d.contains("VRST rsts 0 PWL(0 1.8"), "reset idles inactive (high)");
    // reset->Q measured: reset falling -> Q falling
    assert!(d.contains("TRIG v(RESET_B) VAL=0.9 FALL=1"), "active-low reset edge falls");
    assert!(d.contains("TARG v(Q) VAL=0.9 FALL=1"), "Q cleared (falls)");
    assert!(d.contains("rstq"));
}

#[test]
fn render_seq_emits_clear_and_reset_arc() {
    let (slews, loads) = (vec![0.05], vec![0.005]);
    let cell = SeqCell {
        cell: "DFRTP".into(),
        clock_pin: "CLK".into(),
        data_pin: "D".into(),
        out_pin: "Q".into(),
        rising_edge: true,
        setup_rise: Table { values: vec![vec![0.10]] },
        setup_fall: Table { values: vec![vec![0.10]] },
        hold_rise: Table { values: vec![vec![0.02]] },
        hold_fall: Table { values: vec![vec![0.02]] },
        ckq_rise: Table { values: vec![vec![0.25]] },
        ckq_fall: Table { values: vec![vec![0.24]] },
        ckq_rise_trans: Table { values: vec![vec![0.06]] },
        ckq_fall_trans: Table { values: vec![vec![0.05]] },
        clear: "!RESET_B".into(),
        reset_pin: "RESET_B".into(),
        reset_q: Table { values: vec![vec![0.18]] },
        reset_q_trans: Table { values: vec![vec![0.05]] },
    };
    let lib = render_seq("L", &Units::default(), &slews, &loads, &cell);
    assert!(lib.contains("clear : \"!RESET_B\""), "ff clear attribute");
    assert!(lib.contains("pin (RESET_B)"), "reset pin emitted");
    assert!(lib.contains("timing_type : clear;"), "async reset->Q arc");
    assert!(lib.contains("related_pin : \"RESET_B\""));
    // no clear arc when reset_q is zero
    let mut bare = cell.clone();
    bare.reset_q = Table::new(1, 1);
    let lib2 = render_seq("L", &Units::default(), &slews, &loads, &bare);
    assert!(!lib2.contains("timing_type : clear;"), "no arc when uncharacterized");
    assert!(lib2.contains("clear : \"!RESET_B\""), "clear attribute still emitted");
}
