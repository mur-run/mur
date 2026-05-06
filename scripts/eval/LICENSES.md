# Upstream benchmark licenses

The B0 eval harness pip-installs two upstream benchmarks. Their
licenses + attribution are reproduced here so users running the
harness see the obligations without leaving the repo.

## AgentDojo

> Apache License 2.0
>
> Copyright 2024 ETH Zürich + Princeton + Tübingen + Anthropic + Meta
>
> Licensed under the Apache License, Version 2.0 (the "License");
> you may not use this file except in compliance with the License.
> You may obtain a copy of the License at
>
>     http://www.apache.org/licenses/LICENSE-2.0
>
> Unless required by applicable law or agreed to in writing, software
> distributed under the License is distributed on an "AS IS" BASIS,
> WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or
> implied. See the License for the specific language governing
> permissions and limitations under the License.

Cite as:

> Debenedetti, Edoardo et al. "AgentDojo: A Dynamic Environment to
> Evaluate Prompt Injection Attacks and Defenses for LLM Agents."
> NeurIPS 2024 (Datasets and Benchmarks Track).

Project: https://github.com/ethz-spylab/agentdojo

## HarmBench

> Creative Commons Attribution 4.0 International (CC-BY-4.0)
>
> You are free to:
>   - Share — copy and redistribute the material in any medium or format
>   - Adapt — remix, transform, and build upon the material
>     for any purpose, even commercially.
>
> Under the following terms:
>   - Attribution — You must give appropriate credit, provide a link
>     to the license, and indicate if changes were made. You may do so
>     in any reasonable manner, but not in any way that suggests the
>     licensor endorses you or your use.

Cite as:

> Mazeika, Mantas et al. "HarmBench: A Standardized Evaluation
> Framework for Automated Red Teaming and Robust Refusal."
> ICML 2024.

Project: https://github.com/centerforaisafety/HarmBench

## How we use these benchmarks

- **No relabelling.** The dataset entries are used verbatim. We do
  not re-categorize attacks or alter the expected outcomes.
- **Subset selection only.** We pick 50 cases per benchmark for the
  v1 acceptance gate; the choice is reproducible via a published
  SHA-256-derived seed (see `README.md` §"Selection seed").
- **Results published per release tag.** Each `eval-results/v<X.Y.Z>.jsonl`
  cites the upstream version that was used + the model that
  generated the responses, so reproducibility chains back to the
  benchmark's own commit hash.

If you redistribute mur with the eval harness disabled
(`--features no-eval`), you don't need to include these licenses; if
the harness is enabled in your build, you do.
