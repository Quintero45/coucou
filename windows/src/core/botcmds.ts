// Commands and events of the second batch (voice, meetings, screen sharing,
// context, attachments coming back). The Rust side may not have them yet: every
// call goes through callCmd(), which turns "no such command" into
// "no disponible todavía". The names live here and only here, so renaming one
// on the Rust side is a one-line change.

import { invoke } from "@tauri-apps/api/core";
import { Bridge, IS_TAURI } from "./bridge";
import { BotLive } from "./botlive";

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
  // Orders to the Cursor agent from its conversation in the island.
  cursorSend: "cursor_send",
  cursorOrderNow: "cursor_order_now",
  cursorOrderCancel: "cursor_order_cancel",
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
  cursorOrder: "cursor-order",
} as const;

export const NOT_YET = "no disponible todavía";

// ── Choices in a question (contract with Aerys; names provisional) ───────────
// A question or approval (Grok Bot or Cursor) may carry
//   options: [{ label, description? }]   (plain strings accepted too)
//   allowCustom: boolean                  ("Otra respuesta" box only when true)
// at the top of the hook payload or inside tool_input. The choice goes back the
// same way as Permitir/Denegar: approval_decision with decision "allow" and the
// chosen label (or the typed text) in APPROVAL_ANSWER.field.
export const QUESTION_OPTIONS = {
  field: "options",
  allowCustom: "allowCustom",
  /** Buttons shown before "Más…". */
  visible: 6,
} as const;

export const APPROVAL_ANSWER = {
  cmd: "approval_decision",
  decision: "allow",
  field: "answer",
} as const;

export interface ChoiceOption {
  label: string;
  description?: string;
}

/** The options a payload carries (top level or in tool_input), or null. */
export function parseChoices(payload: Record<string, unknown> | null | undefined): { options: ChoiceOption[]; allowCustom: boolean } | null {
  if (!payload) return null;
  const input = (payload.tool_input && typeof payload.tool_input === "object" ? payload.tool_input : {}) as Record<string, unknown>;
  const raw = payload[QUESTION_OPTIONS.field] ?? input[QUESTION_OPTIONS.field];
  if (!Array.isArray(raw)) return null;
  const options: ChoiceOption[] = [];
  for (const o of raw.slice(0, 24)) {
    if (typeof o === "string" && o.trim()) options.push({ label: o.trim().slice(0, 80) });
    else if (o && typeof o === "object" && typeof (o as { label?: unknown }).label === "string") {
      const r = o as { label: string; description?: unknown };
      if (!r.label.trim()) continue;
      options.push({
        label: r.label.trim().slice(0, 80),
        ...(typeof r.description === "string" && r.description.trim() ? { description: r.description.trim().slice(0, 200) } : {}),
      });
    }
  }
  if (options.length === 0) return null;
  const custom = payload[QUESTION_OPTIONS.allowCustom] ?? input[QUESTION_OPTIONS.allowCustom] ?? payload.allow_custom ?? input.allow_custom;
  return { options, allowCustom: custom === true };
}

/** The error in the owner's words; a command Rust doesn't have is "no disponible todavía". */
/** Shown for any other failure; the original goes to coucou.log. */
export const CMD_FAILED = "No se pudo completar. El detalle quedó en el registro.";

/** grokbot/files error codes in Spanish (same words as attachments.ts botErrorText). */
function codeText(raw: string): string | null {
  if (/^too_large\b/.test(raw)) return "El archivo es demasiado grande";
  if (/^not_connected\b/.test(raw)) return "Este bot aún no está conectado";
  if (/^empty_message\b/.test(raw)) return "El mensaje está vacío";
  const http = /^http_(\d+)\b/.exec(raw);
  if (http) return http[1] === "0" ? "No se pudo contactar con el bot (red o tiempo agotado)" : `El bot no respondió (${http[1]})`;
  return null;
}

/** Tauri's own "no such command" (the Rust side doesn't have it yet). */
const MISSING_CMD = /^(?:command\s+\S+\s+not\s+found|unknown\s+command\b.*)$/i;
/** Already worded for the owner by Rust (Spanish): shown as is. */
const SPANISH = /[áéíóúñ¿¡]|\b(?:no|el|la|los|las|está|archivo|clave|todavía|conectad[oa])\b/i;

/**
 * The error in the owner's words. "no disponible todavía" only for a command
 * Rust doesn't have; a known code (too_large, not_connected, http_N…) in
 * Spanish; a message Rust already wrote in Spanish as is; anything else a
 * generic line (callCmd logs the original).
 */
export function cmdErrorText(err: unknown): string {
  const raw = String(err instanceof Error ? err.message : err).replace(/^Error:\s*/, "").trim();
  if (!raw) return CMD_FAILED;
  if (raw === NOT_YET || MISSING_CMD.test(raw)) return NOT_YET;
  const code = codeText(raw);
  if (code) return code;
  if (SPANISH.test(raw)) return raw;
  return CMD_FAILED;
}

/** invoke() that fails with a readable Spanish message, also outside Tauri. */
export async function callCmd<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!IS_TAURI) throw new Error(NOT_YET);
  try {
    return await invoke<T>(cmd, args);
  } catch (err) {
    const text = cmdErrorText(err);
    // The original, for whoever reads the log (never a key: set_tts_key's stays out).
    const raw = String(err instanceof Error ? err.message : err).replace(/\s+/g, " ").slice(0, 300);
    void Bridge.log(`cmd ${cmd} failed: ${cmd === CMD.setTtsKey ? "(detalle oculto)" : raw}`);
    throw new Error(text);
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

// settings.readReplies (default on), saved with saveSettingsMerged. Until
// settings.rs carries the field Rust drops it on save and every boot brings the
// default back, so the toggle writes the old localStorage key too and an
// explicit choice there wins: that also carries over a choice made before.
const SPEAK_KEY = "coucou.voice.readReplies";

export function readRepliesEnabled(fromSettings?: boolean): boolean {
  try {
    const stored = window.localStorage.getItem(SPEAK_KEY);
    if (stored === "1" || stored === "0") return stored === "1";
  } catch {
    // No storage: settings decide.
  }
  return typeof fromSettings === "boolean" ? fromSettings : true;
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
  const done = callCmd<void>(CMD.speak, { text: clean.slice(0, 4000), bot: bot ?? null });
  // «Hablando» on its avatar while it reads (estimated: Rust sends no playback events).
  if (bot) BotLive.markSpeaking(bot, clean.length, done);
  return done;
}

// ── "Escribir en Cursor" (cursorlink.rs gate card) ───────────────────────────

/** The approval card cursorlink.rs raises before typing an order into Cursor. */
export const CURSOR_WRITE = {
  tool: "Escribir en Cursor",
  /** tool_input field with the order as shown (≤1500 chars + a "(+N…)" tail). */
  field: "prompt",
} as const;

// ── «Sonidos de Coucou» (play_sound in Rust; Aerys confirms the name) ─────────

export const SOUND_CMD = {
  /** The Rust command: play_sound({ name }); Rust plays it at settings.soundVolume. */
  cmd: "play_sound",
  names: ["recibido", "pregunta", "listo", "error"],
} as const;

export type CoucouSound = (typeof SOUND_CMD.names)[number];

/** Set once Rust answered that play_sound doesn't exist: no more tries. */
let soundCmdMissing = false;

/** Plays a Coucou sound if settings.soundEnabled is on. Never throws, never shows anything. */
export function playSound(name: CoucouSound, prefs: { soundEnabled?: boolean }) {
  if (prefs.soundEnabled === false || soundCmdMissing || !IS_TAURI) return;
  invoke(SOUND_CMD.cmd, { name }).catch((err) => {
    const raw = String(err instanceof Error ? err.message : err);
    if (MISSING_CMD.test(raw.trim())) {
      soundCmdMissing = true;
      void Bridge.log(`sound ${SOUND_CMD.cmd} not available yet`);
    } else {
      void Bridge.log(`sound ${name} failed: ${raw.slice(0, 200)}`);
    }
  });
}
