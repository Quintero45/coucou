// The conversation with each Grok Bot: what the owner sent, what the Bot said
// back (`coucou-hook --bot … --status …`), the steps it reported and the
// permissions it asked for. Kept in this webview's localStorage, per Bot,
// capped at LIMIT entries, so it survives a restart. Nothing secret goes here:
// permission arguments arrive already sanitised by botlog.ts.

import { Bridge } from "./bridge";
import { Outbox, botErrorText, toWire, type PendingAttachment } from "./attachments";
import type { BotDecision } from "./botlog";
import type { FileDiff } from "./diff";
import { CMD, callCmd, playSound } from "./botcmds";
import { BotLive } from "./botlive";
import { BOT_PREFIX, CURSOR_AGENT_ID, State } from "./state";
import { t } from "../i18n/i18n";

/** The Cursor agent's conversation: its prompts, steps and answers, read from its hooks. */
export const CURSOR_CHAT = "cursor";

/** Pills that open a conversation: the Grok Bots and the Cursor agent. */
export function hasChat(taskId: string): boolean {
  return taskId.startsWith(BOT_PREFIX) || taskId === CURSOR_AGENT_ID;
}

/** The conversation key of a pill: a Bot's slug, or "cursor". */
export function chatSlug(taskId: string): string {
  return taskId === CURSOR_AGENT_ID ? CURSOR_CHAT : taskId.slice(BOT_PREFIX.length);
}

export type BotChatEntry =
  | {
      id: string;
      kind: "me";
      at: number;
      text: string;
      /** "queued": an order waiting for Cursor to finish its turn (cursorlink.rs). */
      status: "sending" | "sent" | "error" | "queued";
      note?: string;
      /** What went with it: name and size only, never the bytes. */
      files?: { name: string; size: number }[];
      /** An order to Cursor that carried a screenshot. */
      screen?: boolean;
      /** Sent from the island to Cursor (cursor_send); set to "seen" once its prompt came back through the hooks. */
      origin?: "island" | "seen";
    }
  | {
      id: string; kind: "bot"; at: number; text: string; status: "working" | "done" | "needs" | "error";
      /** A question with choices (botcmds.ts QUESTION_OPTIONS), answered as a normal message. */
      options?: { label: string; description?: string }[];
      allowCustom?: boolean;
    }
  | { id: string; kind: "step"; at: number; text: string }
  /** A file the agent edited (afterFileEdit, PostToolUse Edit): a capped copy of its diff. */
  | { id: string; kind: "edit"; at: number; path: string; added: number; removed: number; lines: EditLine[]; more: number; tooLarge?: boolean }
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

/** One line of an edit: added, removed, unchanged, or "…" between two hunks. */
export interface EditLine { k: "+" | "-" | " " | "…"; t: string }

/** An edit lives in localStorage with the rest: only its first lines, each cut short. */
export const EDIT_LINES = 40;
const EDIT_LINE_CHARS = 200;

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

  /** A file the agent edited, shown as code in the conversation. */
  addEdit(slug: string, diff: FileDiff) {
    const lines: EditLine[] = [];
    let total = 0;
    const keep = (line: EditLine) => {
      total++;
      if (lines.length < EDIT_LINES) lines.push(line);
    };
    diff.hunks.forEach((hunk, i) => {
      if (i > 0) keep({ k: "…", t: "" });
      for (const l of hunk.lines) {
        keep({ k: l.kind === "added" ? "+" : l.kind === "removed" ? "-" : " ", t: l.text.slice(0, EDIT_LINE_CHARS) });
      }
    });
    BotChat.add(slug, {
      kind: "edit", path: diff.path, added: diff.added, removed: diff.removed, lines, more: total - lines.length,
      ...(diff.tooLarge ? { tooLarge: true } : {}),
    });
  },

  /** Changes a message in place (its delivery status, an error note). */
  update(slug: string, id: string, patch: { status?: "sending" | "sent" | "error" | "queued"; note?: string; origin?: "island" | "seen" }) {
    const e = load(slug).find((x) => x.id === id);
    if (!e || e.kind !== "me") return;
    Object.assign(e, patch);
    save(slug);
    State.notify();
  },
};

const queuedNote = () => t("Queued: Cursor gets it when it finishes what it's doing.");

/**
 * An order for the Cursor agent (cursorlink.rs): while it works it waits for
 * its next stop; otherwise it is typed into Cursor's chat at once.
 */
export async function sendToCursor(text: string, screen: boolean, busy: boolean): Promise<void> {
  const items = [...Outbox.list(CURSOR_CHAT)];
  if (!text && items.length === 0) return;
  const files = items.map((a) => ({ name: a.name, size: a.size }));
  Outbox.remove(CURSOR_CHAT, items.map((a) => a.key));
  const entry = BotChat.add(CURSOR_CHAT, {
    kind: "me", text, status: "sending", origin: "island", ...(files.length ? { files } : {}), ...(screen ? { screen } : {}),
  });
  try {
    const how = await callCmd<string>(CMD.cursorSend, { id: entry.id, text, screen, busy, attachments: items.map(toWire) });
    BotChat.update(CURSOR_CHAT, entry.id, how === "queued" ? { status: "queued", note: queuedNote() } : { status: "sent", note: "" });
  } catch (err) {
    Outbox.restore(CURSOR_CHAT, items);
    BotChat.update(CURSOR_CHAT, entry.id, { status: "error", note: err instanceof Error ? err.message : String(err) });
  }
}

/** A queued order, typed into Cursor's chat now. */
export async function cursorOrderNow(id: string): Promise<void> {
  BotChat.update(CURSOR_CHAT, id, { status: "sending" });
  try {
    await callCmd(CMD.cursorOrderNow, { id });
    BotChat.update(CURSOR_CHAT, id, { status: "sent", note: "" });
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    // "Esa orden ya salió.": no longer queued, a stop already took it.
    if (ORDER_GONE.test(msg)) BotChat.update(CURSOR_CHAT, id, { status: "sent", note: msg });
    else BotChat.update(CURSOR_CHAT, id, { status: "queued", note: msg });
  }
}

/** cursorlink.rs: the order already left for Cursor (it was sent, not lost). */
const ORDER_GONE = /^Esa orden ya salió(?: hacia Cursor)?\.$/;

export async function cursorOrderCancel(id: string): Promise<void> {
  try {
    await callCmd(CMD.cursorOrderCancel, { id });
    BotChat.update(CURSOR_CHAT, id, { status: "error", note: t("Taken out of the queue: not sent.") });
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    // "Esa orden ya salió hacia Cursor.": it was sent, say so as is.
    if (ORDER_GONE.test(msg)) BotChat.update(CURSOR_CHAT, id, { status: "sent", note: msg });
    // Still queued as far as we know: saying "no se envió" could be false.
    else BotChat.update(CURSOR_CHAT, id, { status: "queued", note: t("Couldn't take it out of the queue: {error}", { error: msg }) });
  }
}

/** cursor-order: a queued order reached Cursor, or Cursor stopped without taking it. */
export function cursorOrderEvent(p: { id: string; state: string }) {
  if (p.state === "sent") BotChat.update(CURSOR_CHAT, p.id, { status: "sent", note: "" });
  else BotChat.update(CURSOR_CHAT, p.id, { note: t("Cursor stopped without getting it. Tap “Send now” to type it into its chat.") });
}

/**
 * Sends a message to a Grok Bot through the existing path (grokbot_send: POST
 * to the routine's webhook from settings, with its key from the Credential
 * Manager) and records it in the conversation with its delivery status.
 */
/**
 * What a failed message carried, so «Reintentar» can send it again as it was
 * (in memory only: after a restart the retry goes without its attachments).
 */
const retryAttachments = new Map<string, readonly PendingAttachment[]>();

/** grokbot_send for one «me» entry: «Enviando…» → «Recibido», or the error and «Reintentar». */
async function deliverEntry(
  slug: string,
  entryId: string,
  text: string,
  attachments: readonly PendingAttachment[],
  quiet: boolean,
): Promise<{ ok: boolean; message: string }> {
  try {
    const message = await Bridge.grokbotSend(slug, text, attachments.length ? attachments.map(toWire) : undefined);
    retryAttachments.delete(entryId);
    BotChat.update(slug, entryId, { status: "sent", note: undefined });
    // The pill says «recibido» for a moment; a ding unless it's one of many (sendToAll).
    BotLive.markReceived(`${BOT_PREFIX}${slug}`);
    if (!quiet) playSound("recibido", State.settings);
    return { ok: true, message };
  } catch (err) {
    // too_large, not_connected, http_N… in the owner's words, under the message.
    const message = botErrorText(err);
    if (attachments.length) retryAttachments.set(entryId, attachments);
    BotChat.update(slug, entryId, { status: "error", note: message });
    if (!quiet) playSound("error", State.settings);
    return { ok: false, message };
  }
}

export async function sendToBot(
  slug: string,
  text: string,
  attachments: readonly PendingAttachment[] = [],
  quiet = false,
): Promise<{ ok: boolean; message: string }> {
  const files = attachments.map((a) => ({ name: a.name, size: a.size }));
  const entry = BotChat.add(slug, { kind: "me", text, status: "sending", ...(files.length ? { files } : {}) });
  return deliverEntry(slug, entry.id, text, attachments, quiet);
}

/** «Reintentar» under a message that didn't leave: the same entry goes again. */
export async function retryToBot(slug: string, entryId: string): Promise<{ ok: boolean; message: string }> {
  const entry = BotChat.list(slug).find((e) => e.id === entryId);
  if (!entry || entry.kind !== "me" || entry.status !== "error") return { ok: false, message: "" };
  BotChat.update(slug, entryId, { status: "sending", note: undefined });
  void Bridge.log(`bot retry bot=${slug}`);
  return deliverEntry(slug, entryId, entry.text, retryAttachments.get(entryId) ?? [], false);
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
 * with each failure in the owner's words.
 */
export async function sendToAll(
  text: string,
  attachments: readonly PendingAttachment[] = [],
): Promise<{ ok: boolean; message: string; sent: number; failed: { name: string; message: string }[] }> {
  const bots = State.settings.grokBots;
  void Bridge.log(`todos n=${bots.length} files=${attachments.length}`);
  if (bots.length === 0) return { ok: false, message: t("You have no Grok Bots connected"), sent: 0, failed: [] };
  const results = await Promise.all(bots.map(async (b) => ({ bot: b, r: await sendToBot(b.id, text, attachments, true) })));
  const failed = results.filter((x) => !x.r.ok).map((x) => ({ name: x.bot.name, message: x.r.message }));
  const sent = results.length - failed.length;
  // One sound for the whole round, not one per Bot.
  playSound(sent > 0 ? "recibido" : "error", State.settings);
  const head = t("Sent to bots: {count}", { count: sent });
  const message = failed.length ? `${head} · ${failed.map((f) => `${f.name}: ${f.message}`).join(" · ")}` : head;
  return { ok: sent > 0, message, sent, failed };
}
