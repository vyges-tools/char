//! Macro Liberty (`vyges-char macro`): what the timer reads back from what we write.
//!
//! The output is read with `vyges_loom::liberty`, a transcription of OpenSTA's `LibertyReader`
//! (the programme pin), so each assertion is a statement about how the timer will see the macro.

use vyges_char::macro_cell::{render, MacroCell};
use vyges_loom::liberty::{ClkEdge, Lib};

/// An SRAM-shaped interface: CSB/WEB/A captured on CLK rising, Q launched on CLK falling
/// (self-timed read), as OpenRAM's Liberty has it too.
fn sram_json() -> String {
    r#"{
  "schema": "vyges-char-macro/1",
  "library": "t_sram__tt", "cell": "t_sram", "comment": "test",
  "area": 1000.5, "nom_voltage": 1.8, "nom_temperature": 25,
  "slew_thresholds_pct": [10, 90],
  "memory": {"type": "ram", "address_width": 2, "word_width": 2},
  "pg_pins": [{"name": "VPWR", "pg_type": "primary_power"}, {"name": "VGND", "pg_type": "primary_ground"}],
  "slews": [0.05, 0.3, 1.0], "loads": [0.001, 0.01, 0.05],
  "clock": {"pin": "CLK", "capacitance": 0.0042, "min_period": 8.0,
            "min_pulse_width_high": 4.0, "min_pulse_width_low": 4.0},
  "inputs": [{"pins": ["CSB", "WEB", "A0", "A1", "D0", "D1"], "capacitance": 0.0018,
              "clock_edge": "rising",
              "setup_rise": [[0.10, 0.20, 0.30], [0.11, 0.21, 0.31], [0.12, 0.22, 0.32]],
              "setup_fall": [[0.15, 0.25, 0.35], [0.16, 0.26, 0.36], [0.17, 0.27, 0.37]],
              "hold_rise":  [[0.01, 0.02, 0.03], [0.04, 0.05, 0.06], [0.07, 0.08, 0.09]],
              "hold_fall":  [[0.02, 0.03, 0.04], [0.05, 0.06, 0.07], [0.08, 0.09, 0.10]]}],
  "outputs": [{"pins": ["Q0", "Q1"], "clock_edge": "falling", "max_capacitance": 0.05,
               "cell_rise": [[2.5, 2.6, 2.9]], "cell_fall": [[2.4, 2.5, 2.8]],
               "rise_transition": [[0.05, 0.12, 0.40]], "fall_transition": [[0.04, 0.10, 0.35]]}]
}"#
    .to_string()
}

fn read(json: &str) -> Lib {
    let m = MacroCell::from_json(json).expect("the test document is valid");
    Lib::parse(&render(&m)).expect("the timer's reader accepts what we write")
}

#[test]
fn macro_is_a_macro_and_its_clock_a_clock() {
    // LibertyReader: `is_macro_cell` -> LibertyCell::isMacro (the ideal-clock clock-tree
    // adjustment in Search.cc / PathEnd.cc keys on it); `clock : true` and the pulse-width
    // attributes make CLK a clock and a check clock (readMinPulseWidth builds the check arc).
    let lib = read(&sram_json());
    let c = &lib.cells["t_sram"];
    assert!(c.is_macro);
    assert!((c.area - 1000.5).abs() < 1e-6);
    let clk = &c.pins["CLK"];
    assert!(clk.clock && clk.is_check_clk, "CLK must be a clock with its own width check");
    assert!(clk.is_reg_clk, "Q's falling_edge arcs launch from CLK: a register clock");
}

#[test]
fn inputs_are_checked_on_the_rising_edge_and_outputs_launch_on_the_falling_edge() {
    // The SRAM's two edges in one cell: a flop's single `rising_edge` flag cannot say this.
    let lib = read(&sram_json());
    let c = &lib.cells["t_sram"];
    for p in ["CSB", "WEB", "A0", "A1", "D0", "D1"] {
        let pin = &c.pins[p];
        assert_eq!(pin.setup.len(), 1, "{p}: one setup group");
        assert_eq!(pin.hold.len(), 1, "{p}: one hold group");
        assert_eq!(pin.setup[0].clk_edge, ClkEdge::Rise, "{p}: setup_rising");
        assert_eq!(pin.hold[0].clk_edge, ClkEdge::Rise, "{p}: hold_rising");
        assert_eq!(pin.setup[0].related_pin, "CLK");
    }
    for q in ["Q0", "Q1"] {
        let arcs = &c.pins[q].arcs;
        assert_eq!(arcs.len(), 1, "{q}: one CLK->Q arc");
        assert_eq!(arcs[0].related_pin, "CLK");
        assert_eq!(arcs[0].clk_edge, Some(ClkEdge::Fall), "{q}: falling_edge");
    }
}

// Tolerance 1e-6: the reader keeps table values as f32, as OpenSTA's `float` tables do.
#[test]
fn constraint_tables_are_clock_transition_by_data_transition() {
    // Template: variable_1 related_pin_transition (clock), variable_2 constrained_pin_transition
    // (data) — OpenRAM's CONSTRAINT_TABLE order. setup_rise[i][j] is (clock slew i, data slew j).
    let lib = read(&sram_json());
    let s = &lib.cells["t_sram"].pins["A0"].setup[0];
    assert!((s.eval_edge(true, 0.05, 1.0) - 0.30).abs() < 1e-6);
    assert!((s.eval_edge(true, 1.0, 0.05) - 0.12).abs() < 1e-6);
    assert!((s.eval_edge(false, 0.3, 0.3) - 0.26).abs() < 1e-6);
}

#[test]
fn a_one_row_output_table_is_over_load_alone() {
    // A table measured over load only reads the same at every clock slew.
    let lib = read(&sram_json());
    let a = &lib.cells["t_sram"].pins["Q0"].arcs[0];
    for slew in [0.05, 1.0] {
        assert!((a.cell_rise.lookup(slew, 0.01) - 2.6).abs() < 1e-6);
        assert!((a.fall_transition.lookup(slew, 0.05) - 0.35).abs() < 1e-6);
    }
}

#[test]
fn the_clock_carries_its_period_and_widths() {
    let m = MacroCell::from_json(&sram_json()).unwrap();
    let text = render(&m);
    assert!(text.contains("min_period : 8.0000;"));
    assert!(text.contains("min_pulse_width_high : 4.0000;"));
    assert!(text.contains("memory () {\n      type : ram;\n      address_width : 2;"));
    assert!(text.contains("slew_lower_threshold_pct_rise : 10;"));
    // the clock tree is inside the measured numbers: declaring it would be counted twice
    assert!(!text.contains("clock_tree_path"));
}

fn rejects(edit: impl Fn(&str) -> String, why: &str) {
    let bad = edit(&sram_json());
    let e = MacroCell::from_json(&bad).expect_err(why);
    assert!(!e.is_empty());
}

#[test]
fn bad_documents_are_errors_not_libraries() {
    rejects(|s| s.replace("vyges-char-macro/1", "vyges-char-macro/9"), "unknown schema");
    rejects(|s| s.replace("\"slew_thresholds_pct\": [10, 90],", ""), "thresholds are required");
    rejects(|s| s.replace("[[2.5, 2.6, 2.9]]", "[[2.5, 2.6]]"), "a short row");
    rejects(|s| s.replace("[[0.10, 0.20, 0.30], ", "["), "a constraint table missing a row");
    rejects(|s| s.replace("\"Q0\", \"Q1\"", "\"Q0\", \"A0\""), "a pin in two groups");
    rejects(|s| s.replace("\"Q0\", \"Q1\"", "\"Q0\", \"CLK\""), "the clock as an output");
    rejects(|s| s.replace("\"min_pulse_width_low\": 4.0", "\"min_pulse_width_low\": 4.5"), "widths over the period");
    rejects(|s| s.replace("\"clock_edge\": \"falling\"", "\"clock_edge\": \"both\""), "an unknown edge");
    rejects(|s| s.replace("[[2.4, 2.5, 2.8]]", "[[-0.1, 2.5, 2.8]]"), "a negative delay");
    rejects(|s| s.replace("\"max_capacitance\": 0.05", "\"max_capacitance\": 0.5"), "max cap past the loads");
    rejects(|s| s.replace("[0.001, 0.01, 0.05]", "[0.01, 0.001, 0.05]"), "loads out of order");
    rejects(|s| s.replace("[[2.5, 2.6, 2.9]]", "[[2.5, 2.6, 8.0]]"), "a launch as long as the period");
    rejects(|s| s.replace("[[0.04, 0.10, 0.35]]", "[[0.04, 0.0, 0.35]]"), "a zero transition");
}

#[test]
fn the_mutants_are_caught() {
    // A check that cannot fail proves nothing: flip each edge and the reading changes.
    let flipped = sram_json().replace("\"clock_edge\": \"falling\"", "\"clock_edge\": \"rising\"");
    let lib = read(&flipped);
    assert_eq!(lib.cells["t_sram"].pins["Q0"].arcs[0].clk_edge, Some(ClkEdge::Rise));
    let flipped = sram_json().replace("\"clock_edge\": \"rising\"", "\"clock_edge\": \"falling\"");
    let lib = read(&flipped);
    assert_eq!(lib.cells["t_sram"].pins["A0"].setup[0].clk_edge, ClkEdge::Fall);
}

#[test]
fn the_rendered_library_matches_its_golden_file() {
    // A regenerated view is compared whole against the reviewed one (OpenRAM keeps golden .lib
    // files the same way): any change to what we write shows up as a diff to review.
    // Regenerate: VYGES_BLESS=1 cargo test --test macro_cell
    let m = MacroCell::from_json(&sram_json()).unwrap();
    let got = render(&m);
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/t_sram__tt.lib");
    if std::env::var_os("VYGES_BLESS").is_some() {
        std::fs::create_dir_all(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden")).unwrap();
        std::fs::write(path, &got).unwrap();
    }
    let want = std::fs::read_to_string(path).expect("golden file present (VYGES_BLESS=1 to create)");
    assert_eq!(got, want, "the macro Liberty changed: review, then re-bless");
}
