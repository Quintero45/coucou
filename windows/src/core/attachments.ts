// Files and pasted things waiting to go to a Grok Bot with the next message.
// One tray per Bot, shared by its conversation and its quick-reply box. Only
// in memory: the history (botchat.ts) keeps a sent file's name and size, never
// its bytes.

import { Bridge, type AttachmentIn } from "./bridge";
import { State } from "./state";

export type PendingAttachment =
  | { key: string; kind: "file"; id: string; name: string; mime: string; size: number }
  | { key: string; kind: "text"; name: string; mime: string; text: string; size: number }
  | { key: string; kind: "image"; name: string; mime: string; base64: string; size: number };

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
  if (/\btoo_large\b/.test(raw)) return "El archivo es demasiado grande";
  if (/\bnot_connected\b/.test(raw)) return "Este bot aún no está conectado";
  if (/\bempty_message\b/.test(raw)) return "El mensaje está vacío";
  const http = /\bhttp_(\d+)\b/.exec(raw);
  if (http) return http[1] === "0" ? "No se pudo contactar con el bot (red o tiempo agotado)" : `El bot no respondió (${http[1]})`;
  return raw;
}

/** Copies dropped files into the inbox and puts them in the Bot's tray. */
export async function attachPaths(slug: string, paths: string[]): Promise<boolean> {
  if (paths.length === 0) return false;
  Outbox.setNotice(slug, paths.length === 1 ? "Preparando el archivo…" : `Preparando ${paths.length} archivos…`);
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
    reader.onerror = () => Outbox.setNotice(slug, "No se pudo leer la imagen pegada");
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
