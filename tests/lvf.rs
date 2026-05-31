// LVF/POCV: Monte-Carlo deck (seed + mismatch), sample sigma, and emission of
// ocv_sigma_cell_rise/fall — the tables vyges-sta-si consumes for POCV.
use vyges_char::engine::stddev;
use vyges_char::liberty::{render, Arc, Table, Units};
use vyges_char::spice::deck;

fn tbl(v: f64) -> Table {
    Table { values: vec![vec![v; 2]; 2] }
}

#[test]
fn stddev_is_sample_stddev() {
    assert_eq!(stddev(&[5.0]), 0.0); // < 2 samples
    assert!(stddev(&[1.0, 1.0, 1.0]).abs() < 1e-12); // no spread
    assert!((stddev(&[1.0, 2.0, 3.0]) - 1.0).abs() < 1e-12); // n-1 var = 1
}

#[test]
fn deck_monte_carlo_seed_and_mismatch() {
    let d = deck(
        "t", &["inv.spice".into()], &[], "X1 A Y VDD VSS INV", "A", "Y", 1.8, 0.04, 0.002, false,
        Some(7),
    );
    assert!(d.contains("set rndseed=7"), "MC run must seed the RNG");
    assert!(d.contains(".param mc_mm_switch=1"), "MC run must enable mismatch");
}

#[test]
fn render_emits_sigma_only_when_present() {
    let (slews, loads) = (vec![0.01, 0.04], vec![0.001, 0.01]);
    let mut a = Arc {
        cell: "INV".into(),
        in_pin: "A".into(),
        out_pin: "Y".into(),
        sense: "negative_unate".into(),
        cell_rise: tbl(0.10),
        cell_fall: tbl(0.10),
        rise_transition: tbl(0.05),
        fall_transition: tbl(0.05),
        sigma_rise: tbl(0.02),
        sigma_fall: tbl(0.02),
        ccs_rise: vec![],
        ccs_fall: vec![],
        recv_c1_rise: Table::new(2, 2),
        recv_c2_rise: Table::new(2, 2),
        recv_c1_fall: Table::new(2, 2),
        recv_c2_fall: Table::new(2, 2),
        int_rise: Table::new(2, 2),
        int_fall: Table::new(2, 2),
        leakage: vec![],
    };
    let lib = render("x", &Units::default(), &slews, &loads, std::slice::from_ref(&a));
    assert!(lib.contains("ocv_sigma_cell_rise"), "LVF sigma must be emitted");
    assert!(lib.contains("ocv_sigma_cell_fall"));
    assert!(lib.contains("sigma_type"));
    assert!(lib.contains("0.020000"), "sigma value present");

    // zero sigma -> NLDM-only lib, no LVF groups
    a.sigma_rise = Table::new(2, 2);
    a.sigma_fall = Table::new(2, 2);
    let lib2 = render("x", &Units::default(), &slews, &loads, &[a]);
    assert!(!lib2.contains("ocv_sigma"), "no sigma tables when not characterized");
}
