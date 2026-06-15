use vyges_char::liberty::{render, Arc, Table, Units};

fn arc() -> Arc {
    let mk = |b: f64| {
        let mut t = Table::new(2, 2);
        t.values = vec![vec![b, b + 1.0], vec![b + 2.0, b + 3.0]];
        t
    };
    Arc {
        cell: "INV".into(),
        in_pin: "A".into(),
        out_pin: "Y".into(),
        sense: "negative_unate".into(),
        cell_rise: mk(0.1),
        cell_fall: mk(0.2),
        rise_transition: mk(0.3),
        fall_transition: mk(0.4),
        sigma_rise: vyges_char::liberty::Table::new(0, 0),
        sigma_fall: vyges_char::liberty::Table::new(0, 0),
        ccs_rise: vec![],
        ccs_fall: vec![],
        recv_c1_rise: Table::new(2, 2),
        recv_c2_rise: Table::new(2, 2),
        recv_c1_fall: Table::new(2, 2),
        recv_c2_fall: Table::new(2, 2),
        int_rise: Table::new(2, 2),
        int_fall: Table::new(2, 2),
        leakage: vec![],
    }
}

#[test]
fn emits_valid_structure() {
    let lib = render("L", &Units::default(), &[0.01, 0.04], &[0.001, 0.004], &[arc()]);
    assert!(lib.contains("library (L) {"));
    assert!(lib.contains("delay_model : table_lookup;"));
    assert!(lib.contains("lu_table_template (vyges_nldm)"));
    assert!(lib.contains("cell (INV)"));
    assert!(lib.contains("pin (Y)"));
    assert!(lib.contains("related_pin : \"A\";"));
    assert!(lib.contains("timing_sense : negative_unate;"));
    for tbl in ["cell_rise", "cell_fall", "rise_transition", "fall_transition"] {
        assert!(lib.contains(tbl), "missing {tbl}");
    }
    // index from the slew/load vectors
    assert!(lib.contains("index_1 (\"0.01, 0.04\")"));
    assert!(lib.contains("index_2 (\"0.001, 0.004\")"));
}

#[test]
fn table_values_rendered() {
    let lib = render("L", &Units::default(), &[0.01, 0.04], &[0.001, 0.004], &[arc()]);
    assert!(lib.contains("0.100000")); // cell_rise[0][0]
    assert!(lib.contains("3.100000")); // cell_rise[1][1] = 0.1+3
}

#[test]
fn balanced_braces() {
    let lib = render("L", &Units::default(), &[0.01, 0.04], &[0.001, 0.004], &[arc()]);
    let open = lib.matches('{').count();
    let close = lib.matches('}').count();
    assert_eq!(open, close, "unbalanced braces in emitted .lib");
}
