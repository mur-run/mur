# mur-compress

Reversible, content-aware token compression for MUR. Shrinks the bulk machine
text an LLM reads — search dumps, build logs, git diffs, large JSON — and offloads
the original to a local store keyed by hash.

> Design inspiration: [headroom](https://github.com/chopratejas/headroom) (Apache-2.0).
> Clean-room reimplementation — no headroom source is copied.

## What it does

A two-stage pipeline (reformat → offload) with content-type routing
(search / log / diff / json / generic). A result is only kept if it actually pays
off (the worth-it gate), so compression never inflates output.

## Manual use (MCP tools)

- `mur_compress(content, query?)` → `{ compressed, hash, … }`
- `mur_retrieve(hash, query?)` → full original, or BM25-filtered items when `query` is given
- `mur_compress_stats()` → cumulative tokens/cost saved

CLI equivalents: `mur compress [file] [--query q]`, `mur retrieve <hash> [--query q]`.

## Automatic use (`auto:`)

MUR auto-compresses large outputs on two LLM-facing surfaces — MCP tool results
(`mur-mcp-server`) and agent-runtime tool outputs (`mur-agent-runtime`) — gated by
`~/.mur/compress.yaml`:

```yaml
auto:
  enabled: true        # master switch
  min_tokens: 800      # outputs smaller than this are never auto-compressed (floor: 500)
  mcp: true            # MCP tool outputs
  agent_runtime: true  # agent post_tool_use outputs
  claude_hook: true    # Claude Code PostToolUse hook stdout replacement
```

When fired, the bulky part of the result becomes `{ compressed:true, content, hash, note }`;
the `note` tells the reader how to `mur_retrieve` the original. The compressor only
collapses a *top-level* array, so for object results MUR compresses the largest
array-valued field and leaves scalar fields intact. See the `mur-compress` skill
for the agent-facing guide.
