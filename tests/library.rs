//! Library-scale: manifest parsing + the per-cell `.lib` merge. (The parallel
//! ngspice run itself needs the EDA host; the merge + parse are pure and tested
//! here against synthetic per-cell libraries.)

use vyges_char::liberty::merge_libraries;
use vyges_char::library::LibraryJob;

#[test]
fn manifest_parses_jobs_and_threads() {
    let m = "library: sky130_subset\nthreads: 4\njobs: a.char, b.char, c.char\n";
    let j = LibraryJob::parse(m, "/work").expect("parse");
    assert_eq!(j.library, "sky130_subset");
    assert_eq!(j.threads, 4);
    assert_eq!(j.jobs, vec!["a.char", "b.char", "c.char"]);
}

#[test]
fn manifest_defaults_threads_to_parallelism() {
    let m = "library: x\njobs: a.char\n";
    let j = LibraryJob::parse(m, ".").unwrap();
    assert!(j.threads >= 1, "threads default to available parallelism");
}

#[test]
fn manifest_requires_library_and_jobs() {
    assert!(
        LibraryJob::parse("jobs: a.char\n", ".").is_err(),
        "missing library"
    );
    assert!(
        LibraryJob::parse("library: x\n", ".").is_err(),
        "missing jobs"
    );
    assert!(
        LibraryJob::parse("library: x\nbogus: 1\njobs: a.char\n", ".").is_err(),
        "unknown key"
    );
}

// a comb cell lib (nldm template) ...
const LIB_A: &str = r#"library (a_char) {
  delay_model : table_lookup;
  time_unit : "1ns";
  capacitive_load_unit (1, "pf");
  voltage_unit : "1V";
  nom_process : 1.0;
  nom_temperature : 25.0;
  nom_voltage : 1.8000;

  lu_table_template (vyges_nldm) {
    variable_1 : input_net_transition;
    variable_2 : total_output_net_capacitance;
    index_1 ("0.01, 0.08");
    index_2 ("0.001, 0.01");
  }

  cell (INV) {
    pin (Y) {
      direction : output;
      timing () { related_pin : "A"; }
    }
  }
}
"#;

// ... and a seq cell lib that adds the constraint template + leakage unit.
const LIB_B: &str = r#"library (dff_char) {
  delay_model : table_lookup;
  time_unit : "1ns";
  capacitive_load_unit (1, "pf");
  voltage_unit : "1V";
  leakage_power_unit : "1nW";
  nom_process : 1.0;
  nom_temperature : 25.0;
  nom_voltage : 1.8000;

  lu_table_template (vyges_nldm) {
    variable_1 : input_net_transition;
    variable_2 : total_output_net_capacitance;
    index_1 ("0.01, 0.08");
    index_2 ("0.001, 0.01");
  }

  lu_table_template (vyges_constraint) {
    variable_1 : related_pin_transition;
    variable_2 : constrained_pin_transition;
    index_1 ("0.01, 0.08");
    index_2 ("0.01, 0.08");
  }

  cell (DFF) {
    ff (IQ, IQN) { clocked_on : "CLK"; next_state : "D"; }
    pin (Q) { direction : output; }
  }
}
"#;

fn braces_balanced(s: &str) -> bool {
    let mut d = 0i32;
    for c in s.chars() {
        match c {
            '{' => d += 1,
            '}' => d -= 1,
            _ => {}
        }
        if d < 0 {
            return false;
        }
    }
    d == 0
}

#[test]
fn merge_unions_templates_and_keeps_all_cells() {
    let merged = merge_libraries("merged_lib", &[LIB_A.to_string(), LIB_B.to_string()]);

    // one library wrapper, balanced.
    assert!(merged.starts_with("library (merged_lib) {"));
    assert!(braces_balanced(&merged), "merged library braces balance");

    // both cells present.
    assert!(merged.contains("cell (INV) {"));
    assert!(merged.contains("cell (DFF) {"));

    // templates unioned, each declared exactly once (vyges_nldm appears in BOTH
    // inputs but must not be duplicated; vyges_constraint comes only from B).
    assert_eq!(merged.matches("lu_table_template (vyges_nldm)").count(), 1);
    assert_eq!(
        merged
            .matches("lu_table_template (vyges_constraint)")
            .count(),
        1
    );

    // library-level attributes unioned (leakage unit came only from B).
    assert_eq!(merged.matches("delay_model : table_lookup;").count(), 1);
    assert!(merged.contains("leakage_power_unit : \"1nW\";"));

    // the templates precede the cells (a valid Liberty ordering).
    let tmpl = merged.find("lu_table_template").unwrap();
    let cell = merged.find("cell (").unwrap();
    assert!(tmpl < cell, "templates declared before cells");
}

#[test]
fn merge_single_lib_is_well_formed() {
    let merged = merge_libraries("solo", &[LIB_A.to_string()]);
    assert!(braces_balanced(&merged));
    assert!(merged.contains("cell (INV) {"));
    assert_eq!(
        merged.matches("library (").count(),
        1,
        "exactly one library header"
    );
}
