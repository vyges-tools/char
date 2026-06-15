// CCS: output-current waveform capture deck + emission of output_current_rise/fall
// vector groups — the format vyges-sta-si's CcsArc parser consumes.
use vyges_char::liberty::{render, Arc, Table, Units, Waveform};
use vyges_char::spice::{deck_ccs, parse_wrdata};

#[test]
fn deck_ccs_has_sense_source_and_wrdata() {
    let d = deck_ccs(
        "t", &["inv.spice".into()], &[], "X1 A Y VDD VSS INV", "A", "Y", 1.8, 0.04, 0.002, false,
        "/tmp/x.dat",
    );
    assert!(d.contains("Vsns Y Y_c 0"), "0 V sense source on the output");
    assert!(d.contains("CL Y_c 0 0.002p"), "load behind the sense");
    assert!(d.contains("tran 0.5p") && d.contains("0.9n"));
    assert!(d.contains("wrdata /tmp/x.dat i(Vsns)"), "dumps the output current");
    assert!(d.contains(".control") && d.contains(".endc"));
}

#[test]
fn parse_wrdata_two_columns() {
    let s = "0.0 0.0\n1.0e-10 1.5e-4\n2.0e-10 3.0e-4\nbad line\n";
    let v = parse_wrdata(s);
    assert_eq!(v.len(), 3);
    assert!((v[2].1 - 3.0e-4).abs() < 1e-12);
}

fn wf(slew: f64, load: f64) -> Waveform {
    Waveform {
        slew,
        load,
        ref_time: 1.0 + slew / 2.0,
        time: vec![1.0, 1.05, 1.1, 1.2],
        current: vec![0.0, 0.5, 0.3, 0.0],
    }
}

#[test]
fn render_emits_output_current_vectors() {
    let (slews, loads) = (vec![0.01, 0.04], vec![0.001, 0.01]);
    let a = Arc {
        cell: "INV".into(),
        in_pin: "A".into(),
        out_pin: "Y".into(),
        sense: "negative_unate".into(),
        cell_rise: Table::new(2, 2),
        cell_fall: Table::new(2, 2),
        rise_transition: Table::new(2, 2),
        fall_transition: Table::new(2, 2),
        sigma_rise: Table::new(2, 2),
        sigma_fall: Table::new(2, 2),
        ccs_rise: vec![wf(0.01, 0.001), wf(0.04, 0.01)],
        ccs_fall: vec![wf(0.01, 0.001)],
        recv_c1_rise: Table::new(2, 2),
        recv_c2_rise: Table::new(2, 2),
        recv_c1_fall: Table::new(2, 2),
        recv_c2_fall: Table::new(2, 2),
        int_rise: Table::new(2, 2),
        int_fall: Table::new(2, 2),
        leakage: vec![],
    };
    let lib = render("x", &Units::default(), &slews, &loads, &[a]);
    // the tokens vyges-sta-si's parse_ccs_set looks for
    assert!(lib.contains("lu_table_template (ccs_tmpl)"), "ccs time template declared");
    assert!(lib.contains("output_current_rise () {"));
    assert!(lib.contains("output_current_fall () {"));
    assert!(lib.contains("vector (ccs_tmpl) {"));
    assert!(lib.contains("reference_time :"));
    assert!(lib.contains("index_3 (")); // the time axis
    // two rise vectors + one fall vector
    assert_eq!(lib.matches("vector (ccs_tmpl)").count(), 3);
}
