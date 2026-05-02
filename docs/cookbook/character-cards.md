# Character Cards (D4)

`.murcard.yaml` is mur's portable character-card format. It is a strict superset of the SillyTavern Character Card v3 (CCv3) schema with mur-specific additions tucked under `extensions.mur` so stock CCv3 readers ignore them safely.

A character card carries everything an agent needs to be re-onboarded on another machine: name, description, persona, scenario, first message, character book — plus mur's relationship metadata, voice config, first-memory, and an optional Ed25519 signature over the JCS-canonical bytes of the `data` block.

## What's in a card

```yaml
spec: murcard_v1
spec_version: "1.0"
data:
  name: Mochi
  description: |
    Mochi is a calm, curious friend who shows up before coffee.
  personality: warm, observant, quietly funny
  scenario: morning chat
  first_mes: "How did you sleep?"
  mes_example: ""
  alternate_greetings: []
  system_prompt: ""
  post_history_instructions: ""
  tags: [companion]
  character_book:
    name: Mochi memories
    entries:
      - keys: [coffee]
        content: "User prefers oat milk lattes."
extensions:
  mur:
    relationship:
      kind: friend
      formality: casual
      primary_language: en-US
    first_memory:
      text: "Sunday in Taipei"
      established_at: 2026-04-12T09:00:00Z
    voice:
      provider: none
      voice_id: ""
    signature:
      algorithm: ed25519
      pubkey: z6Mk…
      sig: base64url(...)
```

The `data` block matches CCv3 exactly (so the file imports cleanly into SillyTavern). The `extensions.mur` block is mur-only and is what the agent runtime reads on `card accept`.

## Three import paths

| Source | Detection | Notes |
|--------|-----------|-------|
| `.murcard.yaml` (native) | YAML body parses as `MurCard` | Round-trips signed cards verbatim |
| SillyTavern PNG (V3) | `\x89PNG…` header + `ccv3`/`chara_card_v3` chunk | Decoded via `png::extract_card_json` |
| SillyTavern PNG (V2) | `\x89PNG…` header + `chara` chunk only | Lifted to v3 schema with empty fields filled in |
| Character.AI scraped JSON | UTF-8 JSON with `name` + `definition` | Normalized into the `data` block |

`mur agent companion card import` auto-detects format from the file body, not the extension.

## Trust ladder

`import_card` runs `verify_card` against the card body and stamps one of three trust levels into the inbox sidecar:

- **`signed`** — the `extensions.mur.signature` block is present, the pubkey decodes, and the Ed25519 signature verifies against the JCS-canonical bytes of `data`.
- **`unsigned`** — no `signature` block. The card is still importable.
- **`failed`** — a `signature` block exists but verification failed (wrong key, tampered body, malformed signature). The card still lands in the inbox so a human can inspect it; `card accept` can refuse it later.

The trust string is forensic metadata — the inbox quarantine + B0 untrusted-input gate apply equally to all three.

## Inbox quarantine

`import_card` NEVER mutates `companion/`. It writes two files:

```
~/.mur/agents/<name>/inbox/cards/<ulid>.murcard.yaml
~/.mur/agents/<name>/inbox/cards/<ulid>.meta.json
```

The user runs `card accept <id>` to promote one card. Accepted cards are atomically moved to `inbox/cards/.applied/` (so the inbox stays small). This mirrors the `mur drafts` pattern.

## B0 protection on the first turn after import

`accept_card` does six things in sequence:

1. Apply selected fields to `profile.companion` (display name, locale, first_memory, proactive tier).
2. Mirror the result into `companion/relationship.json`.
3. Concat all `data.*` strings + `character_book` entries into one untrusted blob, sha256 it.
4. Write the blob to `<agent_home>/telemetry/inputs/<sha>.txt`.
5. Append a `ProvenanceEntry { source: "card_import", turn_id: 1 }` to `<agent_home>/telemetry/inputs.jsonl`.
6. Move the inbox files to `.applied/`.

On the next prompt, `B0SafetyHook.on_prompt_submit` reads `telemetry/inputs.jsonl`, finds the turn-1 entry, loads the sidecar text, wraps it as `<untrusted_image_text source="card_import">…</untrusted_image_text>`, and raises the `after_untrusted_input` turn flag. On the same turn, any side-effect tool call (delete / spawn / send / egress / network / `.write` / `.publish`) returns `Decision::AskUser { default: Deny, … }`.

A malicious card cannot get a side-effect tool to fire on the same turn it was accepted — it has to ask the user first. The `card_malicious_description_blocks` integration test pins this behavior.

## Worked example

```bash
# Export the current agent profile as a signed card.
mur agent companion card export mochi --sign --out /tmp/mochi.murcard.yaml

# Inspect / share / re-import on another machine.
mur agent companion card import mochi-clone /tmp/mochi.murcard.yaml
# imported id=01J… trust=signed -> ~/.mur/agents/mochi-clone/inbox/cards/01J….murcard.yaml

# See what's pending in the inbox.
mur agent companion card list mochi-clone
# id                            name                            trust       imported_at
# 01J…                          Mochi                           signed      2026-05-02T…

# Apply the card. The companion onboarding state flips, telemetry sees a
# card_import provenance entry, and B0 will gate side-effect tools on
# the next turn.
mur agent companion card accept mochi-clone 01J…
```

Unsigned export works the same way without `--sign`; the resulting YAML simply omits the `extensions.mur.signature` block.

## Acceptance gates

Run all gates in one shot:

```bash
scripts/e2e/v1-d4-card.sh
```

The runner exercises:

- **Schema** — CCv3 + `extensions.mur` round-trip without field loss (`card_schema_roundtrip`).
- **Signing** — JCS canonical bytes are deterministic; Ed25519 sign/verify round-trips; tampering is detected (`card_canonical_signature`).
- **Importers** — V3 PNG, V2 PNG, c.ai JSON (`card_png_import_v3`, `card_png_import_v2`, `card_cai_import`).
- **Quarantine** — `import` never touches `companion/`; the inbox carries id + trust + imported_at (`card_inbox_quarantine`).
- **Accept** — `accept` flips `profile.companion.onboarding.completed_at`, writes the relationship mirror, and appends a `card_import` provenance entry on turn 1 (`card_accept_promotes`).
- **CLI dispatch** — signed export → import → list → accept end-to-end through the library entry points the CLI uses (`card_cli_dispatch`).
- **B0 gate** — a card with a prompt-injection description still gets wrapped under `source = "card_import"` and side-effect tools are denied on the same turn (`card_malicious_description_blocks` + `b0_after_card_import_deny`).

## Files of interest

- `mur-core/src/character_card/{schema,extensions,canonical,signing,first_memory}.rs` — schema + JCS canonical bytes + Ed25519 sign/verify.
- `mur-core/src/cmd/agent_companion/card/{cli,export,import,list,accept,png,cai,mod}.rs` — CLI dispatch and the four library entry points.
- `mur-agent-runtime/src/hooks/b0.rs` — wraps any `card_import` provenance entry on the next turn just like a `user_drop` entry.
- `mur-core/tests/card_*.rs` + `mur-agent-runtime/tests/b0_after_card_import_deny.rs` — the acceptance tests run by `v1-d4-card.sh`.
