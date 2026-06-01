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

pub mod job;
pub mod liberty;
pub mod spice;
pub mod engine;
pub mod library;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
