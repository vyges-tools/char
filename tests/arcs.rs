//! Arc auto-derivation from Liberty Boolean functions.

use vyges_char::arcs::derive_arcs;

// helper: (in_pin, sense) pairs, sorted, for easy assertion
fn summary(out: &str, func: &str) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = derive_arcs(out, func)
        .unwrap()
        .into_iter()
        .map(|a| (a.in_pin, a.sense))
        .collect();
    v.sort();
    v
}

#[test]
fn inverter_is_negative_unate() {
    assert_eq!(summary("Y", "!A"), vec![("A".into(), "negative_unate".into())]);
    assert_eq!(summary("Y", "A'"), vec![("A".into(), "negative_unate".into())]);
}

#[test]
fn buffer_is_positive_unate() {
    assert_eq!(summary("X", "A"), vec![("A".into(), "positive_unate".into())]);
}

#[test]
fn nand_both_inputs_negative_unate() {
    // Y = !(A & B); juxtaposition AND + postfix prime both parse
    for f in ["!(A & B)", "(A B)'", "!(A*B)"] {
        assert_eq!(
            summary("Y", f),
            vec![("A".into(), "negative_unate".into()), ("B".into(), "negative_unate".into())],
            "func {f}"
        );
    }
}

#[test]
fn nor_and_or_and_senses() {
    assert_eq!(
        summary("Y", "!(A + B)"), // NOR
        vec![("A".into(), "negative_unate".into()), ("B".into(), "negative_unate".into())]
    );
    assert_eq!(
        summary("X", "A + B"), // OR
        vec![("A".into(), "positive_unate".into()), ("B".into(), "positive_unate".into())]
    );
    assert_eq!(
        summary("X", "A & B & C"), // AND3
        vec![
            ("A".into(), "positive_unate".into()),
            ("B".into(), "positive_unate".into()),
            ("C".into(), "positive_unate".into())
        ]
    );
}

#[test]
fn aoi_compound_a21oi() {
    // a21oi: Y = !((A1 & A2) | B) — every input inverting (AOI output)
    assert_eq!(
        summary("Y", "!((A1 A2) + B)"),
        vec![
            ("A1".into(), "negative_unate".into()),
            ("A2".into(), "negative_unate".into()),
            ("B".into(), "negative_unate".into())
        ]
    );
}

#[test]
fn and2b_one_inverted_input() {
    // and2b: X = !A_N & B  -> A_N inverting, B non-inverting
    assert_eq!(
        summary("X", "!A_N & B"),
        vec![("A_N".into(), "negative_unate".into()), ("B".into(), "positive_unate".into())]
    );
}

#[test]
fn xor_is_characterizable_under_a_definite_side() {
    // XOR is globally non-unate; derivation picks a sensitizing side state, so each
    // input gets a definite (measurable) local sense + that side condition.
    let arcs = derive_arcs("X", "A ^ B").unwrap();
    assert_eq!(arcs.len(), 2, "one arc per input");
    for a in &arcs {
        assert!(a.sense == "positive_unate" || a.sense == "negative_unate");
        assert_eq!(a.side.len(), 1, "the other input is pinned to a definite value");
    }
}

#[test]
fn nand_side_input_is_noncontrolling_high() {
    // sensitizing A->Y of a NAND requires B held high (non-controlling for AND)
    let arcs = derive_arcs("Y", "!(A & B)").unwrap();
    let a = arcs.iter().find(|a| a.in_pin == "A").unwrap();
    assert_eq!(a.side, vec![("B".into(), true)]);
}

#[test]
fn rejects_garbage_function() {
    assert!(derive_arcs("Y", "A & ").is_err());
    assert!(derive_arcs("Y", "(A | B").is_err());
}

#[test]
fn derives_arcs_from_a_reference_lib() {
    use vyges_char::arcs::arcs_from_lib;
    let lib = r#"
library (ref) {
  cell ("MYNAND") {
    pin (A) { direction : input; capacitance : 0.001; }
    pin (B) { direction : input; capacitance : 0.001; }
    pin (Y) { direction : output; function : "!(A&B)"; }
  }
  cell ("MYBUF") {
    pin (A) { direction : input; }
    pin (X) { direction : output; function : "A"; }
  }
}
"#;
    let mut s: Vec<(String, String)> = arcs_from_lib(lib, "MYNAND")
        .unwrap()
        .into_iter()
        .map(|a| (a.in_pin, a.sense))
        .collect();
    s.sort();
    assert_eq!(
        s,
        vec![("A".into(), "negative_unate".into()), ("B".into(), "negative_unate".into())]
    );
    // picks the right cell among several; buffer is positive-unate
    assert_eq!(arcs_from_lib(lib, "MYBUF").unwrap()[0].sense, "positive_unate");
    // missing cell errors
    assert!(arcs_from_lib(lib, "NOPE").is_err());
}
