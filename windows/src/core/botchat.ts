// The conversation with each Grok Bot: what the owner sent, what the Bot said
// back (`coucou-hook --bot … --status …`), the steps it reported and the
// permissions it asked for. Kept in this webview's localStorage, per Bot,
// capped at LIMIT entries, so it survives a restart. Nothing secret goes here:
// permission arguments arrive already sanitised by botlog.ts.

import { Bridge } from "./bridge";
import { Outbox, botErrorText, toWire, type PendingAttachment } from "./attachments";
import type { BotDecision } from "./botlog";
import { readRepliesEnabled, speak } from "./botcmds";
import { State } from "./state";

export type BotChatEntry =
  | {
      id: string;
      kind: "me";
      at: number;
      text: string;
      status: "sending" | "sent" | "error";
      note?: string;
      /** What went with it: name and size only, never the bytes. */
      files?: { name: string; size: number }[];
    }
  | { id: string; kind: "bot"; at: number; text: string; status: "working" | "done" | "needs" | "error" }
  | { id: string; kind: "step"; at: number; text: string }
  /** A file the Bot sent back (bot-attach) or a meeting transcript. Path only, opened by Rust. */
  | { id: string; kind: "file"; at: number; path: string; name: string; mime: string; size: number; source?: "bot" | "meeting"; caption?: string }
  /** A piece of a meeting transcript (meeting-chunk). */
  | { id: string; kind: "transcript"; at: number; text: string }
  | {
      id: string;
      kind: "perm";
      at: number;
      tool: string;
      decision: BotDecision;
      target: string;
      toolInput?: unknown;
      truncated?: boolean;
      omitted?: boolean;
    };

/** Distributes Omit over the union, so each kind keeps its own fields. */
type NewEntry = BotChatEntry extends infer E ? (E extends BotChatEntry ? Omit<E, "id" | "at"> : never) : never;

/** Entries kept per Bot. */
export const LIMIT = 200;

const storageKey = (slug: string) => `coucou.botchat.${slug}`;
const cache = new Map<string, BotChatEntry[]>();
let counter = 0;

function load(slug: string): BotChatEntry[] {
  const hit = cache.get(slug);
  if (hit) return hit;
  let list: BotChatEntry[] = [];
  try {
    const raw = window.localStorage.getItem(storageKey(slug));
    const parsed: unknown = raw ? JSON.parse(raw) : [];
    if (Array.isArray(parsed)) {
      list = parsed.filter(
        (e): e is BotChatEntry => !!e && typeof e === "object" && typeof (e as BotChatEntry).id === "string" &&
          typeof (e as BotChatEntry).at === "number" && typeof (e as BotChatEntry).kind === "string",
      );
    }
  } catch {
    list = [];
  }
  // A message still "sending" when the app closed never got its answer.
  for (const e of list) if (e.kind === "me" && e.status === "sending") e.status = "error";
  cache.set(slug, list);
  return list;
}

function save(slug: string) {
  try {
    window.localStorage.setItem(storageKey(slug), JSON.stringify(load(slug)));
  } catch {
    // Storage full or unavailable: the conversation still works for this session.
  }
}

export const BotChat = {
  /** Oldest first. */
  list(slug: string): readonly BotChatEntry[] {
    return load(slug);
  },

  add(slug: string, entry: NewEntry): BotChatEntry {
    const list = load(slug);
    const full = { ...entry, id: `${Date.now().toString(36)}-${(counter++).toString(36)}`, at: Date.now() } as BotChatEntry;
    list.push(full);
    if (list.length > LIMIT) list.splice(0, list.length - LIMIT);
    save(slug);
    State.notify();
    // "Leer respuestas en voz alta" (Ajustes → Voz).
    if (full.kind === "bot" && full.status !== "working" && readRepliesEnabled()) {
      speak(full.text, slug).catch((err) => void Bridge.log(`speak ${slug} failed: ${String(err)}`));
    }
    return full;
  },

  /**
   * A live step (bot-step): repeated or quick-fire steps collapse into one
   * entry, so a chatty Bot doesn't push the conversation out of the history.
   */
  addStep(slug: string, text: string) {
    const list = load(slug);
    const last = list.at(-1);
    const clean = text.replace(/\s+/g, " ").trim().slice(0, 200);
    if (!clean) return;
    if (last?.kind === "step") {
      if (last.text === clean) return;
      if (Date.now() - last.at < 2500) {
        last.text = clean;
        last.at = Date.now();
        save(slug);
        State.notify();
        return;
      }
    }
    BotChat.add(slug, { kind: "step", text: clean });
  },

  /** Changes a message in place (its delivery status, an error note). */
  update(slug: string, id: string, patch: { status?: "sending" | "sent" | "error"; note?: string }) {
    const e = load(slug).find((x) => x.id === id);
    if (!e || e.kind !== "me") return;
    Object.assign(e, patch);
    save(slug);
    State.notify();
  },
};

/**
 * Sends a message to a Grok Bot through the existing path (grokbot_send: POST
 * to the routine's webhook from settings, with its key from the Credential
 * Manager) and records it in the conversation with its delivery status.
 */
export async function sendToBot(
  slug: string,
  text: string,
  attachments: readonly PendingAttachment[] = [],
): Promise<{ ok: boolean; message: string }> {
  const files = attachments.map((a) => ({ name: a.name, size: a.size }));
  const entry = BotChat.add(slug, { kind: "me", text, status: "sending", ...(files.length ? { files } : {}) });
  try {
    const message = await Bridge.grokbotSend(slug, text, attachments.length ? attachments.map(toWire) : undefined);
    BotChat.update(slug, entry.id, { status: "sent" });
    return { ok: true, message };
  } catch (err) {
    // too_large, not_connected, http_N… in the owner's words, under the message.
    const message = botErrorText(err);
    BotChat.update(slug, entry.id, { status: "error", note: message });
    return { ok: false, message };
  }
}

/**
 * Sends the text with whatever waits in the Bot's tray (dropped files, pasted
 * images or long texts). The tray empties on the way out and gets its items
 * back if the send fails.
 */
export async function sendWithOutbox(slug: string, text: string): Promise<{ ok: boolean; message: string }> {
  const items = [...Outbox.list(slug)];
  if (items.length) Outbox.remove(slug, items.map((a) => a.key));
  // Context notes (window, selection, clipboard) travel inside the message.
  const notes = items.filter((a): a is Extract<PendingAttachment, { kind: "note" }> => a.kind === "note");
  const files = items.filter((a) => a.kind !== "note");
  const body = notes.length
    ? [text, "", "[Contexto]", ...notes.map((n) => `${n.name}:\n${n.text}`)].join("\n").trim()
    : text;
  const todos = parseTodos(body);
  const r = todos != null ? await sendToAll(todos, files) : await sendToBot(slug, body, files);
  if (!r.ok && items.length) Outbox.restore(slug, items);
  return r;
}

/** "@todos mensaje" → "mensaje"; null when the message is for one Bot. */
export function parseTodos(text: string): string | null {
  const m = /^@todos\b[\s,:]*/i.exec(text);
  return m ? text.slice(m[0].length) : null;
}

/**
 * The same message to every Grok Bot (grokbot_send each), recorded in each
 * Bot's conversation. ok when at least one got it; message says how it went,
 * with each failure in Spanish.
 */
export async function sendToAll(
  text: string,
  attachments: readonly PendingAttachment[] = [],
): Promise<{ ok: boolean; message: string; sent: number; failed: { name: string; message: string }[] }> {
  const bots = State.settings.grokBots;
  void Bridge.log(`todos n=${bots.length} files=${attachments.length}`);
  if (bots.length === 0) return { ok: false, message: "No tienes Bots de Grok conectados", sent: 0, failed: [] };
  const results = await Promise.all(bots.map(async (b) => ({ bot: b, r: await sendToBot(b.id, text, attachments) })));
  const failed = results.filter((x) => !x.r.ok).map((x) => ({ name: x.bot.name, message: x.r.message }));
  const sent = results.length - failed.length;
  const head = `Enviado a ${sent} ${sent === 1 ? "bot" : "bots"}`;
  const message = failed.length ? `${head} · ${failed.map((f) => `${f.name}: ${f.message}`).join(" · ")}` : head;
  return { ok: sent > 0, message, sent, failed };
}
