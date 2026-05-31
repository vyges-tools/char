use vyges_char::liberty::{render_json, Arc, Table};

#[test]
fn json_summary_has_tables() {
    let mut t = Table::new(1, 1);
    t.values[0][0] = 0.12;
    let arc = Arc {
        cell: "INV".into(),
        in_pin: "A".into(),
        out_pin: "Y".into(),
        sense: "negative_unate".into(),
        cell_rise: t.clone(),
        cell_fall: t.clone(),
        rise_transition: t.clone(),
        fall_transition: t,
        sigma_rise: vyges_char::liberty::Table::new(0, 0),
        sigma_fall: vyges_char::liberty::Table::new(0, 0),
        ccs_rise: vec![],
        ccs_fall: vec![],
    };
    let j = render_json("lib", &[0.01], &[0.001], &[arc]);
    assert!(j.contains("\"library\":\"lib\""));
    assert!(j.contains("\"cell\":\"INV\""));
    assert!(j.contains("\"cell_rise\":[[0.120000]]"));
    assert!(j.trim_end().ends_with('}'));
}
