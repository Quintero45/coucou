// What the Grok Bots are doing right now, as told by Rust events: the latest
// step of each, a meeting or screen share in progress, the voice engine. Only
// in memory; the conversation (botchat.ts) keeps the lasting part.

import { BOT_PREFIX, State, type AgentTask, type GrokBot } from "./state";
import { notYet, type MeetingStateEvent, type ScreenShareEvent, type VoiceEngineEvent } from "./botcmds";
import { t } from "../i18n/i18n";

/** The Bot an event names: by id first, then by name (case-insensitive). */
export function resolveBot(who: string | undefined | null): GrokBot | null {
  const key = (who ?? "").trim().toLowerCase();
  if (!key) return null;
  const bots = State.settings.grokBots;
  return bots.find((b) => b.id.toLowerCase() === key) ?? bots.find((b) => b.name.toLowerCase() === key) ?? null;
}

const steps = new Map<string, { text: string; ts: number }>();

export type AvatarState = "reposo" | "pensando" | "recibido" | "pregunta" | "listo" | "error";

export const BotLive = {
  meeting: null as MeetingStateEvent | null,
  screenShare: null as ScreenShareEvent | null,
  /** "Pantalla" in Cursor's conversation: each order carries a screenshot taken as it is sent. */
  cursorScreen: false,
  /** Last voice-engine state per engine (whisper). */
  voice: new Map<string, VoiceEngineEvent>(),
  /** Text heard so far while dictating, per Bot slug. */
  dictation: new Map<string, string>(),
  /** Until when (ms) a Bot pill says «recibido» after a message reached it. */
  received: new Map<string, number>(),
  /** The Bot being dictated to (set by the conversation's microphone button). */
  dictatingSlug: null as string | null,

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
   * The avatar's mood (pills, cards, conversation header), as a data-bot-state
   * the CSS animates: recibido wins over the task state.
   */
  avatarState(task: AgentTask): AvatarState {
    if (task.state === "error") return "error";
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
    const v = BotLive.voice.get("whisper") ?? BotLive.voice.get("");
    if (!v || v.ready) return null;
    if (v.downloading) return t("Downloading model {pct}%", { pct: Math.round(v.pct ?? 0) });
    return notYet();
  },
};

// A Bot that is done, idle or failed has no live step any more.
State.subscribe(() => {
  for (const id of [...steps.keys()]) {
    const t = State.tasks.find((x) => x.id === id);
    if (!t || (t.state !== "working" && t.state !== "thinking" && t.state !== "searching")) steps.delete(id);
  }
});
