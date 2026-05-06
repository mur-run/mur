# mur Makefile — primarily a launcher for the B0 eval harness (M11.5)
# and a shortcut for the existing build / install / release flows.
# CLI is the source of truth; this is convenience.

.PHONY: help build install release eval-stub eval-release eval-clean

EVAL_OUT := eval-out
EVAL_JSONL := $(EVAL_OUT)/run.jsonl
EVAL_REPORT := $(EVAL_OUT)/report.md

help:
	@echo "mur — top-level make targets"
	@echo
	@echo "  build           Same as ./build.sh (release with embedded dashboard)"
	@echo "  install         Same as ./build.sh --release --install"
	@echo "  release         Build then run release-gate eval (real LLM)"
	@echo
	@echo "  eval-stub       Run B0 eval against the deterministic stub LLM"
	@echo "                  (free; gates Test/Clippy/etc on every PR)"
	@echo "  eval-release    Run B0 eval against Anthropic Sonnet (~\$$5)"
	@echo "                  Requires \$$ANTHROPIC_API_KEY in env."
	@echo "  eval-clean      Remove $(EVAL_OUT)/ and eval-results/"
	@echo
	@echo "Spec: docs/superpowers/specs/2026-05-06-b0-m11-eval-harness-design.md"

build:
	./build.sh

install:
	./build.sh --release --install

# ── B0 eval-harness targets ─────────────────────────────────────────
# `eval-stub` matches the CI workflow's stub job. Useful locally for:
#   - sanity-checking the runner skeletons before opening a PR
#   - debugging a JSONL schema mismatch
#   - reproducing a CI failure (output is byte-identical)
eval-stub:
	@mkdir -p $(EVAL_OUT)
	@rm -f $(EVAL_JSONL)
	python3 scripts/eval/agentdojo/run.py --out $(EVAL_JSONL)
	python3 scripts/eval/harmbench/run.py --out $(EVAL_JSONL)
	cargo run -q -p mur-core --bin mur -- agent eval report \
		--jsonl $(EVAL_JSONL) \
		--out $(EVAL_REPORT)
	@echo
	@cat $(EVAL_REPORT)

# `eval-release` matches the CI weekly cron + per-tag job. Costs
# real money; keeps a 30-min hard wall-clock cap via the underlying
# Python harness's per-test timeout (M11.2.1 + M11.3.1).
eval-release:
	@if [ -z "$$ANTHROPIC_API_KEY" ]; then \
		echo "ERROR: ANTHROPIC_API_KEY unset; refusing to run real-LLM eval"; \
		exit 1; \
	fi
	@mkdir -p $(EVAL_OUT)
	@rm -f $(EVAL_JSONL)
	python3 scripts/eval/agentdojo/run.py \
		--backend anthropic --model claude-sonnet-4-6 \
		--out $(EVAL_JSONL)
	python3 scripts/eval/harmbench/run.py \
		--backend anthropic --model claude-sonnet-4-6 \
		--out $(EVAL_JSONL)
	cargo run -q -p mur-core --bin mur -- agent eval report \
		--jsonl $(EVAL_JSONL) \
		--out $(EVAL_REPORT)
	@echo
	@cat $(EVAL_REPORT)

eval-clean:
	rm -rf $(EVAL_OUT) eval-results/*.jsonl

release: build eval-release
