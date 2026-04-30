// Thin wrappers around the voice Tauri commands. Mirrors
// mur-agent-gui/src-tauri/src/commands.rs voice_* surface.

import { invoke } from "@tauri-apps/api/core";
import type { VoiceListResponse, VoiceStatus, SttStatus } from "./types";

export const voiceStatus = () => invoke<VoiceStatus>("voice_status");
export const voiceEnable = () => invoke<void>("voice_enable");
export const voiceDisable = () => invoke<void>("voice_disable");
export const voiceListInstalled = () =>
  invoke<VoiceListResponse>("voice_list_installed");
export const voiceSetDefault = (voiceId: string) =>
  invoke<void>("voice_set_default", { voiceId });
export const voiceDownload = (voiceId: string) =>
  invoke<void>("voice_download", { voiceId });
export const voiceSttStatus = () => invoke<SttStatus>("voice_stt_status");
export const voiceSttDownload = () => invoke<void>("voice_stt_download");
export const ttsSpeak = (text: string) =>
  invoke<void>("tts_speak", { text });
export const sttTranscribePcm16k = (samplesI16: number[]) =>
  invoke<string>("stt_transcribe_pcm16k", { samplesI16 });
