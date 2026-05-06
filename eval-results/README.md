# B0 acceptance baseline runs

This directory holds the JSONL outputs from per-release-tag real-LLM
runs of the B0 acceptance evaluation harness (M11). Each
`v<X.Y.Z>.jsonl` file is the canonical baseline for that mur tag.

Workflow (M11.6):

1. Tag a release (`git tag -a v<X.Y.Z>` + push).
2. `.github/workflows/eval.yml::real-llm` fires on the `v*` tag,
   runs against `claude-sonnet-4-6` at `temperature=0`, and commits
   the resulting JSONL here under `eval-results/<tag>.jsonl`.
3. The commit is annotated `eval: baseline for <tag>`.
4. To re-render any historical run's markdown report:
   ```bash
   mur agent eval report --jsonl eval-results/v<X.Y.Z>.jsonl --out -
   ```

A baseline is **never edited in place**. Drift in the protection
numbers across releases is the audit signal — re-running an old tag
should produce byte-identical JSONL because both the seed and the
upstream package versions are pinned.

See `docs/cookbook/b0-eval.md` for the full evaluation workflow.
