/**
 * Pure helpers for the fleet creation form's Mode section.
 * No DOM, no React — unit-testable, mirrored against modelPicker.ts's pattern.
 */

export type FleetMode = "plain" | "speculative" | "partition";

export interface TrackInput {
  name: string;
  approach: string;
  model: string;
}

export interface ParallelTrackPayload {
  name: string;
  approach: string;
  model: string | null;
}

export interface ParallelConfigPayload {
  mode: "speculative" | "partition";
  tracks: ParallelTrackPayload[];
  judge: { model: string };
  pre_filter?: string[];
  partition?: { target_file: string };
}

/** Matches mur-core's parse_duration: digits + optional single-char s/m/h/d suffix. */
export const DURATION_RE = /^\d+[smhd]?$/;

export function canSubmitMode(
  mode: FleetMode,
  tracks: TrackInput[],
  judgeModel: string,
  targetFile: string
): boolean {
  if (mode === "speculative") return tracks.length >= 2 && judgeModel.trim() !== "";
  if (mode === "partition") return targetFile.trim() !== "" && judgeModel.trim() !== "";
  return true;
}

export function buildParallelPayload(
  mode: FleetMode,
  tracks: TrackInput[],
  judgeModel: string,
  targetFile: string,
  preFilterCargoCheck: boolean,
  preFilterClippy: boolean
): ParallelConfigPayload | null {
  if (mode === "plain") return null;
  if (mode === "speculative") {
    return {
      mode: "speculative",
      tracks: tracks.map((t) => ({
        name: t.name,
        approach: t.approach,
        model: t.model.trim() || null,
      })),
      judge: { model: judgeModel.trim() },
      pre_filter: [
        ...(preFilterCargoCheck ? ["cargo_check"] : []),
        ...(preFilterClippy ? ["cargo_clippy_deny"] : []),
      ],
    };
  }
  return {
    mode: "partition",
    tracks: [],
    judge: { model: judgeModel.trim() },
    pre_filter: [],
    partition: { target_file: targetFile.trim() },
  };
}
