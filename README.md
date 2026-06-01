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

vyges-char run     cell.char -o cell.lib   # characterize one cell (needs ngspice + models)
vyges-char run     cell.char --json        # machine-readable summary instead of Liberty
vyges-char library lib.charlib -o out -v   # characterize many cells in parallel -> merged .lib
vyges-char check   cell.char               # validate the job, print a summary
vyges-char demo                            # print a sample .lib (no sim)
# common flags: -o FILE · --json · -q/--quiet · -v/--verbose · -h/--help · -V/--version
```

### Library scale (`library`)

A whole library is just many per-cell jobs run together. A `.charlib` manifest
names them (or a directory of them) and a thread count; cells are characterized
**in parallel** — each `ngspice` point is a subprocess, so the simulator is the
bottleneck and the pool scales the run across cores — then merged per corner into
a single `.lib` (one shared header, the union of lookup-table templates, every
cell group). A cell that fails to characterize is reported and dropped, never
sinking the whole library.

```text
library:  sky130_fd_sc_hd_subset
threads:  12                      # default: available parallelism
jobs_dir: cells                   # every *.char in a dir (or `jobs: a.char, b.char`)
```

`vyges-char library lib.charlib -o out/` writes `out/<library>.lib` (or one
`out/<library>__<corner>.lib` per corner). Mixed combinational + sequential cells
merge into one well-formed library.

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
recv:    true                           # optional: emit CCS receiver capacitance (input pin)
power_char: true                        # optional: leakage_power + internal_power
```

**Multi-arc cells** (multi-input gates, multi-output cells) replace the single
`in_pin`/`out_pin`/`sense` with one `arc:` line per timing arc — `<in> <out> <sense>
[side=0|1 ...]`, where each *other* input is held at its non-controlling value while
this arc is exercised. A 2-input NAND:

```text
cell:    sky130_fd_sc_hd__nand2_1
netlist: sky130_fd_sc_hd.spice
slews:   0.05, 0.20
loads:   0.001, 0.005
vdd:     1.8
arc:     A Y negative_unate B=1         # A->Y, side input B held high
arc:     B Y negative_unate A=1         # B->Y, side input A held high
```

All arcs of one cell render into a single `cell {}` (one `pin` per input, one per
output, a `timing ()` group per arc) — and the A->Y vs B->Y delays come out
distinct (the series-stack input nearer the output switches faster), which is
exactly why each arc must be characterized, not copied.

**Sequential cells** (flip-flops) characterize the setup/hold constraints on the
data pin and the CK->Q delay arc instead of combinational arcs:

```text
cell:       sky130_fd_sc_hd__dfxtp_1
netlist:    sky130_fd_sc_hd.spice
clock_pin:  CLK
data_pin:   D
out_pin:    Q
clock_edge: rising                      # rising | falling
slews:      0.05, 0.20                   # clock & data transition axes
loads:      0.005                        # Q load (CK->Q arc)
vdd:        1.8
```

Setup/hold are found per (clock slew, data slew) grid point by a **push-out
bisection**: the data-to-clock separation is squeezed until the CK->Q delay
degrades 10% past its stable value. The emitted `.lib` is a full sequential cell
(`ff` group, `clock : true`, `setup_*`/`hold_*` constraint groups, edge-triggered
CK->Q arc) — the exact shape `vyges-sta-si` reads for reg-to-reg timing.

**Per-corner sweeps** characterize the cell across PVT corners — one `corner:` line
per (process models, supply, temperature) — emitting one `.lib` per corner, the
per-corner library set `vyges-sta-si`'s MCMM consumes:

```text
cell:    sky130_fd_sc_hd__inv_1
netlist: sky130_fd_sc_hd.spice
in_pin:  A
out_pin: Y
slews:   0.05, 0.20
loads:   0.001, 0.005
# corner:  name | models (csv) | vdd [| temp]
corner:  ss_n40C_1v60 | params_ss.spice, corners/ss.spice | 1.60 | -40
corner:  tt_025C_1v80 | params_tt.spice, corners/tt.spice | 1.80 | 25
corner:  ff_125C_1v95 | params_ff.spice, corners/ff.spice | 1.95 | 125
```

`vyges-char run job.char -o <dir>` then writes `<cell>__<corner>.lib` per corner
(nominal voltage/temperature in each header). Without `corner:` lines the job is a
single run to stdout / `-o FILE` as before — fully back-compatible.

**Async set/reset flops** add a `tie:` list (pins held at their inactive level
during setup/hold/CK->Q) and an optional `reset_pin:`; the reset's active level is
inferred from the name (`_B`/`_N` → active-low) or set with `reset_active:`:

```text
cell:       sky130_fd_sc_hd__dfrtp_1
netlist:    sky130_fd_sc_hd.spice
clock_pin:  CLK
data_pin:   D
out_pin:    Q
reset_pin:  RESET_B          # async reset; held inactive (high) for setup/hold/CK->Q
slews:      0.10
loads:      0.005
vdd:        1.8
```

The emitted `.lib` gains the `ff` `clear : "!RESET_B"` attribute, an async
**reset->Q delay arc** (`timing_type : clear`), and the **recovery/removal**
constraints (the async de-assert-vs-clock timing, found by bisecting the release
edge for the capture/hold boundary). An async **set** uses `set_pin:` symmetrically
— `ff preset` + set->Q arc + recovery/removal; a flop can carry both. Extra unused
inputs (e.g. scan controls) go in `tie:` as `SCE=0, SCD=1`.

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
transient deck, **correlated cell-by-cell against the foundry `.lib`** on the
exact reference grid. The correlation surfaced a real bug: `index_1`
(input_net_transition) is the input edge measured between the **20–80% slew
thresholds**, but the deck drove a full-swing ramp over `slew_ns` — making every
input ~1.67× too steep and biasing delays/transitions low. Fixed (ramp spans
`slew_ns / 0.6`): the **rise arcs now correlate to single digits** (inv_2
`cell_rise` 7%, `rise_transition` 6%) and the weighted error dropped from ~25% to
~13–20%. The fall-arc residual was then chased to ground and **clears char**:
re-deriving the worst and cleanest grid points with independent hand-written
ngspice decks shows char reproduces clean ngspice **to 4 significant figures**, so
the gap is not a char defect. It is (a) a clean ~15% **ngspice-vs-shipped-vendor-`.lib`
floor** at large load (a symmetric rise-slow/fall-fast P/N drive-strength skew that
raw ngspice also shows — a known sky130 re-characterization gap) and (b) an **NLDM
small-load degeneracy** (slow input + tiny load trips the gate before input-50%, so
the measured delay is near-zero/negative — physically real, and raw ngspice does the
same). We did not fudge the device model to chase the vendor number.
See the strategy repo's `char-foundry-correlation.md`.

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

Adds **CCS receiver capacitance**: with `recv: true`, each (slew,load) point drives
the input pin through a 0 V sense source and integrates the captured input current
Q = ∫i·dt over the two halves of the input ramp → the two-segment receiver model
`receiver_capacitance1/2_rise/fall` (C1 = static gate cap before the delay
threshold; C2 = after, inflated by Miller from the switching output). The input pin
also gains the conventional single-number `capacitance` (the C1 lanes). These are
**the input-pin load `vyges-sta-si` charges its drivers with** — completing the CCS
model (output current + receiver). Validated on `sky130_fd_sc_hd__inv_1`: C1/C2
land ~1.8–2.6 fF (matching the foundry input-cap), with C2 inflating over C1 (e.g.
1.44×) exactly when the output switches during the input's second half; sta-si
consuming the receiver load shifts WNS by a sensible Miller delta. Zero-cost when
`recv` is unset.

Adds **multi-arc cells**: one `arc:` line per timing arc, each holding the other
inputs at their non-controlling level via a fixed source, so multi-input gates
(NAND/NOR/AND/OR/MUX) and multi-output cells characterize every arc and render into
one well-formed `cell {}`. Validated on `sky130_fd_sc_hd__nand2_1`: both A->Y and
B->Y arcs emit, with the expected series-stack asymmetry (~25% at the first grid
point), and the two-arc `.lib` round-trips through `vyges-sta-si` (worst path picks
the slower arc).

Adds **sequential (flip-flop) characterization**: `clock_pin`/`data_pin` switch the
job into setup/hold + CK->Q mode, with a push-out bisection (10% CK->Q degradation)
per (clock slew, data slew) point and a small series resistor on every source to
keep the flop's storage-node feedback converging in ngspice. Validated on
`sky130_fd_sc_hd__dfxtp_1`: CK->Q ~0.2 ns, setup ~0.04-0.08 ns, and the
characteristic **negative hold** — all physically sane — and the generated flop
`.lib` round-trips through `vyges-sta-si`, which times a reg-to-reg path from it
(setup WNS + hold WHS, the negative hold relaxing the hold check).

Adds **per-corner sweeps**: `corner:` lines characterize the cell across PVT
corners (process models + supply + temperature), one `.lib` per corner with the
corner's nominal V/T in the header. Validated on `sky130_fd_sc_hd__inv_1` across
ss/tt/ff: the cell_rise delays order ff (0.019 ns) < tt (0.029) < ss (0.053) as
physics demands, and `vyges-sta-si` MCMM across the three generated libs binds the
worst setup at the slow (ss) corner — closing char → per-corner `.lib`s → MCMM.

Adds **async set/reset flops**: a `tie:` list holds async/unused inputs at their
inactive level (through a series R, same de-stiffening) so setup/hold/CK->Q
characterize normally, and `reset_pin:`/`set_pin:` emit the `ff` `clear`/`preset`
attribute plus an async reset->Q (`clear`) / set->Q (`preset`) delay arc. Validated
on `sky130_fd_sc_hd__dfrtp_1` (reset->Q ~0.149 ns, recovery/removal -0.15/+0.15 ns)
and `dfstp_1` (set->Q ~0.221 ns): clocked timing matches the plain dfxtp_1, and both
flop `.lib`s round-trip through `vyges-sta-si` (reg-to-reg setup+hold timed, the
async `clear`/`preset` and recovery/removal correctly skipped, not mistaken for data
paths). **Recovery/removal** bisect the async release edge for the single
capture/hold boundary `t*` relative to the clock (recovery = clock - t*, removal =
t* - clock, both signed — a flop that samples just after the clock 50% tolerates a
late release, giving a small negative recovery). The setup/hold **push-out
bisection** early-exits at 1 ps precision (~halving the ngspice runs per point).

Adds **power characterization** (`power_char: true`): per-arc **internal_power**
(rise/fall switching energy = supply energy minus the load-charging part) and
per-input-state **leakage_power** (DC quiescent current × VDD), with the
`leakage_power_unit` header and a `cell_leakage_power` average. Validated on
`sky130_fd_sc_hd__inv_1`: cell_leakage_power 0.0043 nW vs the foundry 0.0053 nW
(~19%) and internal energy ~0.007 pJ — right magnitudes and units. This is the
power data `vyges-em-ir` will drive its dynamic IR analysis with. (v1 caveat: the
per-state leakage *spread* is narrower than the foundry's N/P asymmetry — a true
DC `.op` settle would sharpen it; the average correlates well.)

The road to sign-off grade builds on the same emitter + job format: sequential
power (clock/data pin energy), sharper per-state leakage, multi-bit / latch cells,
and a two-sided recovery/removal window. Same `run` command, no license.
