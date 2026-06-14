#!/usr/bin/env bash
# Use-vs-no-use compression benchmark. Drives the real `mur compress` CLI over a
# real + synthetic corpus, verifies reversibility, and writes results.json.
#
# The CLI calls CompressEngine::compress directly (no min_tokens auto-gate), so
# this measures the engine's per-content-type capability — the upper bound the
# auto surfaces draw on. Requires `jq` and `rg`.
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"
MUR="${MUR:-$ROOT/target/debug/mur}"
OUT="${OUT:-$ROOT/target/compress-bench}"
CORPUS="$OUT/corpus"
mkdir -p "$CORPUS"
: > "$OUT/rows.ndjson"

command -v jq >/dev/null || { echo "jq required" >&2; exit 1; }
[ -x "$MUR" ] || { echo "build mur first: cargo build -p mur-core" >&2; exit 1; }

echo "Generating corpus..."
# ---- real corpus ---------------------------------------------------------
rg --line-number "fn |pub |impl " mur-core/src mur-compress/src 2>/dev/null | head -3000 > "$CORPUS/ripgrep-search.txt" || true
git log -p -n 5 > "$CORPUS/git-history.diff" 2>/dev/null || true
# cargo metadata is a top-level OBJECT (would only minify); extract the packages
# array so we feed a top-level ARRAY (what the JSON compressor offloads).
cargo metadata --format-version=1 2>/dev/null | jq '.packages' > "$CORPUS/cargo-packages.json" 2>/dev/null || true
"$MUR" project search "compression engine" --json > "$CORPUS/mur-projsearch.json" 2>/dev/null || true

# ---- synthetic corpus ----------------------------------------------------
yes "2026-06-14T10:00:01Z DEBUG cache hit for key=session-token-abc123 worker=7" | head -5000 > "$CORPUS/repetitive.log" || true
{ printf '['; for i in $(seq 1 5000); do printf '{"id":%d,"name":"item-%d","v":%d,"ok":true}' "$i" "$i" $((i*7)); [ "$i" -lt 5000 ] && printf ','; done; printf ']'; } > "$CORPUS/huge-array.json"
head -c 30000 /dev/urandom | base64 > "$CORPUS/dense-incompressible.txt"   # passthrough proof

echo "Measuring (mur compress + reversibility check)..."
set +e   # parsing below is best-effort; a non-matching grep must not abort the run
for f in "$CORPUS"/*; do
  [ -s "$f" ] || continue
  name="$(basename "$f")"
  bytes="$(wc -c < "$f" | tr -d ' ')"
  err="$("$MUR" compress --file "$f" 2>&1 >/dev/null)"   # stats line on stderr
  orig="$(printf '%s' "$err" | grep -oE '\[[0-9]+' | head -1 | tr -d '[')"
  comp="$(printf '%s' "$err" | grep -oE -- '-> [0-9]+' | head -1 | grep -oE '[0-9]+')"
  saved="$(printf '%s' "$err" | grep -oE '\([0-9.]+% saved' | head -1 | grep -oE '[0-9.]+')"
  hash="$(printf '%s' "$err" | grep -oE 'hash=[a-f0-9]+' | head -1 | cut -d= -f2 || true)"
  ok=true
  if [ -n "${hash:-}" ]; then
    # $(...) strips the trailing newline println! adds, so the compare is exact.
    [ "$(cat "$f")" = "$("$MUR" retrieve "$hash" 2>/dev/null)" ] || ok=false
  fi
  jq -nc --arg n "$name" --argjson b "${bytes:-0}" --argjson o "${orig:-0}" \
        --argjson c "${comp:-0}" --arg s "${saved:-0}" --arg h "${hash:-}" --argjson ok "$ok" \
    '{name:$n, bytes:$b, orig_tokens:$o, comp_tokens:$c, saved_pct:($s|tonumber),
      offloaded:($h|length>0), reversible:$ok}' >> "$OUT/rows.ndjson"
done
set -e

# ---- aggregate -----------------------------------------------------------
jq -s '{rows: ., totals: {orig_tokens:(map(.orig_tokens)|add), comp_tokens:(map(.comp_tokens)|add)}}
       | .totals.saved_pct = (if .totals.orig_tokens>0
            then (100*(.totals.orig_tokens-.totals.comp_tokens)/.totals.orig_tokens) else 0 end)' \
   "$OUT/rows.ndjson" > "$OUT/results.json"

echo
printf 'corpus item\torig_tok\tcomp_tok\tsaved\toffload\trevers\n'
jq -r '.rows[] | [.name, .orig_tokens, .comp_tokens, ((.saved_pct|tostring)+"%"),
                  (if .offloaded then "yes" else "-" end),
                  (if .reversible then "ok" else "FAIL" end)] | @tsv' "$OUT/results.json"
jq -r '.totals | ["TOTAL", .orig_tokens, .comp_tokens, ((.saved_pct|floor|tostring)+"%"), "", ""] | @tsv' "$OUT/results.json"
echo
echo "Wrote $OUT/results.json"
