#!/usr/bin/env bash
set -uo pipefail

# Resolve paths relative to this script so it runs anywhere (CI, any checkout).
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"  # absolute dir of this script
ROOT="$(dirname "$HERE")"                             # repo root (parent of difftest/)

# BIN defaults to the release build; override with OXIDIL_BIN for debug/other.
BIN="${OXIDIL_BIN:-$ROOT/target/release/oxidil}"
CORPUS="$HERE/corpus"
OUT="$HERE/out"
NODE="$(command -v node)"

if [[ ! -x "$BIN" ]]; then
  echo "error: oxidil binary not found at '$BIN' (build it or set OXIDIL_BIN)" >&2
  exit 2
fi
if [[ -z "$NODE" ]]; then
  echo "error: node not found on PATH" >&2
  exit 2
fi

rm -rf "$OUT"; mkdir -p "$OUT"

LEVELS=(O0 O1 O2 O3 Os)
FAILS=0

# Compile $f with $flags to $outjs, run it through node, and set the globals
# RUN_OUT (combined stdout+stderr) and RUN_RC (exit code).
run_label() {
  local f="$1" outjs="$2" flags="$3" tag="$4"
  local compile_err="$OUT/${tag}.compile.err"
  local nout="$OUT/${tag}.node.out"
  if ! eval "\"$BIN\" \"$f\" --out \"$outjs\" $flags" 2>"$compile_err"; then
    RUN_OUT="<<COMPILE-FAIL>> $(cat "$compile_err")"
    RUN_RC=255
    return
  fi
  "$NODE" "$outjs" >"$nout" 2>&1
  RUN_RC=$?
  RUN_OUT="$(cat "$nout")"
}

for f in "$CORPUS"/*; do
  name="$(basename "$f")"
  ext="${name##*.}"
  base="${name%.*}"
  isTs=0
  [[ "$ext" == "ts" || "$ext" == "tsx" ]] && isTs=1

  # build the list of (label, flags) to run; O0 is always first (the baseline).
  labels=(); flagsets=()
  for L in "${LEVELS[@]}"; do
    labels+=("$L"); flagsets+=("-$L")
  done
  if [[ $isTs -eq 1 ]]; then
    for L in O1 O2 O3 Os; do
      labels+=("$L+tsof"); flagsets+=("-$L --ts-typeof")
    done
  fi

  baseout=""; baserc=""
  for ((i = 0; i < ${#labels[@]}; i++)); do
    lab="${labels[i]}"
    flags="${flagsets[i]}"
    run_label "$f" "$OUT/${base}.${lab}.js" "$flags" "${base}.${lab}"

    if [[ "$lab" == "O0" ]]; then
      baseout="$RUN_OUT"; baserc="$RUN_RC"
      continue
    fi
    if [[ "$RUN_OUT" != "$baseout" || "$RUN_RC" != "$baserc" ]]; then
      echo "=========================================="
      echo "DIVERGENCE: $name  level=$lab"
      echo "--- O0 (rc=$baserc) ---"
      echo "$baseout"
      echo "--- $lab (rc=$RUN_RC) ---"
      echo "$RUN_OUT"
      echo ""
      FAILS=$((FAILS + 1))
    fi
  done
done

echo "##### TOTAL DIVERGENCES: $FAILS #####"

# Non-zero exit on any divergence so CI fails the job.
[[ $FAILS -eq 0 ]] || exit 1
