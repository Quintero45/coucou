// What the Grok Bots are doing right now, as told by Rust events: the latest
// step of each, a meeting or screen share in progress, the voice engine. Only
// in memory; the conversation (botchat.ts) keeps the lasting part.

import { BOT_PREFIX, State, type GrokBot } from "./state";
import type { MeetingStateEvent, ScreenShareEvent, VoiceEngineEvent } from "./botcmds";

/** The Bot an event names: by id first, then by name (case-insensitive). */
export function resolveBot(who: string | undefined | null): GrokBot | null {
  const key = (who ?? "").trim().toLowerCase();
  if (!key) return null;
  const bots = State.settings.grokBots;
  return bots.find((b) => b.id.toLowerCase() === key) ?? bots.find((b) => b.name.toLowerCase() === key) ?? null;
}

const steps = new Map<string, { text: string; ts: number }>();

export const BotLive = {
  meeting: null as MeetingStateEvent | null,
  screenShare: null as ScreenShareEvent | null,
  /** Last voice-engine state per engine (piper, whisper…). */
  voice: new Map<string, VoiceEngineEvent>(),
  /** Text heard so far while dictating, per Bot slug. */
  dictation: new Map<string, string>(),
  /** The Bot being dictated to (set by the conversation's microphone button). */
  dictatingSlug: null as string | null,

  /** Latest step of a Bot (pill id), while it works. */
  step(taskId: string): { text: string; ts: number } | null {
    return steps.get(taskId) ?? null;
  },
  setStep(slug: string, text: string, ts = Date.now()) {
    steps.set(BOT_PREFIX + slug, { text, ts });
    State.notify();
  },
  clearStep(taskId: string) {
    if (steps.delete(taskId)) State.notify();
  },

  meetingWith(slug: string): boolean {
    const m = BotLive.meeting;
    return !!m?.active && (!m.bot || resolveBot(m.bot)?.id === slug);
  },
  sharingWith(slug: string): boolean {
    const s = BotLive.screenShare;
    return !!s?.active && (!s.bot || resolveBot(s.bot)?.id === slug);
  },
  /** Why the microphone / meeting buttons are off, or null when they can be used. */
  voiceBlocked(): string | null {
    // Dictation and meetings need Whisper; a Piper download doesn't block them.
    const v = BotLive.voice.get("whisper") ?? BotLive.voice.get("");
    if (!v || v.ready) return null;
    if (v.downloading) return `Descargando modelo ${Math.round(v.pct ?? 0)}%`;
    return "no disponible todavía";
  },
};

// A Bot that is done, idle or failed has no live step any more.
State.subscribe(() => {
  for (const id of [...steps.keys()]) {
    const t = State.tasks.find((x) => x.id === id);
    if (!t || (t.state !== "working" && t.state !== "thinking" && t.state !== "searching")) steps.delete(id);
  }
});
