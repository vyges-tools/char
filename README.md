# vyges-char

Standard-cell **timing characterization**: a cell's SPICE netlist in, a Liberty
(`.lib`) timing model out.

> **Vyges open EDA tools.** Commercial-grade silicon sign-off capability, built
> on open standards and plain file formats — and meant to be accessible to
> everyone, not only teams who can license a six-figure tool. `vyges-char`
> opens up standard-cell characterization.

## Why this exists

Timing sign-off needs a timing model for every standard cell — its delay and
output transition as a function of input slew and output load. Foundries ship
these `.lib` files, but you need to (re)generate one whenever you have a new
cell, a new PVT corner, a tweaked transistor, or simply want to **verify** a
vendor library against first-principles SPICE. `vyges-char` produces that model.

## How this is solved today

In production, characterization means Cadence **Liberate**, Synopsys
**SiliconSmart**, or Siemens **Solido** — CCS/ECSM, statistical LVF, full PVT
matrices — the tools foundries and IP teams use to *produce* the libraries that
ship in the PDK. Most design teams never run them; they consume the delivered
`.lib`. In the open world the space is thin (**CharLib**, **LibreCell** over
ngspice/Xyce), so users mostly reuse pre-characterized libraries and skip it.
`vyges-char` makes the generate-and-verify path open and scriptable, behind the
same Liberty file format everything downstream already speaks.

## The problem it solves

Given:

- a cell's **SPICE netlist** (`.subckt …`) and the **PDK device models**, and
- the **slew × load grid**, supply, and temperature to characterize at,

it emits a **Liberty NLDM** (`.lib`): for each timing arc it sweeps input slew
against output load, simulates every point in **ngspice**, measures propagation
delay and output transition, and fills the `cell_rise` / `cell_fall` /
`rise_transition` / `fall_transition` lookup tables.

It reads the cell's **real `.subckt` port order** from the netlist and maps each
pin to the right node — input/output to the driven/measured nets, power/ground
pins to the supplies (sky130 `VPWR`/`VGND`/… handled by default, override with
`power:`/`ground:`). A port it can't place is an error, not a silent float.

## Where it fits in a flow

```text
  PDK cells *.spice ──[ vyges-char + ngspice ]──►  *.lib
  *.v + *.spef + *.lib ──[ STA ]──►  timing sign-off
```

Files in / files out; the simulator is driven as a subprocess. The pure pieces
(Liberty emit, SPICE deck gen, `.measure` parse) run offline and are unit-tested;
only the actual sweep needs ngspice + the PDK on the host.

## Use it

```sh
cargo build --release            # std-only, no external deps

vyges-char run  cell.char -o cell.lib   # characterize (needs ngspice + models)
vyges-char check cell.char              # validate the job, print a summary
vyges-char demo                         # print a sample .lib (no sim)
```

A job (`*.char`) is a few `key: value` lines:

```text
cell:    sky130_fd_sc_hd__inv_1
netlist: sky130_fd_sc_hd.spice          # contains .subckt for the cell
in_pin:  A
out_pin: Y
sense:   negative_unate
slews:   0.01, 0.04, 0.16, 0.64         # ns
loads:   0.0005, 0.002, 0.008           # pF
vdd:     1.8
temp:    25
models:  params.spice, corners/tt.spice # device models, included in order
```

### Running against a real PDK (sky130 example)

The sky130 corner decks use relative `.include` paths and a Monte-Carlo switch
parameter, so:

- prepend a small `params.spice` to `models:` defining
  `.param mc_mm_switch=0` / `.param mc_pr_switch=0`, and
- run from the PDK's `libs.tech/ngspice/corners/` directory so the corner's
  relative includes resolve.

`vyges-char run` then sweeps the grid and writes the `.lib`. Comparing that
output table-by-table against the foundry reference `.lib` is the recommended
way to confirm a characterization is in tolerance.

## Current state (2026-05-30)

v0 emits an **NLDM** (delay + transition lookup tables) from a single-stage
transient deck, and has been **validated against a real PDK**: re-characterizing
`sky130_fd_sc_hd__inv_1` over the foundry reference grid correlates to the
shipped `.lib` to ~13% mean on `cell_rise` (~25% weighted across all four
tables) — the expected gap for a v0 NLDM versus the foundry's CCS sign-off
characterization, and the baseline we improve from.

The road to sign-off grade builds on the same Liberty emitter and job format:
receiver/driver waveform (CCS/ECSM) models, input pin capacitance, statistical
LVF, and multi-arc cells. Same `run` command, no license.
