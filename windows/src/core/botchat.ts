// The conversation with each Grok Bot: what the owner sent, what the Bot said
// back (`coucou-hook --bot … --status …`), the steps it reported and the
// permissions it asked for. Kept in this webview's localStorage, per Bot,
// capped at LIMIT entries, so it survives a restart. Nothing secret goes here:
// permission arguments arrive already sanitised by botlog.ts.

import { Bridge } from "./bridge";
import { Outbox, botErrorText, toWire, type PendingAttachment } from "./attachments";
import type { BotDecision } from "./botlog";
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
    return full;
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
  const r = await sendToBot(slug, text, items);
  if (!r.ok && items.length) Outbox.restore(slug, items);
  return r;
}
