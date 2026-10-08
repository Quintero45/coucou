// Rust → island events for the Grok Bots: live steps, files they send back,
// meetings, screen sharing, dictation and the voice engine. Registered once at
// launch (Island constructor), whatever the island is showing.

import { Bridge, onEvent } from "../core/bridge";
import { BotChat, cursorOrderEvent } from "../core/botchat";
import { BotLive, resolveBot } from "../core/botlive";
import {
  EVT,
  type BotAttachEvent, type BotStepEvent, type MeetingChunkEvent,
  type MeetingStateEvent, type ScreenShareEvent, type VoiceEngineEvent,
} from "../core/botcmds";
import { BOT_PREFIX, State } from "../core/state";

/** The Bot of an event: its `bot` field, else the pill id in `agent`. */
const botOf = (p: { bot?: string; agent?: string } | null | undefined) =>
  resolveBot(p?.bot) ?? resolveBot(p?.agent?.startsWith(BOT_PREFIX) ? p.agent.slice(BOT_PREFIX.length) : null);

let registered = false;

export function registerBotEvents() {
  if (registered) return;
  registered = true;

  void onEvent<BotStepEvent>(EVT.botStep, (p) => {
    const bot = botOf(p);
    const text = String(p?.text ?? "").replace(/\s+/g, " ").trim();
    if (!bot || !text) return;
    void Bridge.log(`step bot=${bot.id} ${text.slice(0, 80)}`);
    const id = BOT_PREFIX + bot.id;
    const t = State.tasks.find((x) => x.id === id);
    // A step means it is working, whatever the pill said before.
    if (t && t.state !== "working" && t.state !== "thinking" && t.state !== "searching") State.updateTask(id, "working");
    if (t) State.appendStep(id, text.slice(0, 60));
    BotLive.setStep(bot.id, text, typeof p.ts === "number" ? p.ts : Date.now());
    BotChat.addStep(bot.id, text);
  });

  void onEvent<BotAttachEvent>(EVT.botAttach, (p) => {
    const bot = botOf(p);
    if (!bot || !p.path) return;
    const name = p.name || p.path.split(/[\\/]/).pop() || "archivo";
    void Bridge.log(`attach bot=${bot.id} name=${name} size=${p.size ?? 0}`);
    BotChat.add(bot.id, {
      kind: "file", path: p.path, name, mime: p.mime || "application/octet-stream", size: Number(p.size) || 0, source: "bot",
      ...(p.caption ? { caption: String(p.caption).slice(0, 200) } : {}),
    });
  });

  void onEvent<MeetingStateEvent>(EVT.meetingState, (p) => {
    BotLive.meeting = p ?? null;
    void Bridge.log(`meeting active=${!!p?.active} bot=${p?.bot ?? "-"}`);
    State.notify();
  });

  void onEvent<MeetingChunkEvent>(EVT.meetingChunk, (p) => {
    const bot = resolveBot(p?.bot);
    const text = String(p?.text ?? "").trim();
    if (!bot || !text) return;
    BotChat.add(bot.id, { kind: "transcript", text: text.slice(0, 2000) });
  });

  void onEvent<ScreenShareEvent>(EVT.screenShare, (p) => {
    BotLive.screenShare = p ?? null;
    void Bridge.log(`screen-share active=${!!p?.active} bot=${p?.bot ?? "-"}`);
    State.notify();
  });

  void onEvent<VoiceEngineEvent>(EVT.voiceEngine, (p) => {
    if (p) BotLive.voice.set(String(p.engine ?? ""), p);
    State.notify();
  });

  void onEvent<{ text?: string }>(EVT.dictationPartial, (p) => {
    const slug = BotLive.dictatingSlug;
    if (!slug) return;
    BotLive.dictation.set(slug, String(p?.text ?? ""));
    State.notify();
  });

  void onEvent<{ id: string; state: string }>(EVT.cursorOrder, (p) => {
    if (p?.id) cursorOrderEvent(p);
  });
}
