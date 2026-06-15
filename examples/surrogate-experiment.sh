#!/usr/bin/env bash
# Surrogate experiment: characterize a cell, export the dataset, and measure how well a
# small CPU model predicts the held-out grid points — no GPU, no CUDA.
#
#   Offline (no PDK, no ngspice) — built-in synthetic demo:
#       ./surrogate-experiment.sh
#
#   Real cell (needs ngspice + the netlist/models your JOB points at):
#       ./surrogate-experiment.sh /abs/path/to/cell.char
#       ./surrogate-experiment.sh /abs/path/to/cell.char /abs/path/to/pdk/.../ngspice/corners
#
# The 2nd arg is your PDK's `libs.tech/ngspice/corners` dir: sky130 (and most PDKs) use
# relative .include paths in the corner deck, so ngspice must run from there. The script
# cd's into it for you; pass the JOB as an ABSOLUTE path in that case.
#
# Override the binary with VYGES_CHAR=/path/to/vyges-char (default: vyges-char on PATH).
set -euo pipefail

BIN="${VYGES_CHAR:-vyges-char}"
JOB="${1:-}"
CORNERS="${2:-}"

if [ -z "$JOB" ]; then
  echo "## Offline demo (synthetic grid — no SPICE, no PDK)"
  echo
  echo "# dataset (tidy table, first rows):"
  "$BIN" dataset | head -5
  echo
  echo "# surrogate, LINEAR fit (poor — the grid is log-spaced):"
  "$BIN" surrogate
  echo
  echo "# surrogate, LOG fit (the right default for log-spaced grids):"
  "$BIN" surrogate --log
  echo
  echo "Next: run on a real cell ->  $0 /abs/path/to/cell.char [/abs/path/to/PDK/.../ngspice/corners]"
  echo "Share what you find:        https://vyges.com/contact"
  exit 0
fi

# Absolutize the JOB and pick an output location BEFORE we change directory.
JOB="$(cd "$(dirname "$JOB")" && pwd)/$(basename "$JOB")"
OUT="$(pwd)/$(basename "${JOB%.char}")_dataset.csv"
[ -n "$CORNERS" ] && cd "$CORNERS"

echo "## Real characterization: $(basename "$JOB")"
echo
echo "# 1) export the full grid as a dataset -> $OUT"
"$BIN" dataset "$JOB" -o "$OUT"
echo
echo "# 2) how well does a CPU surrogate predict the held-out half of the grid?"
echo "#    (max%pk / rms%pk = error as a percentage of the table's peak value)"
"$BIN" surrogate "$JOB" --log
echo
echo "Try: --degree 1|2|3, --metric cell_rise, drop --log to see why log space matters."
echo "Share results & ideas: https://vyges.com/contact"
