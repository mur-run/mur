# MuR Skill Registry Specification

**Repo:** `mur-run/skill-registry` (to be created)

A git-based skill registry for the MuR ecosystem. No server required —
all operations happen via git commits and GitHub Pull Requests.

## Directory Layout

```
mur-run/skill-registry/
├── index.yaml                    # Auto-updated search index (required)
├── skills/
│   ├── research-prices/
│   │   ├── versions/
│   │   │   ├── 1.0.0.yaml        # Published skill canonical YAML
│   │   │   ├── 1.0.0.sig.json   # Ed25519 DSSE signature envelope
│   │   │   └── 1.1.0.yaml
│   │   └── latest                # Symlink (optional, convenience)
│   └── web-browsing/
│       └── versions/
│           └── 2.0.0.yaml
├── .github/
│   └── workflows/
│       └── validate.yml          # CI: validate skills on PR
└── CLAUDE.md                     # Contributor docs
```

## index.yaml Format

Auto-regenerated on each merge to main. Schema defined in
`mur-common/src/skill/registry.rs`:

```yaml
schema_version: 1
updated_at: 2026-05-25T00:00:00Z
skills:
  research-prices:
    latest: 1.1.0
    description: Search and compare product prices
    publisher: human:david
    category: workflow
    tags: [e-commerce, price]
    content_sha256: "abcdef1234..."
    install_count: 42
```

## Publishing Flow

1. Author creates skill.yaml locally
2. `mur skill publish ./skill.yaml` signs it and creates a PR
3. CI auto-validates the PR
4. Maintainer merges → index.yaml auto-regenerates via GitHub Action
5. Users can `mur skill install <name>` from the registry

## CI Validation (validate.yml)

On every PR to `main`:

1. **YAML validation**: `mur skill validate` on each new/changed skill file
2. **Signature verification**: if `*.sig.json` present, verify Ed25519 signature
3. **No duplicate versions**: reject PRs with existing version files
4. **Publisher namespace check**: publisher prefix matches owning user/org
