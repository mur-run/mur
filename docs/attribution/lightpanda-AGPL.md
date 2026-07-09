# Third-Party Attribution: Lightpanda

`mur-research-gateway`'s tier-2 fetch/search path optionally renders pages
using [Lightpanda](https://github.com/lightpanda-io/browser), a headless
browser engine.

- **License**: AGPL-3.0 (see the upstream repository for the full text).
- **Usage**: Lightpanda is invoked **arm's-length as a separate OS
  subprocess**, driven via the `agent-browser` CLI (Apache-2.0). MUR never
  links the Lightpanda library into its own process, and does not modify
  Lightpanda's source — it ships and runs the upstream binary unmodified.
- **Why this matters**: AGPL-3.0's network-copyleft clause is triggered by
  combining/linking AGPL code into a program; invoking an unmodified AGPL
  binary as an independent subprocess over a process boundary does not bring
  MUR's own source under AGPL.
- **Fallback**: if Lightpanda is not installed, `mur-research-gateway`
  degrades to tier-1 (plain HTTP fetch) and tier-3 (Chrome via
  `agent-browser --engine chrome`); Lightpanda is not a hard dependency.

Upstream source: <https://github.com/lightpanda-io/browser>
