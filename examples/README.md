# `vyges-char` examples

| File | What it is |
| --- | --- |
| `inv.char` | The minimal single-cell job (4×4 grid) — `vyges-char run inv.char -o inv.lib`. |
| `inv_sweep.char` | A denser **7×7 log-spaced** grid, sized for the surrogate experiment below. |
| `surrogate-experiment.sh` | Runnable walkthrough: characterize → dataset → surrogate. |

## The surrogate experiment

Characterization's cost is the **SPICE sweep** — one simulation per `(slew, load)` point,
per arc, per corner. But delay and transition are **smooth** functions of slew and load.
So: can you simulate a **subset** of the grid and have a cheap model **predict the rest**,
accurately enough for the fast inner loop? On a plain **CPU — no GPU, no CUDA**.

### Run it (offline first — no PDK, no ngspice)

```sh
./surrogate-experiment.sh
```

This uses built-in synthetic data to show the workflow: a tidy `dataset` table, then a
`surrogate` fit on **half** the grid predicting the held-out half, reported as error vs the
held-out truth (`max%pk` / `rms%pk` = error as a % of the table's peak value). It runs both a
**linear** and a **log** fit so you can compare. (On the synthetic demo surface they're close;
on *real* log-spaced delay data the log fit typically wins by a lot — try it on your cells.)

### Run it on a real cell (needs ngspice + a PDK)

1. Copy `inv_sweep.char` and point `netlist:` / `models:` at your PDK's cell `.spice` and
   device models.
2. Run, giving your PDK's `libs.tech/ngspice/corners` dir (the corner deck uses relative
   `.include` paths, so ngspice must run from there — the script `cd`s for you):

   ```sh
   VYGES_CHAR=../target/release/vyges-char \
     ./surrogate-experiment.sh /abs/path/to/inv_sweep.char /abs/path/to/PDK/.../ngspice/corners
   ```

   *(sky130 tip: also prepend a tiny `params.spice` with `.param mc_mm_switch=0` to
   `models:`, as in the main README's sky130 section.)*

## What do you do with the data?

Two distinct uses:

- **The `dataset` table** (CSV/JSONL) is clean, tidy training/analysis data — one row per
  measured point. Plot delay vs slew/load, diff two corners, or train a model. Non-physical
  artifacts (e.g. a near-zero/negative delay at an extreme corner) are **flagged** in a
  `flag` column; `--clean` drops them.
- **The `surrogate`** answers the question that makes the data *actionable*: **how much of
  the grid is predictable?** If a cheap model nails the held-out points, you don't need to
  simulate them.

### Making the sweep cheaper — the payoff

Today `vyges-char run` still simulates **every** grid point in ngspice. The surrogate is the
evidence for the next step: **simulate a sparse, well-chosen set of points and let the model
fill in the dense grid** — fewer ngspice calls for (nearly) the same `.lib`. Directions to
explore (and tell us about):

- **Coarse-grid + fill:** characterize a 4×4, predict the 7×7 — does it match a true 7×7?
- **Sample-efficiency curve:** plot accuracy vs number of simulated points. Where's the knee?
- **Active sampling:** let the model pick the *next* point to simulate (largest predicted
  uncertainty), instead of a fixed grid.
- **Better models:** splines, RBFs, a tiny neural net; per-metric or per-corner fits.
- **Transfer:** does a model fit at one corner help predict another?
- **Across PDKs / process nodes:** run the same experiment on open PDKs spanning mature to
  advanced nodes — e.g. **gf180** (180 nm), **sky130** (130 nm), and finer / FinFET open
  PDKs such as **ASAP7** (7 nm predictive). Does the surrogate's accuracy and
  sample-efficiency hold as the device physics gets harder (steeper slews, stronger
  Miller / CCS effects, more nonlinearity)? Where does a simple polynomial stop being
  enough, and what model takes over?

We deliberately don't publish headline accuracy numbers — the useful thing is for **you** to
measure it on **your** cells, PDK, and grid.

### Run the cheaper sweeps directly (no code to write)

These are built in — set the knobs, run experiments:

```sh
# parallelize the per-point ngspice sweep (independent points, identical output):
vyges-char run inv_sweep.char --jobs auto            # auto = all cores

# sparse: simulate a coarse 4x4, surrogate-fill the dense .lib; verify on 8 held-out points:
vyges-char run inv_sweep.char --sparse 4x4 --verify 8

# self-tuning: keep sampling the biggest gap until cross-validated error <= 6% of peak:
vyges-char run inv_sweep.char --auto --target 6 --jobs auto
#   -> prints how many points it simulated of the full grid; looser target = fewer sims
```

`--auto` is the "set the accuracy bar, let the tool choose the sampling" mode — the
sample-efficiency experiment with no scripting. Pair any of them with `--jobs` for speed.

`--jobs` works on **every** run (combinational and sequential). `--sparse` also works on
**plain D-flops** — it surrogate-fills the CK→Q arc and the setup/hold tables from a coarse
grid (`--auto` and async set/reset flops are combinational / dense-only for now).

## Who this is for — two cohorts, two purposes

The tool is **open-source and runs locally** (std-only Rust + ngspice on a plain CPU — no
GPU, no CUDA, and **nothing leaves your machine**), so it suits very different teams:

- **University / open-source researchers** — treat it as a study in sample-efficiency and
  surrogate modeling: the open questions above (how few SPICE points, which models, which
  cells/corners/nodes). Open PDKs (gf180, sky130, ASAP7) need no agreement. Publish and
  share what you find.

- **Enterprise / commercial silicon teams** — use it to **evaluate the payoff on _your_
  PDK and _your_ node** — 28 nm, 12 nm, 3 nm, whatever you run. Point it at your licensed
  or NDA models, characterize a few representative cells, and measure for yourself: *how
  much characterization runtime would `--sparse` / `--auto` save my team, at an accuracy my
  flow can accept?* Because the engine is open and local, you can answer that on your
  confidential PDK without sending models or results anywhere. (For sign-off-grade
  libraries on a commercial node, the silicon-correlated calibration ships as a separate
  per-foundry plugin — see "Open core, certified fab plugins" in the main README.)

**Tell us what you find.** Results, surprises, "it worked / it didn't on node X", or a
conversation about your team's flow — start at <https://vyges.com/contact>. Findings from
either cohort may shape where this goes.
