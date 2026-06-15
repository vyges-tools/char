// CCS receiver capacitance: input-pin sense-source deck + emission of the
// receiver_capacitance1/2_rise/fall tables vyges-sta-si consumes as the pin load.
use vyges_char::liberty::{render, Arc, Table, Units};
use vyges_char::spice::deck_recv;

#[test]
fn deck_recv_senses_input_current() {
    let d = deck_recv(
        "t", &["inv.spice".into()], &[], "X1 A Y VDD VSS INV", "A", "Y", 1.8, 0.04, 0.002, true,
        "/tmp/r.dat",
    );
    // input is driven through a sense source so i(Vsin) is the pin current
    assert!(d.contains("VIN in_src 0 PWL"), "drive a source node, not the pin directly");
    assert!(d.contains("Vsin in_src A 0"), "0 V sense source in series with the input pin");
    assert!(d.contains("CL Y 0 0.002p"), "output loaded so Miller current flows");
    assert!(d.contains("wrdata /tmp/r.dat i(Vsin)"), "dumps the input current");
    assert!(d.contains(".control") && d.contains(".endc"));
}

fn tbl(v: f64) -> Table {
    Table { values: vec![vec![v; 2]; 2] }
}

fn arc_with_recv() -> Arc {
    Arc {
        cell: "INV".into(),
        in_pin: "A".into(),
        out_pin: "Y".into(),
        sense: "negative_unate".into(),
        cell_rise: tbl(0.10),
        cell_fall: tbl(0.10),
        rise_transition: tbl(0.05),
        fall_transition: tbl(0.05),
        sigma_rise: Table::new(2, 2),
        sigma_fall: Table::new(2, 2),
        ccs_rise: vec![],
        ccs_fall: vec![],
        recv_c1_rise: tbl(0.0030),
        recv_c2_rise: tbl(0.0052), // C2 > C1: Miller inflation
        recv_c1_fall: tbl(0.0031),
        recv_c2_fall: tbl(0.0050),
        int_rise: Table::new(2, 2),
        int_fall: Table::new(2, 2),
        leakage: vec![],
    }
}

#[test]
fn render_emits_receiver_capacitance() {
    let (slews, loads) = (vec![0.01, 0.04], vec![0.001, 0.01]);
    let lib = render("x", &Units::default(), &slews, &loads, &[arc_with_recv()]);
    assert!(lib.contains("receiver_capacitance () {"), "receiver model group");
    assert!(lib.contains("receiver_capacitance1_rise"));
    assert!(lib.contains("receiver_capacitance1_fall"));
    assert!(lib.contains("receiver_capacitance2_rise"));
    assert!(lib.contains("receiver_capacitance2_fall"));
    // the input pin also carries the conventional single-number capacitance
    assert!(lib.contains("capacitance : "), "nominal input capacitance emitted");
    // nominal cap = mean of the C1 lanes = (0.0030 + 0.0031)/2 = 0.00305
    assert!(lib.contains("capacitance : 0.003050"), "nominal cap = mean of C1 lanes");
}

#[test]
fn render_omits_receiver_when_uncharacterized() {
    let (slews, loads) = (vec![0.01, 0.04], vec![0.001, 0.01]);
    let mut a = arc_with_recv();
    a.recv_c1_rise = Table::new(2, 2);
    a.recv_c2_rise = Table::new(2, 2);
    a.recv_c1_fall = Table::new(2, 2);
    a.recv_c2_fall = Table::new(2, 2);
    let lib = render("x", &Units::default(), &slews, &loads, &[a]);
    assert!(!lib.contains("receiver_capacitance"), "no receiver model when not characterized");
    assert!(!lib.contains("capacitance : "), "no nominal cap line either (NLDM-only)");
}
