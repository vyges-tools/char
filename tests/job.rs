use vyges_char::job::CharJob;

const SAMPLE: &str = "\
cell:    sky130_fd_sc_hd__inv_1   # inverter
netlist: inv.spice
in_pin:  A
out_pin: Y
sense:   negative_unate
slews:   0.01, 0.04, 0.16, 0.64
loads:   0.0005, 0.002, 0.008
vdd:     1.8
temp:    25
models:  a.spice, b.spice
";

#[test]
fn parses_sample() {
    let j = CharJob::parse(SAMPLE, ".").unwrap();
    assert_eq!(j.cell, "sky130_fd_sc_hd__inv_1");
    assert_eq!(j.in_pin, "A");
    assert_eq!(j.out_pin, "Y");
    assert_eq!(j.slews.len(), 4);
    assert_eq!(j.loads.len(), 3);
    assert_eq!(j.vdd, 1.8);
    assert_eq!(j.models, vec!["a.spice", "b.spice"]);
}

#[test]
fn rejects_missing_required() {
    let bad = "cell: x\nnetlist: n\nin_pin: A\n"; // no out_pin/slews/loads/vdd
    assert!(CharJob::parse(bad, ".").is_err());
}

#[test]
fn rejects_bad_number() {
    let bad = SAMPLE.replace("vdd:     1.8", "vdd:     abc");
    assert!(CharJob::parse(&bad, ".").is_err());
}
