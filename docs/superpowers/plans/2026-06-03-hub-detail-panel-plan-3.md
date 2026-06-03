# Hub Detail Panel + CSS — Plan 3 Implementation Plan

**Date:** 2026-06-03
**Spec:** `2026-05-29-mur-agent-export-ux-give-to-a-friend-design.md` Plan 3 outline
**Builds on:** Plan 1, Plan 1b, Plan 2 (all Rust backend done)

## Goal

Complete the Hub UI so the detail panel (right sidebar when clicking an agent) shows 7 functional tabs — Persona, Style, Behavior, Skills, MCP, Permissions, Inbox — with proper CSS styling. Also fix missing `.input` CSS class used by the import modal's model wizard.

## Current State vs Target

| Item | Current | Target |
|------|---------|--------|
| Detail panel CSS | Completely missing (`.detail-panel*` classes undefined) | Full styled right sidebar |
| Detail panel tabs | Only "Inbox" rendered | Persona / Style / Behavior / Skills / MCP / Permissions / Inbox |
| `.input` CSS | Missing (used by ModelResolutionStep) | Styled text inputs |
| Rust: agent detail read | Discovery reads full profile but discards most fields | `get_agent_detail` command |
| Rust: agent detail write | None | `update_agent_detail` command |

## Architecture

**Rust backend** (`mur-hub-gui/src-tauri/src/detail.rs`):
- `get_agent_detail(name)` — reads `profile.yaml` → returns `AgentDetail` with all tab data
- `update_agent_detail(name, patch)` — applies `DetailPatch` (all-Optional fields) → writes back `profile.yaml`

**React frontend** (`mur-hub-gui/ui/src/components/`):
- New `DetailPanel.tsx` component with tab routing
- New tab components: `PersonaTab`, `StyleTab`, `BehaviorTab`, `SkillsTab`, `McpTab`, `PermissionsTab`
- Existing `CompanionInbox.tsx` reused for Inbox tab

**CSS** (`styles.css`):
- `.input` class
- `.detail-panel*` classes
- Tab-specific styles

---

## Task 1: Rust backend — `AgentDetail` type + `get_agent_detail` command

**Files:**
- Create: `mur-hub-gui/src-tauri/src/detail.rs`
- Modify: `mur-hub-gui/src-tauri/src/lib.rs`

**Steps:**

- [ ] 1.1 Create `detail.rs` with `AgentDetail` struct + `get_agent_detail` command

```rust
//! Agent detail panel — full-profile read + partial write for the Hub's
//! right-side slide-in panel (Persona / Style / Behavior / Skills / MCP /
//! Permissions / Inbox tabs).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// All detail-panel tab data extracted from one AgentProfile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDetail {
    // Persona tab
    pub persona_category: String,
    pub persona_description: String,
    pub persona_tone: String,
    pub persona_risk: String,
    pub persona_verbosity: String,
    // Style tab
    pub style_preset: String,
    pub render_status: RenderStatusView,
    // Behavior tab
    pub behavior_preset: String,
    // Skills tab
    pub skills: Vec<SkillView>,
    pub installed_skills: Vec<InstalledSkillView>,
    // MCP tab
    pub mcp_servers: Vec<McpServerView>,
    // Permissions tab
    pub capabilities: Vec<String>,
    // Read-only metadata
    pub display_name: String,
    pub agent_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RenderStatusView {
    Pending,
    Rendering { done: u8, total: u8 },
    Ready,
    Failed { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillView {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledSkillView {
    pub name: String,
    pub version: String,
    pub description: String,
    pub category: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerView {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
}

/// Partial update to an agent's profile. All fields optional — only set
/// fields are applied.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DetailPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona_category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona_description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona_tone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona_risk: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona_verbosity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style_preset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior_preset: Option<String>,
}

#[tauri::command]
pub fn get_agent_detail(name: String) -> Result<AgentDetail, String> {
    let mur_home = crate::mur_home_path();
    let profile_path = mur_home.join("agents").join(&name).join("profile.yaml");
    let bytes = std::fs::read(&profile_path).map_err(|e| format!("read profile: {e}"))?;
    let profile: mur_common::AgentProfile =
        serde_yaml_ng::from_slice(&bytes).map_err(|e| format!("parse profile: {e}"))?;

    Ok(AgentDetail {
        persona_category: format!("{:?}", profile.persona.category).to_lowercase(),
        persona_description: profile.persona.description,
        persona_tone: profile.persona.traits.tone,
        persona_risk: profile.persona.traits.risk,
        persona_verbosity: profile.persona.traits.verbosity,
        style_preset: profile.appearance.style_preset,
        render_status: match profile.appearance.render_status {
            mur_common::agent::RenderStatus::Pending => RenderStatusView::Pending,
            mur_common::agent::RenderStatus::Rendering { done, total } => {
                RenderStatusView::Rendering { done, total }
            }
            mur_common::agent::RenderStatus::Ready => RenderStatusView::Ready,
            mur_common::agent::RenderStatus::Failed { reason } => {
                RenderStatusView::Failed { reason }
            }
        },
        behavior_preset: format!("{:?}", profile.appearance.behavior_preset).to_lowercase(),
        skills: profile
            .skills
            .into_iter()
            .map(|path| SkillView { path })
            .collect(),
        installed_skills: profile
            .installed_skills
            .into_iter()
            .map(|s| InstalledSkillView {
                name: s.name,
                version: s.version,
                description: s.description,
                category: s.category,
            })
            .collect(),
        mcp_servers: profile
            .mcp_servers
            .into_iter()
            .map(|m| McpServerView {
                name: m.name,
                command: m.command,
                args: m.args,
            })
            .collect(),
        capabilities: profile.capabilities,
        display_name: profile.display_name,
        agent_name: profile.name,
    })
}
```

- [ ] 1.2 Build verification: `cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml`

---

## Task 2: Rust backend — `update_agent_detail` command

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/detail.rs`

- [ ] 2.1 Add `update_agent_detail` command to `detail.rs`

```rust
#[tauri::command]
pub fn update_agent_detail(name: String, patch: DetailPatch) -> Result<AgentDetail, String> {
    let mur_home = crate::mur_home_path();
    let profile_path = mur_home.join("agents").join(&name).join("profile.yaml");
    let bytes = std::fs::read(&profile_path).map_err(|e| format!("read profile: {e}"))?;
    let mut profile: mur_common::AgentProfile =
        serde_yaml_ng::from_slice(&bytes).map_err(|e| format!("parse profile: {e}"))?;

    // Apply persona patches
    if let Some(cat) = patch.persona_category {
        profile.persona.category = match cat.as_str() {
            "research" => mur_common::agent::PersonaCategory::Research,
            "automation" => mur_common::agent::PersonaCategory::Automation,
            "monitor" => mur_common::agent::PersonaCategory::Monitor,
            "notify" => mur_common::agent::PersonaCategory::Notify,
            "commerce" => mur_common::agent::PersonaCategory::Commerce,
            _ => mur_common::agent::PersonaCategory::Custom,
        };
    }
    if let Some(d) = patch.persona_description { profile.persona.description = d; }
    if let Some(t) = patch.persona_tone { profile.persona.traits.tone = t; }
    if let Some(r) = patch.persona_risk { profile.persona.traits.risk = r; }
    if let Some(v) = patch.persona_verbosity { profile.persona.traits.verbosity = v; }

    // Apply style patch
    if let Some(s) = patch.style_preset {
        profile.appearance.style_preset = s;
    }

    // Apply behavior patch
    if let Some(b) = patch.behavior_preset {
        profile.appearance.behavior_preset = match b.as_str() {
            "quiet" => mur_common::agent::BehaviorPreset::Quiet,
            "lively" => mur_common::agent::BehaviorPreset::Lively,
            _ => mur_common::agent::BehaviorPreset::Normal,
        };
    }

    profile.updated_at = chrono::Utc::now().to_rfc3339();
    let yaml = serde_yaml_ng::to_string(&profile).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(&profile_path, yaml).map_err(|e| format!("write profile: {e}"))?;

    // Return fresh detail after update
    get_agent_detail(name)
}
```

- [ ] 2.2 Build verification

---

## Task 3: Register new commands in lib.rs

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/lib.rs`

- [ ] 3.1 Add `pub mod detail;` and commands to handler

In `lib.rs`:
- Add `pub mod detail;` near other `pub mod` lines
- Add `detail::get_agent_detail,` and `detail::update_agent_detail,` to `generate_handler![]`

- [ ] 3.2 Build verification: `cargo build -p mur-hub-gui 2>&1`

---

## Task 4: TypeScript types

**Files:**
- Modify: `mur-hub-gui/ui/src/types.ts`

- [ ] 4.1 Add AgentDetail, DetailPatch, and tab-related types

```typescript
export interface SkillView { path: string }
export interface InstalledSkillView {
  name: string;
  version: string;
  description: string;
  category: string;
}
export interface McpServerView {
  name: string;
  command: string;
  args: string[];
}
export type RenderStatusView =
  | { status: "pending" }
  | { status: "rendering"; done: number; total: number }
  | { status: "ready" }
  | { status: "failed"; reason: string };

export interface AgentDetail {
  persona_category: string;
  persona_description: string;
  persona_tone: string;
  persona_risk: string;
  persona_verbosity: string;
  style_preset: string;
  render_status: RenderStatusView;
  behavior_preset: string;
  skills: SkillView[];
  installed_skills: InstalledSkillView[];
  mcp_servers: McpServerView[];
  capabilities: string[];
  display_name: string;
  agent_name: string;
}

export interface DetailPatch {
  persona_category?: string;
  persona_description?: string;
  persona_tone?: string;
  persona_risk?: string;
  persona_verbosity?: string;
  style_preset?: string;
  behavior_preset?: string;
}

export type DetailTab = "persona" | "style" | "behavior" | "skills" | "mcp" | "permissions" | "inbox";
export const ALL_DETAIL_TABS: DetailTab[] = [
  "persona", "style", "behavior", "skills", "mcp", "permissions", "inbox"
];
```

---

## Task 5: DetailPanel React component

**Files:**
- Create: `mur-hub-gui/ui/src/components/DetailPanel.tsx`

- [ ] 5.1 Create the main DetailPanel with tab routing

Component structure:
- Receives `agentName` prop
- On mount/agentName change: calls `invoke<AgentDetail>("get_agent_detail", { name: agentName })`
- Tab bar: Persona | Style | Behavior | Skills | MCP | Permissions | Inbox
- Active tab state, renders corresponding sub-component
- Close button calls `onClose()`

- [ ] 5.2 PersonaTab
  - Editable fields: category (dropdown), description (textarea), tone, risk, verbosity (text inputs)
  - Save button calls `update_agent_detail`

- [ ] 5.3 StyleTab
  - Show current preset name + family
  - Preset gallery (BUILTIN_PRESETS grid, clickable to select)
  - Render status display (Pending/Rendering progress bar/Ready thumbnail/Failed error)
  - "Re-render" button if Ready or Failed

- [ ] 5.4 BehaviorTab
  - Radio buttons: Quiet / Normal / Lively
  - Description of each mode
  - Auto-save on selection

- [ ] 5.5 SkillsTab
  - List installed_skills with name, version, description, category
  - List legacy skills paths
  - (Add/remove deferred to v2)

- [ ] 5.6 McpTab
  - List MCP servers with name, command, args
  - (Add/remove deferred to v2)

- [ ] 5.7 PermissionsTab
  - List capabilities as tags/badges
  - Show entitlements (filesystem paths, network domains, processes)
  - (Edit deferred to v2 — read-only in v1)

- [ ] 5.8 InboxTab — reuse existing CompanionInbox component

---

## Task 6: Wire DetailPanel into DashboardApp

**Files:**
- Modify: `mur-hub-gui/ui/src/components/DashboardApp.tsx`

- [ ] 6.1 Replace the inline detail panel with DetailPanel component
  - Import DetailPanel
  - Replace lines 526-548 (the `<aside className="detail-panel">...</aside>` block) with `<DetailPanel agentName={selectedAgent} onClose={() => setSelected(null)} agents={agents} />`

---

## Task 7: CSS — `.input` class + detail panel styles

**Files:**
- Modify: `mur-hub-gui/ui/src/styles.css`

- [ ] 7.1 Add `.input` class
```css
.input {
  padding: 6px 10px;
  border: 1px solid var(--border, #333);
  border-radius: 6px;
  background: var(--bg-input, #1a1a1a);
  color: var(--text-primary, #e0e0e0);
  font-size: 13px;
  font-family: inherit;
  outline: none;
  transition: border-color 0.15s;
}
.input:focus {
  border-color: var(--accent, #4F46E5);
}
```

- [ ] 7.2 Add `.detail-panel*` classes
```css
.detail-panel {
  width: 320px;
  min-width: 320px;
  border-left: 1px solid var(--border, #2a2a2a);
  background: var(--bg-panel, #111);
  display: flex;
  flex-direction: column;
  overflow: hidden;
}
.detail-panel-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 14px 16px;
  border-bottom: 1px solid var(--border, #2a2a2a);
}
.detail-panel-title {
  font-size: 15px;
  font-weight: 600;
  color: var(--text-primary, #e0e0e0);
}
.detail-panel-close {
  background: none;
  border: none;
  color: var(--text-secondary, #888);
  font-size: 18px;
  cursor: pointer;
  padding: 2px 6px;
  border-radius: 4px;
}
.detail-panel-close:hover {
  background: var(--bg-hover, #1a1a1a);
  color: var(--text-primary, #e0e0e0);
}
.detail-panel-tabs {
  display: flex;
  gap: 0;
  border-bottom: 1px solid var(--border, #2a2a2a);
  overflow-x: auto;
  padding: 0 4px;
}
.detail-tab {
  padding: 8px 12px;
  font-size: 12px;
  color: var(--text-secondary, #888);
  cursor: pointer;
  border-bottom: 2px solid transparent;
  white-space: nowrap;
  transition: color 0.15s, border-color 0.15s;
  user-select: none;
}
.detail-tab:hover {
  color: var(--text-primary, #e0e0e0);
}
.detail-tab--active {
  color: var(--accent, #4F46E5);
  border-bottom-color: var(--accent, #4F46E5);
}
.detail-panel-body {
  flex: 1;
  overflow-y: auto;
  padding: 16px;
}
```

- [ ] 7.3 Add tab-specific CSS (form fields, preset gallery, etc.)

---

## Self-Review

1. All 6 missing tabs covered: Persona, Style, Behavior, Skills, MCP, Permissions + existing Inbox = 7 total ✓
2. Rust backend: read + partial write via single `DetailPatch` struct ✓
3. Frontend: single `DetailPanel` component with tab routing ✓
4. CSS: `.input` fix + detail-panel family + tab-specific styles ✓
5. Builds on existing Plan 1/1b/2 without modifying them ✓
