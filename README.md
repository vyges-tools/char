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

## When & how to use it in your flow

```text
  PDK cell *.spice + device models ─[vyges-char + ngspice]─► *.lib ─► STA
```

Reach for it when you need a cell's timing model and don't already have a
trustworthy one — a **custom or ECO cell**, a **new PVT corner**, or to
**verify** a vendor `.lib` against first-principles SPICE. It runs **after** you
have the cell netlist + device models and **before** STA, which cannot run
without a `.lib`. The Liberty it emits is exactly what `vyges-sta-si` (or
any STA tool) consumes. Most flows consume the foundry's shipped `.lib`
directly and only reach for `vyges-char` to fill those gaps.

## Use it

```sh
# prebuilt binaries: dist/<triple>/vyges-char  (or build it yourself:)
cargo build --release            # std-only, no external deps

vyges-char run  cell.char -o cell.lib   # characterize (needs ngspice + models)
vyges-char run  cell.char --json        # machine-readable summary instead of Liberty
vyges-char check cell.char              # validate the job, print a summary
vyges-char demo                         # print a sample .lib (no sim)
# common flags: -o FILE · --json · -q/--quiet · -v/--verbose · -h/--help · -V/--version
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
montecarlo: 8                           # optional: LVF sigma (omit/0 = NLDM only)
ccs:     true                           # optional: emit CCS output-current waveforms
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

## Open core, certified fab plugins

`vyges-char` is open and contains **no foundry-confidential data**. It runs out
of the box on open PDKs (sky130, gf180) using their published device models.

```text
  vyges-char — OPEN engine  (Apache-2.0, contains no fab data)
  ────────────────────────────────────────────────────────────────────
    cell .subckt  ─►  job.rs ─► engine.rs ─(ngspice)─► liberty.rs  ─►  *.lib
                         ▲
                         └─ published plugin contract
                            (device models · corner · slew×load grid)
                                       │
                 loads ONE characterization plugin
                                       │
        ┌──────────────────────────────┴──────────────────────────────┐
        │                                                              │
  OPEN reference plugin                          CERTIFIED per-fab plugins
  (in-repo · no NDA)                             (private · one per fab/node 🔒)
    • sky130A models + tt corner                   • vyges-char-tsmc28
      ✓ M0/M3 validated                            • vyges-char-sec28
                                                    • vyges-char-micron…
   open data, ships with the tool                correlated corner +
                                                  reference .lib — under NDA
```

**sky130A is the starter / reference plugin** — open, no NDA, and already proven
by the M3 run (re-characterized `inv_1` against the shipped sky130 `.lib`). Today
a "plugin" is just the models + corner setup you pass on the CLI; formal per-fab
plugin packaging (discovery, signing, repo-per-fab) is the remaining open item.

Getting *sign-off-grade* libraries on a **commercial** node takes two things
beyond the tool running: the output must be **correlated to that foundry's
silicon**, and the foundry must **accept the flow under an agreement**. Both live
in a **separate, per-foundry plugin** — never in this repository:

- the open tool defines a published **characterization contract** (the job +
  models/corner setup and its calibration extensions);
- a **certified per-foundry plugin** supplies the silicon-correlated corner setup
  and reference for a specific node, delivered **under that foundry's NDA**;
- the open engine loads it through the contract and never embeds or references
  any foundry-confidential infrastructure. Each foundry has its own plugin.

So the **engine and the contract are open for everyone**, while the **per-foundry
correlation is gated** to those with the agreement — the same way a commercial
characterizer separates its engine from the foundry-delivered calibration, except
here the engine is open. Use `vyges-char` today on open PDKs and to
characterize/verify custom cells on any PDK you have; certified sign-off
libraries on a commercial node come with that node's plugin.

## Current state (2026-05-31)

Emits an **NLDM** (delay + transition lookup tables) from a single-stage
transient deck, **validated against a real PDK**: re-characterizing
`sky130_fd_sc_hd__inv_1` over the foundry reference grid correlates to the
shipped `.lib` to ~13% mean on `cell_rise` (~25% weighted across all four
tables) — the expected gap for NLDM versus the foundry's CCS sign-off
characterization.

Adds **LVF (statistical OCV)**: with `montecarlo: N`, each (slew,load) point runs
N seeded Monte-Carlo samples over device **mismatch** (`mc_mm_switch`) and emits
`ocv_sigma_cell_rise/fall` delay-sigma tables alongside the NLDM — **exactly the
tables `vyges-sta-si` consumes for POCV**, closing the loop `char → .lib → sta-si`.
Zero-cost when `montecarlo` is unset (NLDM-only).

Adds **CCS (composite current source)**: with `ccs: true`, each (slew,load) point
captures the driver's **output-current waveform** — a 0 V sense source in series
with the load lets the transient dump `i(out)` over a fine step tightened to the
switching window — and emits `output_current_rise/fall` vector groups (per-edge
`reference_time` + time/current sampled to a compact vector). These are **the
current-source models `vyges-sta-si` drives into its effective-capacitance (Ceff)
and transient RC-tree solve**, the other half of the `char → .lib → sta-si` loop
beyond LVF. Validated end-to-end on `sky130_fd_sc_hd__inv_1`: the captured charge
spike peaks ~0.12 mA for a few-fF load (physically sane), and `sta-si` consuming
the CCS `.lib` shifts WNS by a sensible CCS-vs-NLDM delta. Zero-cost when `ccs` is
unset.

The road to sign-off grade builds on the same emitter + job format: **receiver**
pin-capacitance models (`receiver_capacitance`), and multi-arc / sequential cells.
Same `run` command, no license.
