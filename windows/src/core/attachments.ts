// Files and pasted things waiting to go to a Grok Bot with the next message.
// One tray per Bot, shared by its conversation and its quick-reply box. Only
// in memory: the history (botchat.ts) keeps a sent file's name and size, never
// its bytes.

import { Bridge, type AttachmentIn } from "./bridge";
import { State } from "./state";
import { cmdErrorText } from "./botcmds";
import { t } from "../i18n/i18n";

export type PendingAttachment =
  | { key: string; kind: "file"; id: string; name: string; mime: string; size: number }
  | { key: string; kind: "text"; name: string; mime: string; text: string; size: number }
  | { key: string; kind: "image"; name: string; mime: string; base64: string; size: number }
  /** A piece of captured context (window, selection, clipboard): goes inside the message text. */
  | { key: string; kind: "note"; name: string; text: string; size: number };

type NewAttachment = PendingAttachment extends infer A ? (A extends PendingAttachment ? Omit<A, "key"> : never) : never;

/** Pasted text longer than this becomes an attachment instead of filling the box. */
export const LONG_PASTE = 2000;

const trays = new Map<string, PendingAttachment[]>();
const notices = new Map<string, string>();
let counter = 0;

export const Outbox = {
  list(slug: string): readonly PendingAttachment[] {
    return trays.get(slug) ?? [];
  },
  add(slug: string, a: NewAttachment) {
    const list = trays.get(slug) ?? [];
    list.push({ ...a, key: `a${(counter++).toString(36)}` } as PendingAttachment);
    trays.set(slug, list);
    State.notify();
  },
  remove(slug: string, keys: readonly string[]) {
    const list = (trays.get(slug) ?? []).filter((a) => !keys.includes(a.key));
    if (list.length) trays.set(slug, list);
    else trays.delete(slug);
    State.notify();
  },
  /** Puts items back in front (a send that failed). */
  restore(slug: string, items: readonly PendingAttachment[]) {
    trays.set(slug, [...items, ...(trays.get(slug) ?? [])]);
    State.notify();
  },
  /** A line shown under the box: preparing files, or why they could not be added. */
  notice(slug: string): string | null {
    return notices.get(slug) ?? null;
  },
  setNotice(slug: string, text: string | null) {
    if (text) notices.set(slug, text);
    else notices.delete(slug);
    State.notify();
  },
};

/** What grokbot_send receives: inbox files by id, pasted things inline. */
export function toWire(a: PendingAttachment): AttachmentIn {
  if (a.kind === "file") return { id: a.id };
  if (a.kind === "note") return { name: a.name, mime: "text/plain", text: a.text };
  if (a.kind === "text") return { name: a.name, mime: a.mime, text: a.text };
  return { name: a.name, mime: a.mime, base64: a.base64 };
}

export function formatSize(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(bytes < 10 * 1024 ? 1 : 0)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/** The send and copy errors, in the owner's words. */
export function botErrorText(err: unknown): string {
  const raw = String(err).replace(/^Error:\s*/, "");
  if (/\btoo_large\b/.test(raw)) return t("The file is too large");
  if (/\bnot_connected\b/.test(raw)) return t("This bot isn't connected yet");
  if (/\bempty_message\b/.test(raw)) return t("The message is empty");
  const http = /\bhttp_(\d+)\b/.exec(raw);
  if (http) return http[1] === "0" ? t("Couldn't reach the bot (network or timeout)") : t("The bot didn't answer ({status})", { status: http[1] });
  return raw;
}

/** Copies dropped files into the inbox and puts them in the Bot's tray. */
export async function attachPaths(slug: string, paths: string[]): Promise<boolean> {
  if (paths.length === 0) return false;
  Outbox.setNotice(slug, paths.length === 1 ? t("Preparing the file…") : t("Preparing files: {count}…", { count: paths.length }));
  try {
    const files = await Bridge.ingestFiles(paths);
    Outbox.setNotice(slug, null);
    for (const f of files) Outbox.add(slug, { kind: "file", id: f.id, name: f.name, mime: f.mime, size: f.size });
    return true;
  } catch (err) {
    Outbox.setNotice(slug, botErrorText(err));
    return false;
  }
}

const stamp = () => {
  const d = new Date();
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getHours())}${p(d.getMinutes())}${p(d.getSeconds())}`;
};

/**
 * Paste into a Bot's box: an image becomes a base64 attachment, a long text a
 * text attachment. Anything else is left to the box.
 */
export function handleBotPaste(e: ClipboardEvent, slug: string) {
  const data = e.clipboardData;
  if (!data || !slug) return;
  const image = Array.from(data.items).find((i) => i.kind === "file" && i.type.startsWith("image/"));
  const file = image?.getAsFile();
  if (file) {
    e.preventDefault();
    const reader = new FileReader();
    reader.onload = () => {
      const url = String(reader.result ?? "");
      const base64 = url.slice(url.indexOf(",") + 1);
      const ext = (file.type.split("/")[1] || "png").replace("jpeg", "jpg").replace(/\W.*$/, "");
      Outbox.add(slug, {
        kind: "image",
        name: file.name && file.name !== "image.png" ? file.name : `imagen-${stamp()}.${ext}`,
        mime: file.type || "image/png",
        base64,
        size: file.size,
      });
    };
    reader.onerror = () => Outbox.setNotice(slug, t("Couldn't read the pasted image"));
    reader.readAsDataURL(file);
    return;
  }
  const text = data.getData("text/plain");
  if (text.length > LONG_PASTE) {
    e.preventDefault();
    Outbox.add(slug, {
      kind: "text",
      name: `texto-pegado-${stamp()}.txt`,
      mime: "text/plain",
      text,
      size: new Blob([text]).size,
    });
  }
}

/**
 * "Contexto": asks Rust what is on screen (capture_context sends nothing) and
 * lays it out as chips in the Bot's tray — the screenshot as a file, the
 * window, selection and clipboard as notes — for the owner to review, trim or
 * pull into the box before sending.
 */
export async function attachContext(slug: string): Promise<string | null> {
  Outbox.setNotice(slug, t("Capturing the context…"));
  try {
    const c = await Bridge.captureContext().catch((err: unknown) => {
      throw new Error(cmdErrorText(err));
    });
    Outbox.setNotice(slug, null);
    const note = (name: string, text: string | null | undefined) => {
      const body = (text ?? "").trim();
      if (body) Outbox.add(slug, { kind: "note", name, text: body, size: new Blob([body]).size });
    };
    const win = [c.window_title, c.process_name ? `(${c.process_name})` : ""].filter(Boolean).join(" ");
    note(t("Window"), win);
    note(t("Selected text"), c.selected_text);
    note(t("Clipboard"), c.clipboard_text);
    const shot = c.screenshot;
    if (shot?.id) Outbox.add(slug, { kind: "file", id: shot.id, name: shot.name, mime: shot.mime, size: shot.size });
    return null;
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    Outbox.setNotice(slug, t("Context: {error}", { error: msg }));
    return msg;
  }
}
