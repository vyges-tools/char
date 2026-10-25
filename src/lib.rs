//! vyges-char — standard-cell characterization engine.
//!
//! Turns a cell's SPICE netlist + PDK device models into a Liberty (`.lib`)
//! timing model: for each timing arc, sweep input slew x output load, simulate
//! in SPICE, measure delay/transition, and emit NLDM lookup tables.
//!
//! Boundaries (per the Vyges flow architecture): inputs and outputs are files
//! (SPICE netlist + models in, Liberty out); the simulator (ngspice) is driven
//! as a subprocess. The pure pieces here — the Liberty emitter, the SPICE deck
//! generator, and the `.measure` parser — are std-only and unit-tested offline;
//! only the actual sim run needs the EDA environment.

// The crate's own name inside it: the command-line entry ([`cli_main`]) is built into the
// library too, and its `vyges_char::…` paths resolve here.
extern crate self as vyges_char;

pub mod arcs;
pub mod dataset;
pub mod diag;
pub mod engine;
pub mod job;
pub mod liberty;
pub mod library;
pub mod sparse;
pub mod spice;
pub mod surrogate;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const COPYRIGHT: &str = "© 2026 Vyges. All Rights Reserved.  https://vyges.com";

/// The `vyges-char` command-line entry (the binary's `main`), built into the library too so a
/// suite binary can link every engine and run the one it is invoked as.
#[cfg(feature = "suite")]
#[doc(hidden)]
#[path = "main.rs"]
pub mod cli_main;
