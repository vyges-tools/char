use vyges_char::spice::{deck, parse_measures};

#[test]
fn deck_has_essentials() {
    let d = deck(
        "t", &["inv.spice".into()], "X1 A Y VDD VSS INV",
        "A", "Y", 1.8, 0.04, 0.002, false,
    );
    assert!(d.contains(".include \"inv.spice\""));
    assert!(d.contains("VVDD VDD 0 1.8"));
    assert!(d.contains("X1 A Y VDD VSS INV"));
    assert!(d.contains("CL Y 0 0.002p"));
    assert!(d.contains(".measure tran prop_delay"));
    assert!(d.contains(".measure tran out_slew"));
    assert!(d.contains(".tran"));
    assert!(d.trim_end().ends_with(".end"));
}

#[test]
fn parses_ngspice_measures() {
    let out = "\
Circuit: char
prop_delay           =  1.234560e-10 targ=  2.0e-09 trig= 1.0e-09
out_slew             =  5.600000e-11
some noise = not_a_number xx
";
    let m = parse_measures(out);
    assert!((m["prop_delay"] - 1.23456e-10).abs() < 1e-16);
    assert!((m["out_slew"] - 5.6e-11).abs() < 1e-16);
    assert!(!m.contains_key("some noise"));
}
