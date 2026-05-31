// Per-corner sweeps: `corner:` lines parse into a CharJob, and run_corners renders
// one library per corner with the corner's nominal voltage/temperature in the header.
use vyges_char::job::CharJob;
use vyges_char::liberty::{render, Arc, Table, Units};

const JOB: &str = "\
cell:    INV
netlist: inv.spice
in_pin:  A
out_pin: Y
slews:   0.01, 0.04
loads:   0.001, 0.004
corner:  ss_n40C_1v60 | params_ss.spice, corners/ss.spice | 1.60 | -40
corner:  tt_025C_1v80 | params_tt.spice, corners/tt.spice | 1.80 | 25
corner:  ff_125C_1v95 | params_ff.spice, corners/ff.spice | 1.95 | 125
";

#[test]
fn parses_corner_lines() {
    let job = CharJob::parse(JOB, ".").unwrap();
    assert_eq!(job.corners.len(), 3);
    let ss = &job.corners[0];
    assert_eq!(ss.name, "ss_n40C_1v60");
    assert_eq!(ss.models, vec!["params_ss.spice", "corners/ss.spice"]);
    assert!((ss.vdd - 1.60).abs() < 1e-12);
    assert!((ss.temp - -40.0).abs() < 1e-12);
    // top-level vdd/models fall back to the first corner when not given
    assert!((job.vdd - 1.60).abs() < 1e-12);
    assert_eq!(job.models, vec!["params_ss.spice", "corners/ss.spice"]);
    let ff = &job.corners[2];
    assert!((ff.vdd - 1.95).abs() < 1e-12 && (ff.temp - 125.0).abs() < 1e-12);
}

#[test]
fn corner_temp_defaults_to_top_level() {
    let job = CharJob::parse(
        "cell: INV\nnetlist: inv.spice\nin_pin: A\nout_pin: Y\nslews: 0.01\nloads: 0.001\ntemp: 85\n\
         corner: tt | corners/tt.spice | 1.8\n",
        ".",
    )
    .unwrap();
    assert_eq!(job.corners.len(), 1);
    assert!((job.corners[0].temp - 85.0).abs() < 1e-12, "omitted corner temp inherits job temp");
}

#[test]
fn bad_corner_line_rejected() {
    let bad = "cell: INV\nnetlist: i.spice\nin_pin: A\nout_pin: Y\nslews: 0.01\nloads: 0.001\ncorner: only_two | corners/tt.spice\n";
    assert!(CharJob::parse(bad, ".").is_err(), "corner needs name|models|vdd");
}

#[test]
fn no_corner_lines_is_single_run() {
    let job = CharJob::parse(
        "cell: INV\nnetlist: inv.spice\nin_pin: A\nout_pin: Y\nslews: 0.01\nloads: 0.001\nvdd: 1.8\n",
        ".",
    )
    .unwrap();
    assert!(job.corners.is_empty());
}

#[test]
fn units_carry_corner_nominal_voltage_and_temp() {
    // the lib header should reflect the corner's nominal supply/temperature
    let (slews, loads) = (vec![0.01], vec![0.001]);
    let arc = Arc {
        cell: "INV".into(),
        in_pin: "A".into(),
        out_pin: "Y".into(),
        sense: "negative_unate".into(),
        cell_rise: Table { values: vec![vec![0.1]] },
        cell_fall: Table { values: vec![vec![0.1]] },
        rise_transition: Table { values: vec![vec![0.05]] },
        fall_transition: Table { values: vec![vec![0.05]] },
        sigma_rise: Table::new(1, 1),
        sigma_fall: Table::new(1, 1),
        ccs_rise: vec![],
        ccs_fall: vec![],
        recv_c1_rise: Table::new(1, 1),
        recv_c2_rise: Table::new(1, 1),
        recv_c1_fall: Table::new(1, 1),
        recv_c2_fall: Table::new(1, 1),
        int_rise: Table::new(2, 2),
        int_fall: Table::new(2, 2),
        leakage: vec![],
    };
    let units = Units { nom_voltage: 1.60, nom_temp: -40.0, ..Units::default() };
    let lib = render("INV__ss", &units, &slews, &loads, &[arc]);
    assert!(lib.contains("nom_voltage : 1.6000"), "corner supply in header");
    assert!(lib.contains("nom_temperature : -40.0"), "corner temp in header");
    assert!(lib.contains("library (INV__ss)"));
}
