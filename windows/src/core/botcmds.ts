// Commands and events of the second batch (voice, meetings, screen sharing,
// context, attachments coming back). The Rust side may not have them yet: every
// call goes through callCmd(), which turns "no such command" into
// "no disponible todavía". The names live here and only here, so renaming one
// on the Rust side is a one-line change.

import { invoke } from "@tauri-apps/api/core";
import { IS_TAURI } from "./bridge";

// bot-step, bot-attach and capture_context are typed in bridge.ts (Aerys).
export type { BotAttachEvent, BotStepEvent, CapturedContext } from "./bridge";

export const CMD = {
  // Voice: Whisper (dictation, meetings) and Piper (speech).
  startDictation: "start_dictation",
  stopDictation: "stop_dictation",
  startMeeting: "start_meeting",
  stopMeeting: "stop_meeting",
  speak: "speak",
  stopSpeaking: "stop_speaking",
  listVoices: "list_voices",
  setBotVoice: "set_bot_voice",
  botVoices: "bot_voices",
  previewVoice: "preview_voice",
  installVoice: "install_voice",
  setTtsKey: "set_tts_key",
  ttsKeyStatus: "tts_key_status",
  // Voice call with the Bots (and the Cursor agent).
  startCall: "start_call",
  stopCall: "stop_call",
  callMute: "call_mute",
  callCursorStatus: "call_cursor_status",
  // Screen and context.
  startScreenShare: "start_screen_share",
  stopScreenShare: "stop_screen_share",
  // capture_context and open_attachment: Bridge.captureContext / Bridge.openAttachment.
} as const;

export const EVT = {
  botStep: "bot-step",
  botAttach: "bot-attach",
  dictationPartial: "dictation-partial",
  meetingState: "meeting-state",
  meetingChunk: "meeting-chunk",
  screenShare: "screen-share",
  voiceEngine: "voice-engine",
  callState: "call-state",
  callLine: "call-line",
} as const;

export const NOT_YET = "no disponible todavía";

/** The error in the owner's words; a command Rust doesn't have is "no disponible todavía". */
export function cmdErrorText(err: unknown): string {
  const raw = String(err).replace(/^Error:\s*/, "");
  if (/not found|unknown command|not allowed|no disponible|command .* not/i.test(raw)) return NOT_YET;
  return raw || NOT_YET;
}

/** invoke() that fails with a readable Spanish message, also outside Tauri. */
export async function callCmd<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!IS_TAURI) throw new Error(NOT_YET);
  try {
    return await invoke<T>(cmd, args);
  } catch (err) {
    throw new Error(cmdErrorText(err));
  }
}

// ── Payloads ──────────────────────────────────────────────────────────────────

export interface MeetingStateEvent { active: boolean; bot?: string; since?: number }
export interface MeetingChunkEvent { bot: string; text: string; ts?: number }
export interface ScreenShareEvent { active: boolean; bot?: string }
export interface VoiceEngineEvent {
  ready: boolean;
  engine?: string;
  downloading?: boolean;
  pct?: number;
  /** What is downloading: `piper`, `es_MX-claude-high`, `ggml-small.bin`… */
  item?: string;
  /** A failed download, or a retry pending while `downloading` stays true. */
  error?: string;
}

export interface CallParticipant { id: string; name: string; color: string }
export interface CallStateEvent {
  active: boolean;
  participants: CallParticipant[];
  muted: boolean;
  phase: "listening" | "hearing" | "thinking" | "speaking";
  /** Participant id of who is thinking or speaking. */
  speaker?: string;
  error?: string;
}
/** One line of the call transcript: `who` is "me" or a participant id. */
export interface CallLineEvent { who: string; name: string; text: string }

export interface VoiceInfo {
  id: string;
  engine: string;
  name: string;
  lang: string;
  installed: boolean;
  sizeMb?: number;
}

// ── "Leer en voz alta los avisos del agente de Cursor" ──
// settings.speakCursor first; mirrored in localStorage in case the Rust
// settings struct doesn't carry the field yet (it would be dropped on save).

const SPEAK_CURSOR_KEY = "coucou.voice.speakCursor";

export function speakCursorEnabled(fromSettings: boolean | undefined): boolean {
  if (typeof fromSettings === "boolean") return fromSettings;
  try {
    return window.localStorage.getItem(SPEAK_CURSOR_KEY) !== "0";
  } catch {
    return true;
  }
}

export function mirrorSpeakCursor(on: boolean) {
  try {
    window.localStorage.setItem(SPEAK_CURSOR_KEY, on ? "1" : "0");
  } catch {
    // Only the fallback copy is lost.
  }
}

// ── "Leer respuestas en voz alta" ─────────────────────────────────────────────

const SPEAK_KEY = "coucou.voice.readReplies";

export function readRepliesEnabled(): boolean {
  try {
    return window.localStorage.getItem(SPEAK_KEY) === "1";
  } catch {
    return false;
  }
}

export function setReadReplies(on: boolean) {
  try {
    window.localStorage.setItem(SPEAK_KEY, on ? "1" : "0");
  } catch {
    // Storage unavailable: the setting just doesn't stick.
  }
}

/** Reads a Bot's answer aloud in that Bot's voice (`bot`: its id). */
export function speak(text: string, bot?: string): Promise<void> {
  const clean = text.replace(/```[\s\S]*?```/g, " ").replace(/[*_`#>]/g, "").trim();
  if (!clean) return Promise.resolve();
  return callCmd<void>(CMD.speak, { text: clean.slice(0, 4000), bot: bot ?? null });
}
