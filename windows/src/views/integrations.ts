// Integration cards shown in the overview's left card — DOM ports of
// IntegrationCardView and friends from IslandViewContent.swift.
//
// Cal.com is the one simplification: macOS shows a three-level calendar
// (month → day → booking); here it is the list of upcoming bookings.

import { h, svg, clear, dot } from "./dom";
import { ICONS } from "./icons";
import { BOT_PREFIX, State, botPhase, type AgentTask } from "../core/state";
import { DECISION_LABELS, type BotApproval } from "../core/botlog";
import { BotChat, type BotChatEntry } from "../core/botchat";
import { Outbox, formatSize, handleBotPaste } from "../core/attachments";
import { createMiniBot, pruneMiniBots } from "../mochi/minibots";
import { Bridge } from "../core/bridge";

/** Same shape as the Swift `timeAgo` computed properties. */
export function timeAgo(value: unknown): string {
  const date = typeof value === "number" ? new Date(value) : new Date(String(value));
  const diff = (Date.now() - date.getTime()) / 1000;
  if (!Number.isFinite(diff)) return "";
  if (diff < 60) return "ahora";
  if (diff < 3600) return `${Math.floor(diff / 60)}m`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h`;
  return `${Math.floor(diff / 86400)}d`;
}

function header(color: string, name: string, kind: string, extra?: Node): HTMLElement {
  const row = h("div", { class: "int-head" }, dot(color, 7), h("b", { text: name }), h("span", { text: kind }));
  if (extra) row.append(extra);
  return row;
}

/** Highlighted first row + plain rows, the layout every list card shares. */
function listRow(accent: string, first: boolean, ...children: Node[]): HTMLElement {
  const row = h("div", { class: first ? "int-row first" : "int-row" }, dot(accent, 5), ...children);
  if (first) row.style.background = `${accent}14`;
  return row;
}

function get(id: string): Record<string, unknown> {
  return (State.integrations[id]?.data ?? {}) as Record<string, unknown>;
}

function arr(id: string, key: string): Record<string, unknown>[] {
  const v = get(id)[key];
  return Array.isArray(v) ? (v as Record<string, unknown>[]) : [];
}

// ── Not configured / idle ─────────────────────────────────────────────────────

const OPEN_URLS: Record<string, string> = {
  integration_resend: "https://resend.com/emails",
  integration_vercel: "https://vercel.com/dashboard",
  integration_github: "https://github.com",
  integration_stripe: "https://dashboard.stripe.com/payments",
  integration_notion: "https://notion.so",
  integration_calcom: "https://app.cal.com/bookings",
};

function idleCard(task: AgentTask, openSettings: () => void): HTMLElement {
  const info = State.integrations[task.id];
  const configured = info?.configured ?? false;
  const error = info?.error ?? null;
  // The Claude Code pill is about hooks, not a key — the macOS wording would be
  // misleading here.
  const missing = task.id === "integration_claude" ? "Hooks sin instalar" : "Falta la clave";
  const label = error ?? (configured ? "Conectado · cargando…" : missing);
  const statusColor = error || !configured ? "#F4505E" : "#22C55E";

  const actions = h("div", { class: "int-actions" });
  if (task.id === "integration_claude") {
    actions.append(
      h("button", {
        class: "link-btn",
        style: `color:${task.color}b3`,
        text: "Abrir Visual Studio Code",
        onclick: () => void Bridge.openInVSCode(task.sessionCwd ?? null),
      }),
    );
  } else if (task.id === "integration_n8n") {
    actions.append(
      h("button", {
        class: "link-btn",
        style: `color:${task.color}d9`,
        text: "Abrir n8n",
        onclick: () => void Bridge.openN8n(),
      }),
    );
  } else if (OPEN_URLS[task.id]) {
    actions.append(
      h("button", {
        class: "link-btn",
        style: `color:${task.color}d9`,
        text: `Abrir ${task.name}`,
        onclick: () => void Bridge.openUrl(OPEN_URLS[task.id]),
      }),
    );
  }
  if (configured) {
    actions.append(
      h("button", {
        class: "link-btn",
        style: `color:${task.color}d9`,
        text: "Actualizar",
        onclick: () => void Bridge.refreshIntegration(task.id),
      }),
    );
  } else {
    actions.append(
      h("button", { class: "link-btn", style: "color:#8e939c", text: "Ajustes…", onclick: openSettings }),
    );
  }

  return h(
    "div",
    { class: "int-card" },
    header(task.color, task.id === "integration_claude" ? "VS Code" : task.name, "Integración"),
    h("div", { class: "int-status" }, dot(statusColor, 5), h("span", { text: label })),
    actions,
  );
}

// ── Vercel ────────────────────────────────────────────────────────────────────

function vercelCard(onDetail: () => void): HTMLElement {
  const deployments = arr("integration_vercel", "deployments");
  const rows = h("div", { class: "int-rows" });
  deployments.slice(0, 3).forEach((d, i) => {
    const accent = d.state === "READY" ? "#22C55E" : "#F4505E";
    const name = h("span", { class: "int-name", text: String(d.projectName ?? "") });
    const ago = h("span", { class: "int-ago", text: timeAgo(d.createdAt) });
    if (i === 0) {
      const more = h(
        "button",
        { class: "int-more", title: "Detalles", onclick: onDetail },
        svg(ICONS.ellipsis, 8),
      );
      rows.append(listRow(accent, true, name, ago, more));
    } else {
      rows.append(listRow(accent, false, name, ago));
    }
  });
  return h("div", { class: "int-card" }, header("#7C5CFF", "Vercel", "Despliegues"), rows);
}

function vercelDetail(onBack: () => void): HTMLElement {
  const d = arr("integration_vercel", "deployments")[0] ?? {};
  const success = d.state === "READY";
  const accent = success ? "#22C55E" : "#F4505E";
  const status = success ? "Listo" : d.state === "CANCELED" ? "Cancelado" : "Error";
  const body = h("div", { class: "int-detail-body" });
  if (d.commitMessage) body.append(h("div", { class: "int-commit", text: String(d.commitMessage) }));
  const meta = h("div", { class: "int-meta" });
  if (d.branch) meta.append(h("span", { text: String(d.branch) }));
  meta.append(h("span", { text: `hace ${timeAgo(d.createdAt)}` }));
  body.append(meta);
  if (d.url) {
    body.append(
      h("button", {
        class: "int-link",
        text: String(d.url),
        onclick: () => void Bridge.openUrl(`https://${d.url}`),
      }),
    );
  }
  return h(
    "div",
    { class: "int-card detail" },
    h(
      "div",
      { class: "int-detail-head" },
      h("button", { class: "int-back", onclick: onBack }, svg(ICONS.chevronLeft, 10, { stroke: 2.4 })),
      dot(accent, 6),
      h("b", { text: String(d.projectName ?? "Despliegue") }),
      h("span", { class: "int-badge", style: `color:${accent};background:${accent}24`, text: status }),
    ),
    body,
  );
}

// ── Resend ────────────────────────────────────────────────────────────────────

function resendCard(): HTMLElement {
  const emails = arr("integration_resend", "emails");
  const total = get("integration_resend").total;
  const extra =
    total != null
      ? h("span", { class: "int-total" }, h("i", { class: "pulse" }), h("span", { text: String(total) }))
      : undefined;
  const rows = h("div", { class: "int-rows" });
  emails.slice(0, 3).forEach((e, i) => {
    const delivered = e.lastEvent === "delivered";
    const accent = delivered ? "#22C55E" : "#F4505E";
    const to = Array.isArray(e.to) ? String(e.to[0] ?? "?") : "?";
    const short = to.split("@")[0];
    const cells: Node[] = [
      h("span", { class: "int-name", text: short }),
      h("span", { class: "int-ago", text: timeAgo(e.createdAt) }),
    ];
    if (i === 0 && e.subject) cells.push(h("span", { class: "int-sub", text: String(e.subject) }));
    rows.append(listRow(accent, i === 0, ...cells));
  });
  return h("div", { class: "int-card" }, header("#22C55E", "Resend", "Correos", extra), rows);
}

// ── GitHub ────────────────────────────────────────────────────────────────────

function statRow(icon: string, color: string, label: string, value: string): HTMLElement {
  return h(
    "div",
    { class: "int-stat" },
    h("i", { class: "int-stat-icon", style: `color:${color}` }, svg(icon, 10)),
    h("span", { class: "int-stat-label", text: label }),
    h("span", { class: "int-stat-value", text: value }),
  );
}

function githubCard(): HTMLElement {
  const d = get("integration_github");
  const stars = Number(d.totalStars ?? 0);
  const repos = Number(d.totalRepos ?? 0);
  const fmt = (n: number) => (n >= 1000 ? `${(n / 1000).toFixed(1)}k` : String(n));
  return h(
    "div",
    { class: "int-card" },
    header("#F4505E", "GitHub", "Resumen"),
    h(
      "div",
      { class: "int-stats" },
      statRow(ICONS.star, "#F5A524", "Estrellas", fmt(stars)),
      statRow(ICONS.stack, "#6B7079", "Repositorios", String(repos)),
    ),
  );
}

// ── Stripe ────────────────────────────────────────────────────────────────────

function stripeCard(): HTMLElement {
  const d = get("integration_stripe");
  const balance = (Number(d.balance ?? 0) / 100).toFixed(2);
  const currency = String(d.currency ?? "eur").toUpperCase();
  const rows = h("div", { class: "int-rows tight" });
  for (const p of arr("integration_stripe", "payments")) {
    const success = p.status === "succeeded";
    const accent = success ? "#22C55E" : "#F4505E";
    rows.append(
      h(
        "div",
        { class: "int-row" },
        dot(accent, 5),
        h("span", { class: "int-name", text: String(p.description ?? "Pago") }),
        h("span", {
          class: "int-amount",
          style: "color:#22c55e",
          text: `+${(Number(p.amount ?? 0) / 100).toFixed(2)}`,
        }),
        h("span", { class: "int-ago", text: timeAgo(p.createdAt) }),
      ),
    );
  }
  return h(
    "div",
    { class: "int-card" },
    header("#0570DE", "Stripe", "Pagos"),
    h("div", { class: "int-balance" }, h("span", { text: balance }), h("i", { text: currency })),
    rows,
  );
}

// ── Notion ────────────────────────────────────────────────────────────────────

function notionCard(): HTMLElement {
  const rows = h("div", { class: "int-rows tight" });
  for (const p of arr("integration_notion", "pages").slice(0, 3)) {
    rows.append(
      h(
        "button",
        {
          class: "int-page",
          onclick: () => {
            if (typeof p.url === "string") void Bridge.openUrl(p.url);
          },
        },
        p.emoji
          ? h("span", { class: "int-emoji", text: String(p.emoji) })
          : h("i", { class: "int-emoji" }, svg(ICONS.doc, 9)),
        h("span", { class: "int-name", text: String(p.title ?? "Sin título") }),
        h("span", { class: "int-ago", text: timeAgo(p.lastEditedAt) }),
      ),
    );
  }
  return h("div", { class: "int-card" }, header("#E8E8E8", "Notion", "Recientes"), rows);
}

// ── Cal.com ───────────────────────────────────────────────────────────────────

function calcomCard(): HTMLElement {
  const bookings = arr("integration_calcom", "bookings")
    .slice()
    .sort((a, b) => new Date(String(a.start)).getTime() - new Date(String(b.start)).getTime());
  const rows = h("div", { class: "int-rows tight" });
  if (bookings.length === 0) {
    rows.append(h("div", { class: "int-empty", text: "No hay llamadas agendadas" }));
  }
  for (const b of bookings.slice(0, 3)) {
    const when = new Date(String(b.start));
    const day = when.toLocaleDateString(undefined, { day: "2-digit", month: "2-digit" });
    const time = when.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
    rows.append(
      h(
        "div",
        { class: "int-row" },
        dot("#C9956A", 4),
        h("span", { class: "int-time", text: `${day} ${time}` }),
        h("span", { class: "int-name", text: String(b.title ?? "Reunión") }),
      ),
    );
  }
  return h("div", { class: "int-card" }, header("#C9956A", "Cal.com", "Agenda"), rows);
}

// ── n8n ───────────────────────────────────────────────────────────────────────

function n8nCard(task: AgentTask, onDetail: () => void, openSettings: () => void): HTMLElement {
  const hasActivity = task.steps.length > 0 && (task.state === "finished" || task.state === "error");
  if (!hasActivity) return idleCard(task, openSettings);
  const success = task.state === "finished";
  const accent = success ? "#22C55E" : "#F4505E";
  return h(
    "div",
    { class: "int-card" },
    header("#F29B38", "n8n", "Flujo"),
    h(
      "div",
      { class: "int-actions" },
      h(
        "button",
        {
          class: "int-pill",
          style: `background:${accent}1a;border-color:${accent}38`,
          onclick: onDetail,
        },
        dot(accent, 5),
        h("span", { class: "int-name", text: task.steps[0] ?? "Workflow" }),
        svg(ICONS.ellipsis, 8),
      ),
    ),
  );
}

function n8nDetail(task: AgentTask, onBack: () => void): HTMLElement {
  const success = task.state === "finished";
  const accent = success ? "#22C55E" : "#F4505E";
  const detail = task.steps[1];
  return h(
    "div",
    { class: "int-card detail" },
    h(
      "div",
      { class: "int-detail-head" },
      h("button", { class: "int-back", onclick: onBack }, svg(ICONS.chevronLeft, 10, { stroke: 2.4 })),
      dot(accent, 6),
      h("b", { text: task.steps[0] ?? "Workflow" }),
      h("span", {
        class: "int-badge",
        style: `color:${accent};background:${accent}24`,
        text: success ? "Éxito" : "Falló",
      }),
    ),
    detail
      ? h("pre", { class: "int-detail-text", text: detail })
      : h("div", {
          class: "int-status",
          text: success ? "Terminó bien." : "Sin detalles del error.",
        }),
  );
}

// ── Grok Bot ──────────────────────────────────────────────────────────────────

function botCard(task: AgentTask, hooks: IntegrationCardHooks): HTMLElement {
  const last = task.lastMessage ?? task.steps.at(-1) ?? null;
  const color = task.state === "error" ? "#F4505E" : task.state === "finished" ? "#22C55E" : `${task.color}`;
  const head = header(task.color, task.name, "Bot de Grok");
  (head.querySelector("b") as HTMLElement).style.color = task.color;
  return h(
    "div",
    { class: "int-card" },
    head,
    h("div", { class: "int-status" }, dot(color, 5),
      h("span", { text: last ?? "En reposo. Toca la tarjeta para hablar con él." })),
    h("div", { class: "int-actions" },
      h("button", { class: "link-btn", style: "color:#8e939c", text: "Ajustes…", onclick: hooks.openSettings }),
    ),
  );
}

// ── Quick reply under a Bot's message ─────────────────────────────────────────

/** What was typed per Bot, so a redraw or another card keeps the text. */
const replyDrafts = new Map<string, string>();
/** Escape closed the box for this message (Bot id → its message). */
const replyDismissed = new Map<string, string>();
/** The Bot whose quick-reply box has the cursor. */
let replyFocusId: string | null = null;

/** True while the owner is typing a quick reply (to this Bot, when given). */
export function botReplyBusy(taskId?: string): boolean {
  return replyFocusId != null && (taskId == null || replyFocusId === taskId);
}

/**
 * A Bot that just answered (done, asking, failed) gets a box under its message,
 * until Escape closes it. While it is being typed in it stays, whatever the Bot
 * does meanwhile.
 */
export function botCanReply(task: AgentTask | null | undefined): task is AgentTask {
  if (!task || !task.id.startsWith(BOT_PREFIX) || !task.lastMessage) return false;
  if (replyFocusId === task.id) return true;
  if (task.state !== "finished" && task.state !== "question" && task.state !== "error") return false;
  return replyDismissed.get(task.id) !== task.lastMessage;
}

export interface BotReplyHandlers {
  /** botchat.sendToBot: same path and same history as the conversation. */
  send(slug: string, text: string): Promise<{ ok: boolean; message: string }>;
  /** The box got (true) or lost (false) the cursor. */
  focus(on: boolean): void;
}

export interface BotReply {
  el: HTMLElement;
  sync(task: AgentTask): void;
}

/** A compact box to answer a Bot right where its message is shown. */
export function createBotReply(handlers: BotReplyHandlers): BotReply {
  const input = h("textarea", {
    class: "bot-reply-input",
    rows: "1",
    placeholder: "Respóndele…",
    spellcheck: "true",
  }) as HTMLTextAreaElement;
  const sendBtn = h("button", { class: "bot-reply-send", title: "Enviar (Enter)" }, svg(ICONS.arrowUp, 10));
  const status = h("div", { class: "bot-reply-status" });
  const chips = h("div", { class: "bot-chips" });
  const note = h("div", { class: "bot-chips-note" });
  const box = h("div", { class: "bot-reply-box" }, input, sendBtn);
  const el = h("div", { class: "bot-reply" }, chips, note, box, status);

  let taskId = "";
  let slug = "";
  let message = "";
  let connected = false;
  let sending = false;
  let statusTimer: number | null = null;
  let blurTimer: number | null = null;

  // Nothing in here opens the conversation.
  el.addEventListener("click", (e) => e.stopPropagation());

  function setStatus(text: string, kind: "" | "sending" | "ok" | "err", clearAfter = 0) {
    if (statusTimer != null) window.clearTimeout(statusTimer);
    statusTimer = null;
    status.textContent = text;
    status.className = kind ? `bot-reply-status ${kind}` : "bot-reply-status";
    if (clearAfter > 0) {
      statusTimer = window.setTimeout(() => {
        statusTimer = null;
        status.textContent = "";
        status.className = "bot-reply-status";
      }, clearAfter);
    }
  }

  function autosize() {
    input.style.height = "auto";
    input.style.height = `${Math.min(input.scrollHeight, 44)}px`;
  }

  function release() {
    if (replyFocusId === taskId) replyFocusId = null;
    handlers.focus(false);
    State.notify();
  }

  async function submit() {
    const text = input.value.trim();
    if ((!text && Outbox.list(slug).length === 0) || !connected || sending) return;
    sending = true;
    // readOnly, not disabled: the box keeps the cursor (and the island stays).
    input.readOnly = true;
    sendBtn.disabled = true;
    setStatus("Enviando…", "sending");
    const sentTo = taskId;
    const r = await handlers.send(slug, text);
    sending = false;
    input.readOnly = false;
    sendBtn.disabled = !connected;
    if (sentTo !== taskId) return;
    if (r.ok) {
      input.value = "";
      replyDrafts.delete(taskId);
      autosize();
      setStatus("Enviado ✓", "ok", 2500);
    } else {
      setStatus(r.message, "err", 6000);
    }
  }

  // The island only receives keys once it has the keyboard: ask for it on the
  // press, then put the cursor in the box.
  input.addEventListener("pointerdown", () => {
    if (input.disabled) return;
    handlers.focus(true);
    window.setTimeout(() => input.focus(), 60);
  });
  input.addEventListener("focus", () => {
    if (blurTimer != null) window.clearTimeout(blurTimer);
    blurTimer = null;
    replyFocusId = taskId;
    handlers.focus(true);
  });
  input.addEventListener("blur", () => {
    // Taking the keyboard can blur for an instant: only a real departure counts.
    if (blurTimer != null) window.clearTimeout(blurTimer);
    blurTimer = window.setTimeout(() => {
      blurTimer = null;
      if (document.activeElement !== input || !document.hasFocus()) release();
    }, 150);
  });
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      void submit();
    } else if (e.key === "Escape") {
      // Closes the box only, not the island.
      e.preventDefault();
      e.stopPropagation();
      replyDismissed.set(taskId, message);
      if (replyFocusId === taskId) replyFocusId = null;
      input.blur();
      release();
    }
  });
  input.addEventListener("input", () => {
    replyDrafts.set(taskId, input.value);
    autosize();
  });
  // A pasted image or a long text goes as an attachment.
  input.addEventListener("paste", (e) => handleBotPaste(e, slug));
  sendBtn.addEventListener("click", () => void submit());

  return {
    el,
    sync(task) {
      if (task.id !== taskId) {
        taskId = task.id;
        slug = task.id.slice(BOT_PREFIX.length);
        input.value = replyDrafts.get(taskId) ?? "";
        setStatus("", "");
        window.requestAnimationFrame(autosize);
      }
      if (task.lastMessage !== message) {
        message = task.lastMessage ?? "";
        // A new message brings the box back after an Escape.
        el.classList.remove("bot-reply-enter");
        void el.offsetWidth;
        el.classList.add("bot-reply-enter");
      }
      el.style.setProperty("--bot", task.color);
      connected = !!State.settings.grokBots.find((b) => b.id === slug)?.url.trim();
      input.disabled = !connected;
      input.placeholder = connected ? `Respóndele a ${task.name}…` : "Este bot aún no está conectado";
      sendBtn.disabled = !connected || sending;
      box.classList.toggle("off", !connected);
      syncChips(chips, note, slug);
    },
  };
}

/** The Bot's tray as chips (name, size, × to take it out) and its notice line. */
function syncChips(row: HTMLElement, note: HTMLElement, slug: string) {
  const list = Outbox.list(slug);
  const k = `${slug}|${list.map((a) => a.key).join(",")}`;
  if (row.dataset.k !== k) {
    row.dataset.k = k;
    clear(row);
    for (const a of list) {
      row.append(h("span", { class: `bot-chip ${a.kind}`, title: a.name },
        h("span", { class: "bot-chip-name", text: a.name }),
        h("span", { class: "bot-chip-size", text: formatSize(a.size) }),
        h("button", {
          class: "bot-chip-x",
          title: "Quitar",
          onclick: (e: Event) => {
            e.stopPropagation();
            Outbox.remove(slug, [a.key]);
          },
        }, "×")));
    }
  }
  row.style.display = list.length ? "" : "none";
  const n = Outbox.notice(slug);
  note.textContent = n ?? "";
  note.style.display = n ? "" : "none";
}

/** "14:05" today, "03/10 14:05" before. */
function when(ms: number): string {
  const d = new Date(ms);
  const hm = `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
  const now = new Date();
  if (d.toDateString() === now.toDateString()) return hm;
  return `${String(d.getDate()).padStart(2, "0")}/${String(d.getMonth() + 1).padStart(2, "0")} ${hm}`;
}

/** The log's local "YYYY-MM-DD HH:MM:SS" as epoch ms (0 when missing). */
const logTime = (at?: string) => (at ? new Date(at.replace(" ", "T")).getTime() || 0 : 0);

/** Respect the owner's "reduce motion" setting for the timed parts (closing, scrolling). */
export const reducedMotion = () => window.matchMedia("(prefers-reduced-motion: reduce)").matches;

export interface BotChatHandlers {
  back(): void;
  /** Sends through botchat.sendToBot (grokbot_send). */
  send(slug: string, text: string): void;
  /** The approval card's own decide(): same audit, same flash. */
  decide(d: "allow" | "deny"): void;
}

export interface BotChatView {
  el: HTMLElement;
  /** Redraws what changed; new entries animate in and the timeline follows the end. */
  sync(task: AgentTask, approvals: BotApproval[] | null): void;
  /** Puts the cursor in the message box. */
  focus(): void;
  /** Drops the header mascot (its canvas) while the conversation is closed. */
  detach(): void;
}

type PermLike = {
  id: string;
  at: number;
  tool: string;
  decision: BotApproval["decision"];
  target: string;
  toolInput?: unknown;
  truncated?: boolean;
  omitted?: boolean;
};

/**
 * The conversation with one Grok Bot, on the island expanded like the chat:
 * one timeline — your messages, its messages, its steps, its permissions (the
 * pending one answerable right there) — and a box to write to it.
 */
export function createBotChat(handlers: BotChatHandlers): BotChatView {
  const mascot = h("span", { class: "bot-detail-mascot" });
  const dotEl = h("i", { class: "bot-detail-dot" });
  const nameEl = h("b", { class: "bot-detail-name" });
  const stateEl = h("span", { class: "bot-detail-state" });
  const backBtn = h("button", { class: "btn secondary bot-detail-back", title: "Volver" },
    svg(ICONS.chevronLeft, 9, { stroke: 2.4 }), h("span", { text: "Volver" }));
  const timeline = h("div", { class: "bot-detail-timeline" });
  const input = h("textarea", {
    class: "bot-detail-input",
    rows: "1",
    placeholder: "Escríbele…",
    spellcheck: "true",
  }) as HTMLTextAreaElement;
  const sendBtn = h("button", { class: "bot-detail-send", title: "Enviar (Enter)" }, svg(ICONS.arrowUp, 11));
  const offline = h("div", { class: "bot-detail-offline", text: "Este bot aún no está conectado. Pega su webhook en Ajustes → Mis Bots de Grok." });
  const composer = h("div", { class: "bot-detail-composer" }, input, sendBtn);
  const chips = h("div", { class: "bot-chips" });
  const note = h("div", { class: "bot-chips-note" });
  const cardEl = h("div", { class: "card bot-detail-card" },
    h("div", { class: "bot-detail-head" }, backBtn, mascot, dotEl, nameEl, stateEl),
    timeline,
    offline,
    chips,
    note,
    composer,
  );

  let slug = "";
  let taskId = "";
  let mascotFor = "";
  let headKey = "";
  let listKey = "";
  let connected = false;
  let seen = new Set<string>();
  const open = new Set<string>();
  let last: { task: AgentTask; approvals: BotApproval[] | null } | null = null;

  backBtn.addEventListener("click", () => handlers.back());

  function submit() {
    const text = input.value.trim();
    if ((!text && Outbox.list(slug).length === 0) || !connected) return;
    input.value = "";
    autosize();
    handlers.send(slug, text);
  }
  sendBtn.addEventListener("click", submit);
  input.addEventListener("keydown", (e) => {
    // Enter sends, Shift+Enter breaks the line; Escape stays with the island.
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      submit();
    }
  });
  function autosize() {
    input.style.height = "auto";
    input.style.height = `${Math.min(input.scrollHeight, 64)}px`;
  }
  input.addEventListener("input", autosize);
  // A pasted image or a long text goes as an attachment.
  input.addEventListener("paste", (e) => handleBotPaste(e, slug));

  const enter = (el: HTMLElement, order: number) => {
    el.classList.add("bot-detail-enter");
    el.style.animationDelay = `${Math.min(order, 10) * 35}ms`;
  };

  function permRow(p: PermLike): HTMLElement {
    const v = DECISION_LABELS[p.decision];
    const json = p.toolInput === undefined
      ? null
      : typeof p.toolInput === "string"
        ? `${p.toolInput}${p.truncated ? "…" : ""}`
        : JSON.stringify(p.toolInput, null, 2);
    const marks: string[] = [];
    if (json && p.truncated) marks.push("(recortado) El registro guarda hasta 2 KB de argumentos.");
    if (json && p.omitted) marks.push("(omitido) El contenido de archivos y los textos largos no se guardan, solo su longitud.");
    const isOpen = open.has(p.id);
    const row = h("div", { class: isOpen ? "bot-detail-perm open" : "bot-detail-perm", title: "Ver los argumentos" },
      h("div", { class: "bot-detail-perm-line" },
        h("span", { class: "bot-detail-chev" }, svg(ICONS.chevronLeft, 8, { stroke: 2.4 })),
        h("span", { class: "bot-detail-perm-kind", text: "Permiso" }),
        h("span", { class: "tool", text: p.tool }),
        h("span", { class: "bot-detail-verdict", style: `color:${v.color};background:${v.color}22`, text: v.text }),
        h("span", { class: "time", text: when(p.at) })),
      isOpen ? h("pre", { class: "bot-detail-json", text: json ?? (p.target || "Sin argumentos.") }) : null,
      isOpen && marks.length > 0 ? h("div", { class: "bot-detail-note", text: marks.join(" ") }) : null,
      isOpen && !json
        ? h("div", { class: "bot-detail-note", text: "Este permiso es de antes de que se guardaran los argumentos: solo queda el resumen." })
        : null,
    );
    row.addEventListener("click", (e) => {
      if ((e.target as HTMLElement).closest(".bot-detail-json")) return; // let text be selected
      if (open.has(p.id)) open.delete(p.id);
      else open.add(p.id);
      listKey = "";
      if (last) draw(last.task, last.approvals, false);
    });
    return row;
  }

  function entryEl(e: BotChatEntry): HTMLElement {
    switch (e.kind) {
      case "me": {
        const status = e.status === "sending" ? "enviando…" : e.status === "sent" ? "enviado" : "no se envió";
        return h("div", { class: `bot-detail-msg me ${e.status}` },
          e.text ? h("div", { class: "bot-detail-bubble", text: e.text }) : null,
          e.files?.length
            ? h("div", { class: "bot-detail-files" },
                ...e.files.map((f) => h("span", { class: "bot-chip sent", title: f.name },
                  h("span", { class: "bot-chip-name", text: f.name }),
                  h("span", { class: "bot-chip-size", text: formatSize(f.size) }))))
            : null,
          h("div", { class: "bot-detail-meta", text: `${when(e.at)} · ${status}` }),
          e.status === "error" && e.note ? h("div", { class: "bot-detail-note err", text: e.note }) : null);
      }
      case "bot": {
        const tag = { done: "listo", needs: "necesita algo", error: "error", working: "" }[e.status];
        return h("div", { class: `bot-detail-msg bot ${e.status}` },
          h("div", { class: "bot-detail-bubble", text: e.text }),
          h("div", { class: "bot-detail-meta", text: tag ? `${when(e.at)} · ${tag}` : when(e.at) }));
      }
      case "step":
        return h("div", { class: "bot-detail-step" },
          h("i"), h("span", { class: "bot-detail-text", text: e.text }), h("span", { class: "time", text: when(e.at) }));
      case "perm":
        return permRow(e);
    }
  }

  function pendingEl(): HTMLElement | null {
    const req = State.pendingApproval;
    if (!req || req.agentId !== taskId) return null;
    const question = req.tool === "Pregunta";
    return h("div", { class: "bot-detail-pending" },
      h("div", { class: "bot-detail-pending-head" },
        h("span", { class: "bot-detail-perm-kind", text: question ? "Pregunta" : "Pide permiso" }),
        question ? null : h("span", { class: "tool", text: req.tool })),
      h("div", { class: "bot-detail-pending-target", text: req.command || req.tool }),
      req.detail ? h("pre", { class: "bot-detail-json", text: req.detail }) : null,
      h("div", { class: "bot-detail-pending-actions" },
        h("button", { class: "btn secondary", onclick: () => handlers.decide("deny") }, h("span", { text: "Denegar" })),
        h("button", { class: "btn primary", onclick: () => handlers.decide("allow") }, h("span", { text: "Permitir" }))));
  }

  function draw(task: AgentTask, approvals: BotApproval[] | null, follow: boolean) {
    const entries = BotChat.list(slug);
    // Permissions the log knows but the conversation doesn't (older ones, or
    // the relay's free reads) join the timeline by time.
    const mine = entries.filter((e): e is Extract<BotChatEntry, { kind: "perm" }> => e.kind === "perm");
    const extra: PermLike[] = (approvals ?? [])
      .filter((a) => a.bot === slug)
      .map((a) => ({
        id: `log:${a.at}|${a.tool}|${a.decision}|${a.target}`,
        at: logTime(a.at), tool: a.tool, decision: a.decision, target: a.target,
        toolInput: a.toolInput, truncated: a.truncated, omitted: a.omitted,
      }))
      .filter((a) => !mine.some((m) => m.tool === a.tool && m.decision === a.decision && m.target === a.target && Math.abs(m.at - a.at) < 5000));
    const items: { id: string; at: number; el: () => HTMLElement }[] = [
      ...entries.map((e) => ({ id: e.id, at: e.at, el: () => entryEl(e) })),
      ...extra.map((p) => ({ id: p.id, at: p.at, el: () => permRow(p) })),
    ].sort((a, b) => a.at - b.at);

    const pending = State.pendingApproval?.agentId === taskId ? State.pendingApproval : null;
    const typing = !pending && botPhase(task.state)?.label === "trabajando";
    const key = [
      items.map((i) => i.id).join(","),
      entries.map((e) => (e.kind === "me" ? e.status : "")).join(""),
      pending?.requestId ?? "", typing, [...open].join(","),
    ].join("~");
    if (key === listKey) return;
    listKey = key;

    const nearEnd = timeline.scrollHeight - timeline.scrollTop - timeline.clientHeight < 48;
    clear(timeline);
    if (items.length === 0 && !pending) {
      timeline.append(h("div", { class: "bot-detail-empty", text: `Todavía no hablaron. Escríbele a ${task.name} y su respuesta aparecerá aquí.` }));
    }
    const next = new Set<string>();
    let fresh = 0;
    for (const item of items) {
      next.add(item.id);
      const el = item.el();
      if (!seen.has(item.id)) enter(el, fresh++);
      timeline.append(el);
    }
    const pendingNode = pendingEl();
    if (pendingNode) {
      const id = `pending:${pending?.requestId}`;
      next.add(id);
      if (!seen.has(id)) enter(pendingNode, fresh++);
      timeline.append(pendingNode);
    }
    if (typing) {
      timeline.append(h("div", { class: "bot-detail-typing", title: `${task.name} está trabajando` }, h("i"), h("i"), h("i")));
    }
    seen = next;
    if (follow && (nearEnd || fresh > 0)) {
      timeline.scrollTo({ top: timeline.scrollHeight, behavior: reducedMotion() || seen.size === fresh ? "auto" : "smooth" });
    }
  }

  return {
    el: cardEl,
    sync(task, approvals) {
      last = { task, approvals };
      if (task.id !== taskId) {
        taskId = task.id;
        slug = task.id.slice(BOT_PREFIX.length);
        headKey = listKey = "";
        seen = new Set();
        open.clear();
        input.value = "";
        autosize();
      }
      if (mascotFor !== `${task.id}|${task.color}` || !mascot.firstChild) {
        mascotFor = `${task.id}|${task.color}`;
        clear(mascot);
        mascot.append(createMiniBot(task, 18));
      }
      const phase = botPhase(task.state) ?? { label: "en reposo", color: "#8e939c" };
      const hk = `${task.name}|${task.color}|${phase.label}`;
      if (hk !== headKey) {
        headKey = hk;
        cardEl.style.setProperty("--bot", task.color);
        cardEl.style.setProperty("--phase", phase.color);
        nameEl.textContent = task.name;
        dotEl.classList.toggle("live", phase.label === "trabajando");
        stateEl.textContent = phase.label;
        // Re-run the little pop on every state change.
        stateEl.classList.remove("bot-detail-pop");
        void stateEl.offsetWidth;
        stateEl.classList.add("bot-detail-pop");
      }
      // Files dropped anywhere on the open conversation go to this Bot.
      cardEl.dataset.botDrop = task.id;
      syncChips(chips, note, slug);
      connected = !!State.settings.grokBots.find((b) => b.id === slug)?.url.trim();
      offline.style.display = connected ? "none" : "";
      composer.classList.toggle("off", !connected);
      input.disabled = !connected;
      sendBtn.toggleAttribute("disabled", !connected);
      draw(task, approvals, true);
    },
    focus() {
      if (connected) input.focus();
    },
    detach() {
      clear(mascot);
      mascotFor = "";
      pruneMiniBots();
    },
  };
}

// ── Dispatch ──────────────────────────────────────────────────────────────────

export interface IntegrationCardHooks {
  detailOpen: boolean;
  openDetail(): void;
  closeDetail(): void;
  openSettings(): void;
}

/** True when this integration has data worth showing instead of the idle card. */
export function hasIntegrationData(id: string): boolean {
  const info = State.integrations[id];
  if (!info || info.error) return false;
  switch (id) {
    case "integration_vercel":
      return arr(id, "deployments").length > 0;
    case "integration_resend":
      return arr(id, "emails").length > 0;
    case "integration_github":
      return get(id).totalRepos != null;
    case "integration_stripe":
      return info.loaded;
    case "integration_notion":
      return arr(id, "pages").length > 0;
    case "integration_calcom":
      return info.loaded;
    default:
      return false;
  }
}

export function renderIntegrationCard(task: AgentTask, hooks: IntegrationCardHooks): HTMLElement {
  if (task.id.startsWith(BOT_PREFIX)) return botCard(task, hooks);
  if (task.id === "integration_n8n") {
    const hasActivity = task.steps.length > 0 && (task.state === "finished" || task.state === "error");
    return hooks.detailOpen && hasActivity
      ? n8nDetail(task, hooks.closeDetail)
      : n8nCard(task, hooks.openDetail, hooks.openSettings);
  }
  if (task.id === "integration_vercel" && hasIntegrationData(task.id)) {
    return hooks.detailOpen ? vercelDetail(hooks.closeDetail) : vercelCard(hooks.openDetail);
  }
  if (!hasIntegrationData(task.id)) return idleCard(task, hooks.openSettings);

  switch (task.id) {
    case "integration_resend":
      return resendCard();
    case "integration_github":
      return githubCard();
    case "integration_stripe":
      return stripeCard();
    case "integration_notion":
      return notionCard();
    case "integration_calcom":
      return calcomCard();
    default:
      return idleCard(task, hooks.openSettings);
  }
}

export { clear };
