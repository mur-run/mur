/**
 * ModelCombobox — searchable grouped combobox for the model picker.
 *
 * Option B from the 2026 benchmark mockup: trigger button → popover with
 * search input, provider-grouped rows, per-row tier/cost/context badges,
 * keyboard navigation (ArrowUp/Down/Enter/Esc), click-outside close.
 */

import { useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AgentDetail, DetailPatch, ModelOption } from "../types";
import { filterModels, formatCost, groupByProvider } from "./modelPicker";
import { useT } from "../i18n";

// ── Provider avatar colours (deterministic; no hardcoded per-provider logic
//    beyond a colour palette — unknown providers fall back gracefully).
const PROVIDER_COLORS: Record<string, string> = {
  anthropic: "#C5694A",
  deepseek: "#4A6CF0",
  ollama: "#6B7688",
  openai: "#10A37F",
  google: "#4285F4",
  cohere: "#3B82F6",
  mistral: "#FF7000",
  local: "#6B7688",
};

function providerColor(provider: string): string {
  return PROVIDER_COLORS[provider.toLowerCase()] ?? "#6B7688";
}

function providerInitial(provider: string): string {
  // Use up to two uppercase chars so short labels like "DS" fit.
  return provider.slice(0, 2).toUpperCase();
}

// ── Tier badge CSS classes (matches mockup .b-frontier / .b-local).
function tierBadgeClass(tier?: string): string {
  if (!tier) return "mc-badge mc-badge--tier-unknown";
  return tier === "frontier" ? "mc-badge mc-badge--frontier" : "mc-badge mc-badge--local";
}

// ── Chevron SVG (inline, no external dep).
function Chevron({ open }: { open: boolean }) {
  return (
    <svg
      className={`mc-trigger__chev${open ? " mc-trigger__chev--open" : ""}`}
      width="14"
      height="14"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.8"
      aria-hidden="true"
    >
      <path d="M3 5l4 4 4-4" />
    </svg>
  );
}

// ── Search icon SVG.
function SearchIcon() {
  return (
    <svg
      className="mc-search__icon"
      width="15"
      height="15"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.8"
      aria-hidden="true"
    >
      <circle cx="6.5" cy="6.5" r="4.5" />
      <path d="M10 10l3.5 3.5" />
    </svg>
  );
}

// ── Check mark SVG (shown on selected row).
function CheckIcon() {
  return (
    <svg
      className="mc-opt__check"
      width="15"
      height="15"
      fill="none"
      stroke="currentColor"
      strokeWidth="2.2"
      aria-hidden="true"
    >
      <path d="M3 8l3.5 3.5L12 4" />
    </svg>
  );
}

// ── Props ──────────────────────────────────────────────────────────────────

interface Props {
  detail: AgentDetail;
  onSaved: (d: AgentDetail) => void;
  onManage: () => void;
}

// ── Component ──────────────────────────────────────────────────────────────

export function ModelCombobox({ detail, onSaved, onManage }: Props) {
  const { t } = useT();

  const [models, setModels] = useState<ModelOption[]>([]);
  const [fetchError, setFetchError] = useState<string | null>(null);

  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [activeIdx, setActiveIdx] = useState(0);

  const [saving, setSaving] = useState(false);
  const [justSaved, setJustSaved] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);

  const comboRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  // Fetch model list once on mount.
  useEffect(() => {
    invoke<ModelOption[]>("list_models")
      .then(setModels)
      .catch((e) => setFetchError(String(e)));
  }, []);

  // Click-outside closes popover.
  useEffect(() => {
    if (!open) return;
    function handler(e: MouseEvent) {
      if (comboRef.current && !comboRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    }
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [open]);

  // Focus search input when opening.
  useEffect(() => {
    if (open) {
      setQuery("");
      setActiveIdx(0);
      setTimeout(() => searchRef.current?.focus(), 0);
    }
  }, [open]);

  // ── Derived ──────────────────────────────────────────────────────────────

  const filtered = filterModels(models, query);
  const groups = groupByProvider(filtered);

  // Precompute groups with deterministic flat indices — pure, render-count-independent.
  // Each row's flatIndex = sum of prior groups' lengths + index within group.
  const indexedGroups = useMemo(() => {
    let offset = 0;
    return groups.map(([provider, items]) => {
      const indexed = items.map((m, i) => ({ m, flatIndex: offset + i }));
      offset += items.length;
      return { provider, indexed };
    });
  }, [groups]);

  // Build a flat ordered list of ref_names so keyboard nav can index them.
  const flatRefs: string[] = indexedGroups.flatMap(({ indexed }) =>
    indexed.map(({ m }) => m.ref_name)
  );

  const currentModel = models.find((m) => m.ref_name === detail.model_ref);

  // ── Handlers ─────────────────────────────────────────────────────────────

  function handleTrigger() {
    setOpen((prev) => !prev);
  }

  async function pick(refName: string) {
    if (!refName || refName === detail.model_ref) {
      setOpen(false);
      return;
    }
    setSaving(true);
    setSaveError(null);
    try {
      const updated = await invoke<AgentDetail>("update_agent_detail", {
        name: detail.agent_name,
        patch: { model_ref: refName } as DetailPatch,
      });
      onSaved(updated);
      setJustSaved(true);
      setTimeout(() => setJustSaved(false), 4000);
      setOpen(false);
    } catch (e) {
      setSaveError(String(e));
    } finally {
      setSaving(false);
    }
  }

  function handleKeyDown(e: React.KeyboardEvent<HTMLInputElement>) {
    const total = flatRefs.length;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      const next = Math.min(activeIdx + 1, total - 1);
      setActiveIdx(next);
      scrollActiveIntoView(next);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      const prev = Math.max(activeIdx - 1, 0);
      setActiveIdx(prev);
      scrollActiveIntoView(prev);
    } else if (e.key === "Enter") {
      e.preventDefault();
      const ref = flatRefs[activeIdx];
      if (ref) pick(ref);
    } else if (e.key === "Escape") {
      setOpen(false);
    }
  }

  function scrollActiveIntoView(idx: number) {
    // Defer so the DOM has updated after activeIdx state change.
    setTimeout(() => {
      const el = listRef.current?.querySelector<HTMLElement>(`[data-idx="${idx}"]`);
      el?.scrollIntoView({ block: "nearest" });
    }, 0);
  }

  // ── Render trigger label ──────────────────────────────────────────────────

  function TriggerContent() {
    if (!currentModel) {
      // Fallback: show inline model from profile.
      const inlineLabel = `${detail.model_provider}/${detail.model_name}`;
      return (
        <span className="mc-trigger__label">
          <span className="mc-trigger__alias">{inlineLabel}</span>
        </span>
      );
    }
    const color = providerColor(currentModel.provider);
    const initials = providerInitial(currentModel.provider);
    return (
      <>
        <span className="mc-av" style={{ background: color }} aria-hidden="true">
          {initials}
        </span>
        <span className="mc-trigger__label">
          <span className="mc-trigger__alias">{currentModel.ref_name}</span>
          <span className="mc-trigger__sub">
            {" "}
            · {currentModel.provider}/{currentModel.model}
          </span>
        </span>
      </>
    );
  }

  // ── Render popover rows ───────────────────────────────────────────────────

  function renderGroups() {
    if (filtered.length === 0) {
      return (
        <div className="mc-empty">
          {query.trim()
            ? t("detail.modelSearch").replace("…", "") + ` — "${query}"`
            : t("detail.modelEmpty")}
        </div>
      );
    }

    return indexedGroups.map(({ provider, indexed }) => {
      const color = providerColor(provider);
      const initials = providerInitial(provider);

      return (
        <div key={provider} className="mc-group">
          <div className="mc-group__header">
            <span className="mc-av mc-av--sm" style={{ background: color }} aria-hidden="true">
              {initials}
            </span>
            {provider}
            <span className="mc-group__count">· {indexed.length}</span>
          </div>
          {indexed.map(({ m, flatIndex: idx }) => {
            const isSel = m.ref_name === detail.model_ref;
            const isActive = idx === activeIdx;
            const outCost = formatCost(m.output_cost);
            const inCost = formatCost(m.input_cost);

            return (
              <div
                key={m.ref_name}
                className={`mc-opt${isSel ? " mc-opt--sel" : ""}${isActive ? " mc-opt--active" : ""}`}
                data-idx={idx}
                role="option"
                aria-selected={isSel}
                onMouseEnter={() => setActiveIdx(idx)}
                onClick={() => pick(m.ref_name)}
              >
                <span className="mc-opt__check-wrap">
                  {isSel && <CheckIcon />}
                </span>
                <span className="mc-opt__body">
                  <span className="mc-opt__name">{m.ref_name}</span>
                  <span className="mc-opt__sub">
                    {m.provider}/{m.model}
                  </span>
                </span>
                <span className="mc-badges">
                  {m.tier && (
                    <span className={tierBadgeClass(m.tier)}>{m.tier}</span>
                  )}
                  {outCost !== null && (
                    <span className="mc-badge mc-badge--cost">out {outCost}</span>
                  )}
                  {inCost !== null && (
                    <span className="mc-badge mc-badge--cost">in {inCost}</span>
                  )}
                  {m.context_window != null && (
                    <span className="mc-badge mc-badge--ctx">
                      {m.context_window >= 1000
                        ? `${Math.round(m.context_window / 1000)}k`
                        : String(m.context_window)}
                    </span>
                  )}
                  {m.capabilities?.map((cap) => (
                    <span key={cap} className="mc-badge mc-badge--cap">
                      {cap}
                    </span>
                  ))}
                </span>
              </div>
            );
          })}
        </div>
      );
    });
  }

  // ── Empty-state: no models in registry ────────────────────────────────────

  if (fetchError) {
    return (
      <div className="tab-form" style={{ marginBottom: 18 }}>
        <label className="field-label">{t("detail.model")}</label>
        <p className="save-error">{fetchError}</p>
      </div>
    );
  }

  if (models.length === 0) {
    return (
      <div className="tab-form" style={{ marginBottom: 18 }}>
        <label className="field-label">{t("detail.model")}</label>
        <p className="field-muted" style={{ fontSize: 12 }}>
          {t("detail.modelEmpty")}
        </p>
      </div>
    );
  }

  // ── Full combobox ─────────────────────────────────────────────────────────

  return (
    <div className="tab-form" style={{ marginBottom: 18 }}>
      <label className="field-label">{t("detail.model")}</label>

      <div
        ref={comboRef}
        className={`mc-combo${open ? " mc-combo--open" : ""}${saving ? " mc-combo--saving" : ""}`}
        aria-haspopup="listbox"
        aria-expanded={open}
      >
        {/* Trigger button */}
        <button
          type="button"
          className="mc-trigger"
          onClick={handleTrigger}
          disabled={saving}
          aria-label={t("detail.model")}
        >
          <TriggerContent />
          <Chevron open={open} />
        </button>

        {/* Popover */}
        {open && (
          <div className="mc-pop" role="listbox">
            {/* Search row */}
            <div className="mc-search">
              <SearchIcon />
              <input
                ref={searchRef}
                className="mc-search__input"
                type="text"
                value={query}
                placeholder={t("detail.modelSearch")}
                autoComplete="off"
                aria-label={t("detail.modelSearch")}
                onChange={(e) => {
                  setQuery(e.target.value);
                  setActiveIdx(0);
                }}
                onKeyDown={handleKeyDown}
              />
            </div>

            {/* Model list */}
            <div ref={listRef} className="mc-list">
              {renderGroups()}
            </div>

            {/* Keyboard hint footer */}
            <div className="mc-kbd-foot">
              <span>
                <kbd>↑</kbd>
                <kbd>↓</kbd> navigate
              </span>
              <span>
                <kbd>↵</kbd> select
              </span>
              <span>
                <kbd>esc</kbd> close
              </span>
              <span style={{ marginLeft: "auto" }}>
                <button
                  type="button"
                  className="mc-manage-link"
                  onClick={() => {
                    setOpen(false);
                    onManage();
                  }}
                >
                  ⚙︎ {t("detail.manageModels")}
                </button>
              </span>
            </div>
          </div>
        )}
      </div>

      {/* Status messages */}
      {justSaved && (
        <p className="field-muted" style={{ fontSize: 12 }}>
          {t("detail.modelRestartHint")}
        </p>
      )}
      {saveError && <p className="save-error">{saveError}</p>}
    </div>
  );
}
