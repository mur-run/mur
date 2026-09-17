---
name: update-docs
description: Update MUR's user-facing documentation after shipping a feature or cutting a release. Covers the three canonical locations (README, the app.mur.run docs site, the product page), exactly how each is wired, and the publish gotchas. Use when a change adds/renames a CLI command, a capability, or a user-facing behavior.
---

# Updating MUR Documentation

Three canonical locations must stay in sync when a user-facing surface changes. Two live in **this repo** (`mur`); two live in the **`mur-server`** repo and deploy to the public `app.mur.run` on merge.

| # | What | Where | Repo |
|---|---|---|---|
| 1 | README | `README.md` | `mur` |
| 2 | Docs site pages | `dashboard/docs-content/*.md` + `dashboard/src/components/docs/coreNavigation.tsx` | `mur-server` |
| 3 | Product / marketing page | `dashboard/src/app/products/mur/page.tsx` | `mur-server` |

`mur-server` is at `/Volumes/Firecuda4tb/Projects/mur-server`.

## 1. README (`mur` repo)

- **Command tree** — a `<details>` block titled `Full command tree (N top-level commands)`. Add a new top-level command as its own `├── name  sub · sub …  (one-line purpose)` row; add a subcommand to an existing command's row. **Update the `N` count** in the `<summary>` when you add/remove a top-level command.
- **Feature sections** — the emoji-headed `### 🔐 Stay governed` / `### 🔌 Power the tools …` sections. Add a bullet where a new capability belongs; keep the terse, benefit-led voice.
- **Roadmap** (`## 🧭 Roadmap`) — if a shipped feature was listed as upcoming, remove/downgrade it.
- Grep before editing: `grep -nE "top-level commands|<name>|<feature-keyword>" README.md`.

## 2. Docs site pages (`mur-server`)

- Pages are **plain Markdown** in `dashboard/docs-content/`, no front-matter (start with `# Title`). They render at `/docs/core/<slug>` — the slug is the filename without `.md`, loaded by `getDocBySlug` (`dashboard/src/app/docs/core/[[...slug]]/page.tsx`).
- **Register the slug in `SLUG_TO_FILE`** (`dashboard/src/lib/docs.ts`) — adding the file alone is not enough. `getAllDocSlugs()` enumerates that hardcoded map, so `generateStaticParams` never sees an unregistered slug: the page still works (`getDocBySlug` falls back to `DOCS_DIR/<slug>.md`) but renders on demand instead of prerendering, and a missing file fails at request time rather than at build. Verify with `find .next -name "<slug>*"` after `npm run build` — registered slugs produce `.html`/`.rsc`/`.segments`, unregistered ones produce nothing. (The eight-page fallback backlog this file used to list is gone — audited 2026-09-17, all 33 top-level pages are registered.)
- **Audit the registry the other way too — that failure is worse.** A slug registered to a file that no longer exists becomes a *live 404 route*: the map is what `generateStaticParams` enumerates, so the route ships, and `getDocBySlug` logs `Doc file not found` and returns null. Five such routes were live on app.mur.run until mur-server#90 (`quickstart`, `configuration`, `collections`, `import`, `cloud-sync` — deleted by the v2 docs rewrite, which left both the map entries and the sidebar links). They were reachable by clicking, not just by typing a URL. When a page *moves*, repoint its entry (`'quickstart': 'getting-started/quick-start.md'`) rather than deleting it — that keeps the short URL working for external bookmarks; only drop the entry when the content is genuinely gone.
- **Navigation** — add `{ title: '<Title>', href: '/docs/core/<slug>' }` to the right group in `dashboard/src/components/docs/coreNavigation.tsx` (groups: Getting Started / Features / Integrations / Resources). Without a nav entry the route exists but nothing links to it. **Check every `href:` resolves, not just the one you added** — the bug class is "sidebar link → 404", and resolution order is mapped name → `<slug>.md` → multi-segment path (`getting-started/quick-start` is a real page at `/docs/core/getting-started/quick-start`). Auditing all of them is what proves a nav fix complete; auditing the map alone finds only what you happened to look at.
- Mirror an existing page's shape (e.g. `agent-cli.md`): `# Title`, a one-line what-it-is, a `bash` example block, then `##` sections.
- **Known debt:** `docs-content/commands.md` is stale — organized around the old pattern/learning pipeline, missing the agent/fleet/capability model. Prefer adding a focused new page over threading into it; a full `commands.md` overhaul is its own task.

## 3. Product page (`mur-server`)

- `dashboard/src/app/products/mur/page.tsx` — a React/JSX page. The `{/* Features */}` section is a grid of cards: `<div className="p-6 rounded-lg border bg-card"><h3>…</h3><p>…</p></div>`. Add a card; use `border-2 border-primary/30` to highlight a flagship feature.
- It's **JSX**: escape `&` as `&amp;`, inline code as `<code className="bg-muted px-1 rounded text-xs">…</code>`, keep every `<div>` balanced. A typo breaks the Vercel build.

## Publish flow & gotchas

- **`mur` repo** (README) — normal PR; may auto-merge on green CI per the supervised-PR convention. It's a GitHub-hosted README.
- **`mur-server`** (docs + product) — merging **deploys to the public `app.mur.run`** (Git-connected Vercel project). Treat it as publishing public content: open the PR, **do NOT auto-merge**, let the human review the **Vercel preview** and merge when happy. See `mem:project_app_mur_run_vercel_domain_move`.
- Verify the CLI surface against the source before writing (`grep` the `cli/` action enums / `dispatch.rs`), not from memory — command names and subcommands drift.
- One PR per repo. Cross-repo doc changes are two PRs.
