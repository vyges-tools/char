// Async set/reset flops: tie:/reset_pin:/set_pin: parse, the async control->Q deck,
// and render_seq emission of the ff clear/preset attribute, the async ->Q timing arc,
// and the recovery/removal constraints.
use vyges_char::job::CharJob;
use vyges_char::liberty::{render_seq, AsyncCtl, SeqCell, Table, Units};
use vyges_char::spice::deck_async_q;

#[test]
fn parses_reset_and_set_pins() {
    let job = CharJob::parse(
        "cell: DFBBP\nnetlist: dff.spice\nclock_pin: CLK\ndata_pin: D\nout_pin: Q\n\
         reset_pin: RESET_B\nset_pin: SET_B\nslews: 0.05\nloads: 0.005\nvdd: 1.8\n",
        ".",
    )
    .unwrap();
    assert_eq!(job.reset_pin, "RESET_B");
    assert!(job.reset_active_low, "RESET_B -> active-low");
    assert_eq!(job.set_pin, "SET_B");
    assert!(job.set_active_low, "SET_B -> active-low");
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
fn set_active_high_when_no_b_suffix() {
    let job = CharJob::parse(
        "cell: DFF\nnetlist: dff.spice\nclock_pin: CLK\ndata_pin: D\nout_pin: Q\n\
         set_pin: SET\nset_active: high\nslews: 0.05\nloads: 0.005\nvdd: 1.8\n",
        ".",
    )
    .unwrap();
    assert!(!job.set_active_low);
}

#[test]
fn deck_async_q_reset_clears_q_low() {
    // active-low reset: primes Q=1 (D high), asserts low at 6ns, Q falls.
    let d = deck_async_q(
        "t", &["dff.spice".into()], &[], "X1 CLK D RESET_B VGND VNB VPB VPWR Q DFRTP",
        "CLK", "D", "Q", "RESET_B", 1.8, 0.05, 0.05, 0.005, true, true, false, &[],
    );
    assert!(d.contains("RASY asys RESET_B 1"), "async driven through series R");
    assert!(d.contains("VD ds 0 PWL(0 1.8"), "reset primes Q=1 (data high)");
    assert!(d.contains("TRIG v(RESET_B) VAL=0.9 FALL=1"), "active-low assertion falls");
    assert!(d.contains("TARG v(Q) VAL=0.9 FALL=1"), "reset clears Q (falls)");
}

#[test]
fn deck_async_q_set_drives_q_high() {
    // active-low set: primes Q=0 (D low), asserts low at 6ns, Q rises.
    let d = deck_async_q(
        "t", &["dff.spice".into()], &[], "X1 CLK D SET_B VGND VNB VPB VPWR Q DFSTP",
        "CLK", "D", "Q", "SET_B", 1.8, 0.05, 0.05, 0.005, true, true, true, &[],
    );
    assert!(d.contains("VD ds 0 PWL(0 0)"), "set primes Q=0 (data low)");
    assert!(d.contains("TARG v(Q) VAL=0.9 RISE=1"), "set drives Q high (rises)");
}

fn ctl(pin: &str, expr: &str, sets_high: bool) -> AsyncCtl {
    AsyncCtl {
        pin: pin.into(),
        expr: expr.into(),
        sets_high,
        active_low: true,
        q: Table { values: vec![vec![0.15]] },
        q_trans: Table { values: vec![vec![0.05]] },
        recovery: Table { values: vec![vec![0.08]] },
        removal: Table { values: vec![vec![0.02]] },
    }
}

fn seq_with(asyncs: Vec<AsyncCtl>) -> SeqCell {
    SeqCell {
        cell: "DFF".into(),
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
        asyncs,
    }
}

#[test]
fn render_emits_clear_arc_recovery_removal() {
    let (slews, loads) = (vec![0.05], vec![0.005]);
    let lib = render_seq("L", &Units::default(), &slews, &loads, &seq_with(vec![ctl("RESET_B", "!RESET_B", false)]));
    assert!(lib.contains("clear : \"!RESET_B\""), "ff clear attribute");
    assert!(lib.contains("pin (RESET_B)"));
    assert!(lib.contains("timing_type : clear;"), "async reset->Q arc");
    assert!(lib.contains("timing_type : recovery_rising;"));
    assert!(lib.contains("timing_type : removal_rising;"));
    // reset -> Q clears: the ->Q arc is a cell_fall
    assert!(lib.contains("cell_fall (vyges_nldm)"));
    // the emitted Liberty must be brace-balanced (recovery/removal groups closed)
    let (open, close) = (lib.matches('{').count(), lib.matches('}').count());
    assert_eq!(open, close, "unbalanced braces in emitted .lib");
}

#[test]
fn render_emits_preset_for_set() {
    let (slews, loads) = (vec![0.05], vec![0.005]);
    let lib = render_seq("L", &Units::default(), &slews, &loads, &seq_with(vec![ctl("SET_B", "!SET_B", true)]));
    assert!(lib.contains("preset : \"!SET_B\""), "ff preset attribute");
    assert!(lib.contains("timing_type : preset;"), "async set->Q arc");
    // set -> Q drives high: rise_constraint on recovery + cell_rise arc
    assert!(lib.contains("rise_constraint (vyges_constraint)"));
}

#[test]
fn render_both_set_and_reset() {
    let (slews, loads) = (vec![0.05], vec![0.005]);
    let cell = seq_with(vec![ctl("RESET_B", "!RESET_B", false), ctl("SET_B", "!SET_B", true)]);
    let lib = render_seq("L", &Units::default(), &slews, &loads, &cell);
    assert!(lib.contains("clear : \"!RESET_B\"") && lib.contains("preset : \"!SET_B\""));
    assert!(lib.contains("pin (RESET_B)") && lib.contains("pin (SET_B)"));
}
