// What the Grok Bots are doing right now, as told by Rust events: the latest
// step of each, a meeting or screen share in progress, the voice engine. Only
// in memory; the conversation (botchat.ts) keeps the lasting part.

import { BOT_PREFIX, CURSOR_AGENT_ID, State, type AgentTask, type GrokBot } from "./state";
import type { CallLineEvent, CallStateEvent, MeetingStateEvent, ScreenShareEvent, VoiceEngineEvent } from "./botcmds";

/** The Bot an event names: by id first, then by name (case-insensitive). */
export function resolveBot(who: string | undefined | null): GrokBot | null {
  const key = (who ?? "").trim().toLowerCase();
  if (!key) return null;
  const bots = State.settings.grokBots;
  return bots.find((b) => b.id.toLowerCase() === key) ?? bots.find((b) => b.name.toLowerCase() === key) ?? null;
}

const steps = new Map<string, { text: string; ts: number }>();

export type AvatarState = "reposo" | "pensando" | "hablando" | "recibido" | "pregunta" | "listo" | "error";

export const BotLive = {
  meeting: null as MeetingStateEvent | null,
  screenShare: null as ScreenShareEvent | null,
  /** "Pantalla" in Cursor's conversation: each order carries a screenshot taken as it is sent. */
  cursorScreen: false,
  /** Last voice-engine state per engine (piper, whisper…). */
  voice: new Map<string, VoiceEngineEvent>(),
  /** Text heard so far while dictating, per Bot slug. */
  dictation: new Map<string, string>(),
  /** Until when (ms) a Bot pill says «recibido» after a message reached it. */
  received: new Map<string, number>(),
  /** Until when (ms) a Bot is reading aloud (speak), per chat slug. */
  speaking: new Map<string, number>(),
  /** The Bot being dictated to (set by the conversation's microphone button). */
  dictatingSlug: null as string | null,
  /** The voice call in progress (null when there is none). */
  call: null as CallStateEvent | null,
  /** Its last lines, newest last. */
  callLines: [] as CallLineEvent[],
  /** Why the last call stopped or a participant could not answer. */
  callError: null as string | null,

  inCall(id: string): boolean {
    return !!BotLive.call?.participants.some((p) => p.id === id);
  },

  /** «Recibido» on the pill for a moment (the send resolved, no reply yet). */
  markReceived(taskId: string, ms = 2500) {
    BotLive.received.set(taskId, Date.now() + ms);
    State.notify();
    window.setTimeout(() => {
      if ((BotLive.received.get(taskId) ?? 0) <= Date.now()) {
        BotLive.received.delete(taskId);
        State.notify();
      }
    }, ms + 50);
  },
  isReceived(taskId: string): boolean {
    return (BotLive.received.get(taskId) ?? 0) > Date.now();
  },

  /**
   * Speaking (no playback events from Rust): from the speak() call for a
   * length-based estimate, or until the call settles if that is later.
   */
  markSpeaking(slug: string, chars: number, done: Promise<unknown>) {
    const until = Date.now() + Math.min(20_000, Math.max(1500, chars * 65));
    BotLive.speaking.set(slug, until);
    State.notify();
    const clear = () => {
      if ((BotLive.speaking.get(slug) ?? 0) <= Date.now()) {
        BotLive.speaking.delete(slug);
        State.notify();
      }
    };
    window.setTimeout(clear, until - Date.now() + 50);
    done.then(clear, () => {
      BotLive.speaking.delete(slug);
      State.notify();
    });
  },
  stopSpeaking(slug?: string) {
    if (slug) BotLive.speaking.delete(slug);
    else BotLive.speaking.clear();
    State.notify();
  },
  isSpeaking(slug: string): boolean {
    return (BotLive.speaking.get(slug) ?? 0) > Date.now();
  },

  /**
   * The avatar's mood (pills, cards, conversation header), as a data-bot-state
   * the CSS animates: hablando and recibido win over the task state.
   */
  avatarState(task: AgentTask): AvatarState {
    const slug = task.id.startsWith(BOT_PREFIX) ? task.id.slice(BOT_PREFIX.length) : task.id === CURSOR_AGENT_ID ? "cursor" : task.id;
    if (task.state === "error") return "error";
    if (BotLive.isSpeaking(slug)) return "hablando";
    if (BotLive.isReceived(task.id) && !["finished", "approval", "question"].includes(task.state)) return "recibido";
    switch (task.state) {
      case "working":
      case "thinking":
      case "searching":
        return "pensando";
      case "approval":
      case "question":
        return "pregunta";
      case "finished":
        return "listo";
      default:
        return "reposo";
    }
  },

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
    if (slug === "cursor") return BotLive.cursorScreen;
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
