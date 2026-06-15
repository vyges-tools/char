// Power: power_char job knob, and render of internal_power (per arc) +
// leakage_power / cell_leakage_power (per cell) with the leakage_power_unit header.
use vyges_char::job::CharJob;
use vyges_char::liberty::{render, Arc, Table, Units};

#[test]
fn power_char_knob_parses() {
    let job = CharJob::parse(
        "cell: INV\nnetlist: inv.spice\nin_pin: A\nout_pin: Y\nslews: 0.01\nloads: 0.001\nvdd: 1.8\npower_char: true\n",
        ".",
    )
    .unwrap();
    assert!(job.power_char);
    // `power:` still means the power-pin list, not the boolean
    assert!(job.power.contains(&"VPWR".to_string()));
}

fn arc_with_power() -> Arc {
    Arc {
        cell: "INV".into(),
        in_pin: "A".into(),
        out_pin: "Y".into(),
        sense: "negative_unate".into(),
        cell_rise: Table { values: vec![vec![0.10, 0.20]] },
        cell_fall: Table { values: vec![vec![0.10, 0.20]] },
        rise_transition: Table { values: vec![vec![0.05, 0.06]] },
        fall_transition: Table { values: vec![vec![0.05, 0.06]] },
        sigma_rise: Table::new(1, 2),
        sigma_fall: Table::new(1, 2),
        ccs_rise: vec![],
        ccs_fall: vec![],
        recv_c1_rise: Table::new(1, 2),
        recv_c2_rise: Table::new(1, 2),
        recv_c1_fall: Table::new(1, 2),
        recv_c2_fall: Table::new(1, 2),
        int_rise: Table { values: vec![vec![0.0021, 0.0030]] },
        int_fall: Table { values: vec![vec![0.0018, 0.0025]] },
        leakage: vec![("!A".into(), 0.0042), ("A".into(), 0.0051)],
    }
}

#[test]
fn render_emits_internal_and_leakage_power() {
    let (slews, loads) = (vec![0.01], vec![0.001, 0.004]);
    let lib = render("x", &Units::default(), &slews, &loads, &[arc_with_power()]);
    // header unit
    assert!(lib.contains("leakage_power_unit : \"1nW\";"), "leakage unit declared");
    // cell-level leakage: average + per-state when groups
    assert!(lib.contains("cell_leakage_power :"), "average leakage");
    assert!(lib.contains("leakage_power () {"));
    assert!(lib.contains("when : \"!A\"") && lib.contains("when : \"A\""));
    // average = (0.0042 + 0.0051)/2 = 0.00465
    assert!(lib.contains("cell_leakage_power : 0.004650"));
    // per-arc internal power
    assert!(lib.contains("internal_power () {"));
    assert!(lib.contains("rise_power (vyges_nldm)"));
    assert!(lib.contains("fall_power (vyges_nldm)"));
    // brace-balanced
    assert_eq!(lib.matches('{').count(), lib.matches('}').count(), "balanced braces");
}

#[test]
fn no_power_means_no_power_groups() {
    let (slews, loads) = (vec![0.01], vec![0.001, 0.004]);
    let mut a = arc_with_power();
    a.int_rise = Table::new(1, 2);
    a.int_fall = Table::new(1, 2);
    a.leakage = vec![];
    let lib = render("x", &Units::default(), &slews, &loads, &[a]);
    assert!(!lib.contains("internal_power"), "no internal_power when uncharacterized");
    assert!(!lib.contains("leakage_power"), "no leakage when uncharacterized");
}
