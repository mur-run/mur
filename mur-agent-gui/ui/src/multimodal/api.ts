import { invoke } from "@tauri-apps/api/core";
import type { MultimodalArtifact } from "./types";

export const multimodalDrop = (agent: string, paths: string[], turnId: number) =>
  invoke<MultimodalArtifact[]>("multimodal_drop", { agent, paths, turnId });

export const multimodalPaste = (agent: string, turnId: number) =>
  invoke<MultimodalArtifact[]>("multimodal_paste", { agent, turnId });
