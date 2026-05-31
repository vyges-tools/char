use vyges_char::spice::{deck, parse_measures, parse_subckt_pins};

#[test]
fn deck_has_essentials() {
    let d = deck(
        "t", &["inv.spice".into()], &[], "X1 A Y VDD VSS INV",
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
    assert!(!d.contains("pre_osdi")); // no OSDI block when none requested
}

#[test]
fn deck_emits_osdi_preload() {
    let d = deck(
        "t", &["inv.spice".into()], &["psp103.osdi".into(), "hicum.osdi".into()],
        "X1 A Y VDD VSS INV", "A", "Y", 1.2, 0.04, 0.002, false,
    );
    // control block with pre_osdi for each model, before the includes
    assert!(d.contains(".control\npre_osdi psp103.osdi\npre_osdi hicum.osdi\n.endc"));
    assert!(d.find("pre_osdi").unwrap() < d.find(".include").unwrap());
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

#[test]
fn parses_real_sky130_subckt_pin_order() {
    let nl = "\
* sky130 hd cells
.subckt sky130_fd_sc_hd__inv_1 A VGND VNB VPB VPWR Y
M0 Y A VGND VNB nfet
M1 Y A VPWR VPB pfet
.ends
";
    let pins = parse_subckt_pins(nl, "sky130_fd_sc_hd__inv_1").unwrap();
    assert_eq!(pins, vec!["A", "VGND", "VNB", "VPB", "VPWR", "Y"]);
    assert!(parse_subckt_pins(nl, "sky130_fd_sc_hd__nand2_1").is_none());
}

#[test]
fn skips_blank_and_comment_lines_before_target() {
    // a combined PDK netlist has many cells + blank/comment lines before ours
    let nl = "* header\n\n.subckt other X Y\n.ends\n\n\n* the one we want\n\
              .subckt sky130_fd_sc_hd__inv_1 A VGND VNB VPB VPWR Y\n.ends\n";
    let pins = parse_subckt_pins(nl, "sky130_fd_sc_hd__inv_1").unwrap();
    assert_eq!(pins, vec!["A", "VGND", "VNB", "VPB", "VPWR", "Y"]);
}

#[test]
fn parses_gf180_subckt_pin_order() {
    // gf180mcu inverter: input I, output ZN, well pins VNW/VPW (uppercase .SUBCKT)
    let nl = "* gf180 cells\n\
              .SUBCKT gf180mcu_fd_sc_mcu7t5v0__inv_1 I ZN VDD VNW VPW VSS\n.ENDS\n";
    let pins = parse_subckt_pins(nl, "gf180mcu_fd_sc_mcu7t5v0__inv_1").unwrap();
    assert_eq!(pins, vec!["I", "ZN", "VDD", "VNW", "VPW", "VSS"]);
}

#[test]
fn folds_continuation_lines() {
    let nl = ".subckt big A B\n+ C D\n+ VGND VPWR\nM0 ...\n.ends\n";
    let pins = parse_subckt_pins(nl, "big").unwrap();
    assert_eq!(pins, vec!["A", "B", "C", "D", "VGND", "VPWR"]);
}
