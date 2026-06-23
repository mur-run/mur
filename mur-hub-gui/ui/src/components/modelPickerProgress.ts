/**
 * Pure helper for model download progress display.
 * No DOM, no React — unit-testable.
 *
 * HuggingFace omits file sizes so total=0 is common; treat as indeterminate.
 * done > total is also possible in that case — never show >100% or NaN.
 */

export interface DownloadProgress {
  /** True when total is unknown (total<=0). Show a spinner instead of a bar. */
  indeterminate: boolean;
  /** Clamped 0..100 integer. Always 0 when indeterminate. */
  percent: number;
  /** Human-readable label, e.g. "42%" or "Downloading…" */
  label: string;
}

export function downloadProgress(done: number, total: number): DownloadProgress {
  if (total <= 0) {
    return { indeterminate: true, percent: 0, label: "Downloading…" };
  }
  const raw = (done / total) * 100;
  const percent = Math.min(100, Math.max(0, Math.round(raw)));
  return { indeterminate: false, percent, label: `${percent}%` };
}
