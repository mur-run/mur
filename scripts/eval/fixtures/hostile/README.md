# Hostile Fixture Corpus

20 character cards and 10 MCP manifests containing real-world injection vectors.
Used by the B2 nightly eval and cargo-fuzz harness.

| Range | Attack vector |
|-------|---------------|
| cards/hostile_01–05 | name / first_mes / personality / Unicode tags / extensions.mur |
| cards/hostile_06–10 | description / scenario / creator_notes / tags / mes_example |
| cards/hostile_11–15 | YAML bombs (deep nesting, aliases, long strings) |
| cards/hostile_16–20 | BIDI override, zero-width joiners, homoglyphs |
| mcp/hostile_01–05 | Tool description injection, name injection, $ref abuse |
| mcp/hostile_06–10 | DoS (large schema), circular refs, BIDI, tool-count explosion |

Validate: `python scripts/eval/fixtures/hostile/validate.py`
