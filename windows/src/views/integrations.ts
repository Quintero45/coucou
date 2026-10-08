// Integration cards shown in the overview's left card — DOM ports of
// IntegrationCardView and friends from IslandViewContent.swift.
//
// Cal.com is the one simplification: macOS shows a three-level calendar
// (month → day → booking); here it is the list of upcoming bookings.

import { h, svg, clear, dot } from "./dom";
import { ICONS } from "./icons";
import { BOT_PREFIX, CURSOR_AGENT_ID, State, botPhase, type AgentTask } from "../core/state";
import { DECISION_LABELS, type BotApproval } from "../core/botlog";
import {
  BotChat, CURSOR_CHAT, chatSlug, cursorOrderCancel, cursorOrderNow, retryToBot, sendToCursor, type BotChatEntry,
} from "../core/botchat";
import { Outbox, attachContext, formatSize, handleBotPaste } from "../core/attachments";
import { BotLive } from "../core/botlive";
import { CMD, CURSOR_WRITE, callCmd, cmdErrorText } from "../core/botcmds";
import { renderMarkdown } from "./markdown";
import { buildChoices } from "./options";
import { fileName } from "../core/diff";

/** Small icons for the conversation's tools (24×24 paths, like ICONS). */
const TOOL_ICONS = {
  mic: "M12 14a3 3 0 0 0 3-3V5a3 3 0 0 0-6 0v6a3 3 0 0 0 3 3zm5-3a5 5 0 0 1-10 0H5a7 7 0 0 0 6 6.92V21h2v-3.08A7 7 0 0 0 19 11h-2z",
  meeting: "M9 11a3 3 0 1 0 0-6 3 3 0 0 0 0 6zm7 0a2.5 2.5 0 1 0 0-5 2.5 2.5 0 0 0 0 5zM9 13c-3 0-6 1.5-6 4v2h12v-2c0-2.5-3-4-6-4zm7 0c-.5 0-1 .05-1.5.13A4.6 4.6 0 0 1 17 17v2h4v-2c0-2.3-2.6-4-5-4z",
  screen: "M3 5h18v11H3V5zm2 2v7h14V7H5zm3 11h8v2H8v-2z",
  context: "M11 3h2v3h-2V3zm0 15h2v3h-2v-3zM3 11h3v2H3v-2zm15 0h3v2h-3v-2zM12 8a4 4 0 1 1 0 8 4 4 0 0 1 0-8zm0 2a2 2 0 1 0 0 4 2 2 0 0 0 0-4z",
  file: "M6 2h8l6 6v14H6V2zm7 1.5V9h5.5L13 3.5z",
  image: "M4 5h16v14H4V5zm2 2v8l3.5-3.5 2.5 2.5 3-3L18 14V7H6zm3 1.5a1.5 1.5 0 1 1 0 3 1.5 1.5 0 0 1 0-3z",
} as const;
import { createMiniBot, pruneMiniBots } from "../mochi/minibots";
import { Bridge } from "../core/bridge";
import { isComingSoon, pillDefinition } from "../core/pills";
import { refreshHookPills } from "../island/integrations";
import { readActivity, readPulse, readStats } from "../core/github";
import { githubDetail, githubPulseCard } from "./github";
import { N_, language, t } from "../i18n/i18n";

/** Same shape as the Swift `timeAgo` computed properties. */
export function timeAgo(value: unknown): string {
  const date = typeof value === "number" ? new Date(value) : new Date(String(value));
  const diff = (Date.now() - date.getTime()) / 1000;
  if (!Number.isFinite(diff)) return "";
  if (diff < 60) return t("just now");
  if (diff < 3600) return t("{n}m", { n: Math.floor(diff / 60) });
  if (diff < 86400) return t("{n}h", { n: Math.floor(diff / 3600) });
  return t("{n}d", { n: Math.floor(diff / 86400) });
}

/** An element holding `text` rendered as Markdown (bold, code, lists, links). */
function markdown(tag: "div" | "span", cls: string, text: string): HTMLElement {
  const el = h(tag, { class: cls });
  renderMarkdown(el, text);
  return el;
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

/** IntegrationCardView.statusLabel on macOS. */
export function idleStatus(
  id: string,
  info: { configured: boolean; error: string | null } | undefined,
  chatModel: string,
): { label: string; color: string } {
  if (isComingSoon(id)) return { label: t("Coming soon"), color: "#6B7079" };
  if (info?.error) return { label: info.error, color: "#F4505E" };
  const configured = info?.configured ?? false;
  const def = pillDefinition(id);
  const ok = (label: string) => ({ label, color: "#22C55E" });
  const missing = (label: string) => ({ label, color: "#F4505E" });
  // Pills driven by hooks never have a key: they are connected once the hooks
  // are in place (Mac #183). A session replaces this card; nothing is loading.
  if (def?.connect.kind === "hooks") return configured ? ok(t("Hooks installed")) : missing(t("Hooks not installed"));
  if (def?.connect.kind === "none") return ok(t("Ready · no setup needed"));
  if (def?.connect.kind === "server") return configured ? ok(t("Connected")) : missing(t("Not connected"));
  if (def?.category === "ai") {
    if (!configured) return missing(t("Key not configured"));
    return ok(id === "ai_anthropic" ? t("Key configured · {model}", { model: chatModel }) : t("Key configured"));
  }
  return configured ? ok(t("Connected · loading…")) : missing(t("Key not configured"));
}

function idleCard(task: AgentTask, openSettings: () => void): HTMLElement {
  const info = State.integrations[task.id];
  const configured = info?.configured ?? false;
  const def = pillDefinition(task.id);
  const status = idleStatus(task.id, info, State.settings.model);

  const actions = h("div", { class: "int-actions" });
  if (task.id === "integration_claude") {
    actions.append(
      h("button", {
        class: "link-btn",
        style: `color:${task.color}b3`,
        text: t("Open Visual Studio Code"),
        onclick: () => void Bridge.openInVSCode(task.sessionCwd ?? null),
      }),
    );
  } else if (task.id === "agent_claude-desktop") {
    actions.append(
      h("button", {
        class: "link-btn",
        style: `color:${task.color}d9`,
        text: t("Open Claude"),
        onclick: () => void Bridge.openClaudeDesktop(),
      }),
    );
  } else if (task.id === "integration_n8n") {
    actions.append(
      h("button", {
        class: "link-btn",
        style: `color:${task.color}d9`,
        text: t("Open {name}", { name: "n8n" }),
        onclick: () => void Bridge.openN8n(),
      }),
    );
  } else if (OPEN_URLS[task.id]) {
    actions.append(
      h("button", {
        class: "link-btn",
        style: `color:${task.color}d9`,
        text: t("Open {name}", { name: task.name }),
        onclick: () => void Bridge.openUrl(OPEN_URLS[task.id]),
      }),
    );
  }
  const hookPill = def?.connect.kind === "hooks";
  if (isComingSoon(task.id) || def?.connect.kind === "none") {
    // Nothing to set up, and nothing to refresh.
  } else if (configured && (hookPill || def?.category !== "ai")) {
    actions.append(
      h("button", {
        class: "link-btn",
        style: `color:${task.color}d9`,
        text: t("Refresh"),
        // A hook pill has nothing to poll: look at its hooks again instead.
        onclick: () => void (hookPill ? refreshHookPills() : Bridge.refreshIntegration(task.id)),
      }),
    );
  } else if (!configured) {
    actions.append(
      h("button", { class: "link-btn", style: "color:#8e939c", text: t("Settings…"), onclick: openSettings }),
    );
  }

  return h(
    "div",
    { class: "int-card" },
    header(
      task.color,
      task.id === "integration_claude" ? "VS Code" : task.name,
      t(def?.subtitle ?? N_("Integration")),
    ),
    h("div", { class: "int-status" }, dot(status.color, 5), h("span", { text: status.label })),
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
        { class: "int-more", title: t("Details"), onclick: onDetail },
        svg(ICONS.ellipsis, 8),
      );
      rows.append(listRow(accent, true, name, ago, more));
    } else {
      rows.append(listRow(accent, false, name, ago));
    }
  });
  return h("div", { class: "int-card" }, header("#7C5CFF", "Vercel", t("Deployments")), rows);
}

function vercelDetail(onBack: () => void): HTMLElement {
  const d = arr("integration_vercel", "deployments")[0] ?? {};
  const success = d.state === "READY";
  const accent = success ? "#22C55E" : "#F4505E";
  const status = success ? t("Ready") : d.state === "CANCELED" ? t("Canceled") : t("Error");
  const body = h("div", { class: "int-detail-body" });
  if (d.commitMessage) body.append(h("div", { class: "int-commit", text: String(d.commitMessage) }));
  const meta = h("div", { class: "int-meta" });
  if (d.branch) meta.append(h("span", { text: String(d.branch) }));
  meta.append(h("span", { text: t("{time} ago", { time: timeAgo(d.createdAt) }) }));
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
      h("b", { text: String(d.projectName ?? t("Deployment")) }),
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
  return h("div", { class: "int-card" }, header("#22C55E", "Resend", t("Emails"), extra), rows);
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
    header("#F4505E", "GitHub", t("Overview")),
    h(
      "div",
      { class: "int-stats" },
      statRow(ICONS.star, "#F5A524", t("Total stars"), fmt(stars)),
      statRow(ICONS.stack, "#6B7079", t("Repositories"), String(repos)),
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
        h("span", { class: "int-name", text: String(p.description ?? t("Payment")) }),
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
    header("#0570DE", "Stripe", t("Payments")),
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
        h("span", { class: "int-name", text: String(p.title ?? t("Untitled")) }),
        h("span", { class: "int-ago", text: timeAgo(p.lastEditedAt) }),
      ),
    );
  }
  return h("div", { class: "int-card" }, header("#E8E8E8", "Notion", t("Recent")), rows);
}

// ── Cal.com ───────────────────────────────────────────────────────────────────

function calcomCard(): HTMLElement {
  const bookings = arr("integration_calcom", "bookings")
    .slice()
    .sort((a, b) => new Date(String(a.start)).getTime() - new Date(String(b.start)).getTime());
  const rows = h("div", { class: "int-rows tight" });
  if (bookings.length === 0) {
    rows.append(h("div", { class: "int-empty", text: t("No calls scheduled") }));
  }
  for (const b of bookings.slice(0, 3)) {
    const when = new Date(String(b.start));
    const day = when.toLocaleDateString(language(), { day: "2-digit", month: "2-digit" });
    const time = when.toLocaleTimeString(language(), { hour: "2-digit", minute: "2-digit" });
    rows.append(
      h(
        "div",
        { class: "int-row" },
        dot("#C9956A", 4),
        h("span", { class: "int-time", text: `${day} ${time}` }),
        h("span", { class: "int-name", text: String(b.title ?? t("Meeting")) }),
      ),
    );
  }
  return h("div", { class: "int-card" }, header("#C9956A", "Cal.com", t("Schedule")), rows);
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
    header("#F29B38", "n8n", t("Workflow")),
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
        h("span", { class: "int-name", text: task.steps[0] ?? t("Workflow") }),
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
      h("b", { text: task.steps[0] ?? t("Workflow") }),
      h("span", {
        class: "int-badge",
        style: `color:${accent};background:${accent}24`,
        text: success ? t("Success") : t("Failed"),
      }),
    ),
    detail
      ? h("pre", { class: "int-detail-text", text: detail })
      : h("div", {
          class: "int-status",
          text: success ? t("Completed successfully.") : t("No error details available."),
        }),
  );
}

// ── Grok Bot ──────────────────────────────────────────────────────────────────

function botCard(task: AgentTask, hooks: IntegrationCardHooks): HTMLElement {
  const last = task.lastMessage ?? task.steps.at(-1) ?? null;
  const color = task.state === "error" ? "#F4505E" : task.state === "finished" ? "#22C55E" : `${task.color}`;
  const head = header(task.color, task.name, t("Grok Bot"));
  (head.querySelector("b") as HTMLElement).style.color = task.color;
  return h(
    "div",
    // Its mood animates the colour dot (style.css).
    { class: "int-card bot-card", "data-bot-state": BotLive.avatarState(task) },
    head,
    h("div", { class: "int-status" }, dot(color, 5),
      task.lastMessage
        ? markdown("span", "bot-card-md", task.lastMessage)
        : h("span", { text: last ?? t("Idle. Tap the card to talk to it.") })),
    h("div", { class: "int-actions" },
      h("button", { class: "link-btn", style: "color:#8e939c", text: t("Settings…"), onclick: hooks.openSettings }),
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
    placeholder: t("Reply…"),
    spellcheck: "true",
  }) as HTMLTextAreaElement;
  const sendBtn = h("button", { class: "bot-reply-send", title: t("Send (Enter)") }, svg(ICONS.arrowUp, 10));
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
      input.placeholder = connected ? t("Reply to {name}…", { name: task.name }) : t("This bot isn't connected yet");
      sendBtn.disabled = !connected || sending;
      box.classList.toggle("off", !connected);
      syncChips(chips, note, slug, (text) => {
        input.value = input.value ? `${input.value}\n${text}` : text;
        replyDrafts.set(taskId, input.value);
        autosize();
      });
    },
  };
}

/** The Bot's tray as chips (name, size, × to take it out) and its notice line. */
function syncChips(row: HTMLElement, note: HTMLElement, slug: string, pull?: (text: string) => void) {
  const list = Outbox.list(slug);
  const k = `${slug}|${list.map((a) => a.key).join(",")}`;
  if (row.dataset.k !== k) {
    row.dataset.k = k;
    clear(row);
    for (const a of list) {
      // A context note can be pulled into the box to be edited before sending.
      const preview = a.kind === "note" ? `${a.name}: ${a.text.replace(/\s+/g, " ").slice(0, 60)}` : a.name;
      const chip = h("span", {
        class: `bot-chip ${a.kind}`,
        title: a.kind === "note" ? `${a.name}\n${a.text.slice(0, 400)}\n\n${t("(Click: move it to the box to edit it)")}` : a.name,
      },
        h("span", { class: "bot-chip-name", text: preview }),
        h("span", { class: "bot-chip-size", text: formatSize(a.size) }),
        h("button", {
          class: "bot-chip-x",
          title: t("Remove"),
          onclick: (e: Event) => {
            e.stopPropagation();
            Outbox.remove(slug, [a.key]);
          },
        }, "×"));
      if (a.kind === "note" && pull) {
        chip.addEventListener("click", (e) => {
          e.stopPropagation();
          Outbox.remove(slug, [a.key]);
          pull(`${a.name}:\n${a.text}`);
        });
      }
      row.append(chip);
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
  /** The approval card's own decide(): same audit, same flash; `answer` for a choice. */
  decide(d: "allow" | "deny", answer?: string): void;
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
  const backBtn = h("button", { class: "btn secondary bot-detail-back", title: t("Back") },
    svg(ICONS.chevronLeft, 9, { stroke: 2.4 }), h("span", { text: t("Back") }));
  const timeline = h("div", { class: "bot-detail-timeline" });
  const input = h("textarea", {
    class: "bot-detail-input",
    rows: "1",
    placeholder: t("Write to it…"),
    spellcheck: "true",
  }) as HTMLTextAreaElement;
  const sendBtn = h("button", { class: "bot-detail-send", title: t("Send (Enter)") }, svg(ICONS.arrowUp, 11));
  const offline = h("div", { class: "bot-detail-offline", text: t("This bot isn't connected yet. Paste its webhook in Settings → My Grok Bots.") });
  // Second batch: context, dictation, meeting, screen sharing (Rust may not
  // have them yet — each says "no disponible todavía" then).
  const ctxBtn = h("button", { class: "bot-tool", title: t("Context: captures the active window, the selection and the clipboard so you can check them before sending") },
    svg(TOOL_ICONS.context, 12));
  const micBtn = h("button", { class: "bot-tool", title: t("Hold to dictate") }, svg(TOOL_ICONS.mic, 12));
  const meetingBtn = h("button", { class: "bot-tool-pill", title: t("Meeting: records and transcribes (microphone and PC sound)") },
    svg(TOOL_ICONS.meeting, 11), h("span", { text: t("Meeting") }));
  const shareSwitch = h("i", { class: "bot-switch" });
  const shareBtn = h("button", { class: "bot-tool-pill", title: t("Share your screen with the bot (one capture every 5 s)") },
    svg(TOOL_ICONS.screen, 11), h("span", { text: t("Screen") }), shareSwitch);
  const composer = h("div", { class: "bot-detail-composer" }, ctxBtn, micBtn, input, sendBtn);
  const chips = h("div", { class: "bot-chips" });
  const note = h("div", { class: "bot-chips-note" });
  const cardEl = h("div", { class: "card bot-detail-card" },
    h("div", { class: "bot-detail-head" }, backBtn, mascot, dotEl, nameEl, stateEl,
      h("span", { class: "bot-detail-tools" }, meetingBtn, shareBtn)),
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

  /** Cursor gets an order (queued while it works); a Bot, a message. */
  function deliver(who: string, text: string) {
    if (who !== CURSOR_CHAT) return handlers.send(who, text);
    const state = last?.task.state ?? "idle";
    const busy = ["working", "thinking", "searching", "approval", "question"].includes(state);
    void sendToCursor(text, BotLive.cursorScreen, busy);
  }

  function submit() {
    const text = input.value.trim();
    if ((!text && Outbox.list(slug).length === 0) || !connected) return;
    input.value = "";
    autosize();
    deliver(slug, text);
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

  const errMsg = (err: unknown) => (err instanceof Error ? err.message : String(err));
  const toolError = (what: string, err: unknown) => Outbox.setNotice(slug, `${what}: ${errMsg(err)}`);

  // Contexto: a preview in the tray; nothing leaves until Enviar.
  ctxBtn.addEventListener("click", () => {
    void attachContext(slug).then(() => input.focus());
  });

  // Hold to dictate: partial text shows in the box, the final text is sent.
  let dictBase = "";
  let dictPointer: number | null = null;
  micBtn.addEventListener("pointerdown", (e) => {
    if (micBtn.hasAttribute("disabled") || BotLive.dictatingSlug) return;
    e.preventDefault();
    dictPointer = e.pointerId;
    micBtn.setPointerCapture(e.pointerId);
    dictBase = input.value.trim();
    const who = slug;
    BotLive.dictatingSlug = who;
    BotLive.dictation.delete(who);
    micBtn.classList.add("live");
    Outbox.setNotice(who, t("Listening… release to send"));
    callCmd(CMD.startDictation, { bot: who }).catch((err) => {
      BotLive.dictatingSlug = null;
      micBtn.classList.remove("live");
      toolError(t("Dictation"), err);
    });
  });
  const endDictation = (e: PointerEvent) => {
    if (dictPointer !== e.pointerId) return;
    dictPointer = null;
    const who = BotLive.dictatingSlug;
    micBtn.classList.remove("live");
    if (!who) return;
    Outbox.setNotice(who, t("Transcribing…"));
    callCmd<{ text?: string }>(CMD.stopDictation)
      .then((r) => {
        const heard = String(r?.text ?? BotLive.dictation.get(who) ?? "").trim();
        BotLive.dictatingSlug = null;
        BotLive.dictation.delete(who);
        Outbox.setNotice(who, null);
        const text = [dictBase, heard].filter(Boolean).join(" ");
        if (who === slug) {
          input.value = "";
          autosize();
        }
        if (heard && connected) deliver(who, text);
        else if (who === slug) input.value = text;
      })
      .catch((err) => {
        BotLive.dictatingSlug = null;
        BotLive.dictation.delete(who);
        toolError(t("Dictation"), err);
      });
  };
  micBtn.addEventListener("pointerup", endDictation);
  micBtn.addEventListener("pointercancel", endDictation);

  // Reunión: start/stop; the transcript file lands in the timeline.
  meetingBtn.addEventListener("click", () => {
    if (meetingBtn.hasAttribute("disabled")) return;
    const who = slug;
    if (BotLive.meetingWith(who)) {
      callCmd<{ path_transcript?: string }>(CMD.stopMeeting)
        .then((r) => {
          BotLive.meeting = { active: false, bot: who };
          const path = r?.path_transcript;
          if (path) {
            BotChat.add(who, {
              kind: "file", path, name: path.split(/[\\/]/).pop() || "transcript.txt",
              mime: "text/plain", size: 0, source: "meeting",
            });
          }
          State.notify();
        })
        .catch((err) => toolError(t("Meeting"), err));
    } else {
      callCmd(CMD.startMeeting, { bot: who, options: { mic: true, system: true } })
        .then(() => {
          // meeting-state will confirm; until then, show it as started.
          if (!BotLive.meeting?.active) BotLive.meeting = { active: true, bot: who, since: Date.now() };
          State.notify();
        })
        .catch((err) => toolError(t("Meeting"), err));
    }
  });

  // Compartir pantalla: a switch.
  shareBtn.addEventListener("click", () => {
    const who = slug;
    if (who === CURSOR_CHAT) {
      BotLive.cursorScreen = !BotLive.cursorScreen;
      State.notify();
      return;
    }
    if (BotLive.sharingWith(who)) {
      callCmd(CMD.stopScreenShare)
        .then(() => {
          BotLive.screenShare = { active: false, bot: who };
          State.notify();
        })
        .catch((err) => toolError(t("Screen"), err));
    } else {
      // Tauri takes command arguments in camelCase (interval_s → intervalS).
      callCmd(CMD.startScreenShare, { bot: who, intervalS: 5 })
        .then(() => {
          if (!BotLive.screenShare?.active) BotLive.screenShare = { active: true, bot: who };
          State.notify();
        })
        .catch((err) => toolError(t("Screen"), err));
    }
  });

  /** The newest entry: only its choices are still open. */
  let lastEntryId = "";
  /** The newest file edit: open unless folded by hand (`open` toggles it). */
  let lastEditId = "";
  let answeredIds = new Set<string>();
  /** The "Escribir en Cursor" request whose full text is unfolded. */
  let orderOpenFor = "";

  /** A file the Bot sent (or a meeting transcript): icon, name, size; a click opens it. */
  function fileCard(e: Extract<BotChatEntry, { kind: "file" }>): HTMLElement {
    // The asset protocol is off in tauri.conf, so no thumbnails: an icon says it's an image.
    const isImage = e.mime.startsWith("image/");
    const card = h("div", { class: "bot-detail-file", title: t("Open {name}", { name: e.name }) },
      h("span", { class: "bot-detail-file-icon" }, svg(isImage ? TOOL_ICONS.image : TOOL_ICONS.file, 14)),
      h("span", { class: "bot-detail-file-body" },
        h("b", { text: e.name }),
        h("span", { text: e.caption || [e.source === "meeting" ? t("Meeting transcript") : isImage ? t("Image") : t("File"), e.size ? formatSize(e.size) : ""].filter(Boolean).join(" · ") })),
      h("span", { class: "time", text: when(e.at) }));
    card.addEventListener("click", () => {
      Bridge.openAttachment(e.path).catch((err: unknown) => toolError(t("Open"), new Error(cmdErrorText(err))));
    });
    return card;
  }

  /** A file the agent edited: its name and +N −M; open, the changed lines as code. */
  function editCard(e: Extract<BotChatEntry, { kind: "edit" }>): HTMLElement {
    const isOpen = open.has(e.id) !== (e.id === lastEditId);
    const name = fileName(e.path);
    const extAt = name.lastIndexOf(".");
    const ext = extAt > 0 ? name.slice(extAt + 1, extAt + 5).toUpperCase() : "";
    const where = e.path.replace(/\\/g, "/").split("/").slice(-3).join("/");
    const lines = isOpen
      ? h("div", { class: "bot-detail-edit-lines" },
          ...e.lines.map((l) => l.k === "…"
            ? h("div", { class: "diff-line gap", text: "⋯" })
            : h("div", { class: `diff-line ${l.k === "+" ? "added" : l.k === "-" ? "removed" : "context"}` },
                h("span", { class: "sym", text: l.k === "-" ? "−" : l.k }),
                h("span", { class: "txt", text: l.t }))),
          e.more > 0 ? h("div", { class: "diff-note", text: t("More lines: {count}", { count: e.more }) }) : null,
          e.tooLarge ? h("div", { class: "diff-note", text: t("Diff too large") }) : null)
      : null;
    const card = h("div", { class: isOpen ? "bot-detail-edit open" : "bot-detail-edit" },
      h("div", { class: "bot-detail-edit-head", title: e.path },
        h("span", { class: "bot-detail-chev" }, svg(ICONS.chevronLeft, 8, { stroke: 2.4 })),
        ext ? h("span", { class: "bot-detail-edit-ext", text: ext }) : null,
        h("b", { text: name }),
        e.added > 0 ? h("span", { class: "plus", text: `+${e.added}` }) : null,
        e.removed > 0 ? h("span", { class: "minus", text: `−${e.removed}` }) : null,
        h("span", { class: "bot-detail-edit-path", text: where }),
        h("button", {
          class: "icon-btn", title: t("Open in VS Code"),
          onclick: (ev: Event) => {
            ev.stopPropagation();
            void Bridge.openFileInVSCode(e.path);
          },
        }, svg(ICONS.arrowUpRight, 8)),
        h("span", { class: "time", text: when(e.at) })),
      lines);
    card.addEventListener("click", (ev) => {
      if ((ev.target as HTMLElement).closest(".bot-detail-edit-lines")) return; // let code be selected
      if (open.has(e.id)) open.delete(e.id);
      else open.add(e.id);
      listKey = "";
      if (last) draw(last.task, last.approvals, false);
    });
    return card;
  }

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
    if (json && p.truncated) marks.push(t("(trimmed) The log keeps up to 2 KB of arguments."));
    if (json && p.omitted) marks.push(t("(omitted) File contents and long texts aren't kept, only their length."));
    const isOpen = open.has(p.id);
    const row = h("div", { class: isOpen ? "bot-detail-perm open" : "bot-detail-perm", title: t("See the arguments") },
      h("div", { class: "bot-detail-perm-line" },
        h("span", { class: "bot-detail-chev" }, svg(ICONS.chevronLeft, 8, { stroke: 2.4 })),
        h("span", { class: "bot-detail-perm-kind", text: t("Permission") }),
        h("span", { class: "tool", text: p.tool }),
        h("span", { class: "bot-detail-verdict", style: `color:${v.color};background:${v.color}22`, text: t(v.text) }),
        h("span", { class: "time", text: when(p.at) })),
      isOpen ? h("pre", { class: "bot-detail-json", text: json ?? (p.target || t("No arguments.")) }) : null,
      isOpen && marks.length > 0 ? h("div", { class: "bot-detail-note", text: marks.join(" ") }) : null,
      isOpen && !json
        ? h("div", { class: "bot-detail-note", text: t("This permission is from before arguments were kept: only the summary is left.") })
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
        // «Recibido» until its first step or reply shows up; then just the time.
        const answered = e.status === "sent" && answeredIds.has(e.id);
        const status = { sending: t("Sending…"), sent: answered ? "" : t("Received"), queued: t("Queued"), error: t("Couldn't send") }[e.status];
        const queued = e.status === "queued";
        const canRetry = e.status === "error" && slug !== CURSOR_CHAT;
        return h("div", { class: `bot-detail-msg me ${e.status}` },
          e.text ? h("div", { class: "bot-detail-bubble", text: e.text }) : null,
          e.files?.length
            ? h("div", { class: "bot-detail-files" },
                ...e.files.map((f) => h("span", { class: "bot-chip sent", title: f.name },
                  h("span", { class: "bot-chip-name", text: f.name }),
                  h("span", { class: "bot-chip-size", text: formatSize(f.size) }))))
            : null,
          e.screen
            ? h("div", { class: "bot-detail-files" },
                h("span", { class: "bot-chip sent", title: t("It carried a capture of your screen") },
                  svg(TOOL_ICONS.screen, 10), h("span", { class: "bot-chip-name", text: t("Screenshot") })))
            : null,
          h("div", { class: "bot-detail-meta" },
            h("span", { text: when(e.at) }),
            status ? h("span", { class: `bot-detail-status ${e.status}`, text: ` · ${status}` }) : null),
          // A note under it: why it failed, why it waits, or that it already left.
          e.status !== "sending" && e.note ? h("div", { class: `bot-detail-note${e.status === "error" ? " err" : ""}`, text: e.note }) : null,
          queued
            ? h("div", { class: "bot-detail-order-actions" },
                h("button", { class: "btn secondary", title: t("Take it out of the queue"), onclick: () => void cursorOrderCancel(e.id) },
                  h("span", { text: t("Remove") })),
                h("button", { class: "btn primary", title: t("Write it in Cursor's chat now"), onclick: () => void cursorOrderNow(e.id) },
                  h("span", { text: t("Send now") })))
            : null,
          canRetry
            ? h("div", { class: "bot-detail-order-actions" },
                h("button", { class: "btn secondary bot-retry", title: t("Send it again"), onclick: () => void retryToBot(slug, e.id) },
                  h("span", { text: t("Retry") })))
            : null);
      }
      case "bot": {
        const tag = { done: t("Finished"), needs: t("Needs something"), error: t("Error"), working: "" }[e.status];
        return h("div", { class: `bot-detail-msg bot ${e.status}` },
          // Its formatting kept: bold, italics, code, lists, http(s) links.
          markdown("div", "bot-detail-bubble", e.text),
          // Its last question with choices (no relay request behind it): a pick
          // is sent as a normal message (grokbot_send).
          e.status === "needs" && e.options?.length && e.id === lastEntryId
            ? buildChoices({ options: e.options, allowCustom: !!e.allowCustom, onPick: (a) => deliver(slug, a) })
            : null,
          h("div", { class: "bot-detail-meta" },
            h("span", { text: tag ? `${when(e.at)} · ${tag}` : when(e.at) })));
      }
      case "step":
        return h("div", { class: "bot-detail-step" },
          h("i"), h("span", { class: "bot-detail-text", text: e.text }), h("span", { class: "time", text: when(e.at) }));
      case "file":
        return fileCard(e);
      case "edit":
        return editCard(e);
      case "transcript":
        return h("div", { class: "bot-detail-step transcript" },
          h("span", { class: "bot-detail-perm-kind", text: t("Transcript") }),
          h("span", { class: "bot-detail-text", text: e.text }), h("span", { class: "time", text: when(e.at) }));
      case "perm":
        return permRow(e);
    }
  }

  function pendingEl(): HTMLElement | null {
    const req = State.pendingApproval;
    if (!req || req.pillId !== taskId) return null;
    const question = req.tool === "Pregunta";
    // "Escribir en Cursor": the whole order, folded; open it to read it all.
    const order = req.tool === CURSOR_WRITE.tool && !!req.detail;
    return h("div", { class: "bot-detail-pending" },
      h("div", { class: "bot-detail-pending-head" },
        h("span", { class: "bot-detail-perm-kind", text: question ? t("Question") : t("Asks for permission") }),
        question ? null : h("span", { class: "tool", text: req.tool })),
      h("div", { class: "bot-detail-pending-target", text: req.command || req.tool }),
      order
        ? (() => {
            // Stays open across redraws of the timeline (new steps, typing…).
            const box = h("details", { class: "bot-detail-order", open: orderOpenFor === req.requestId },
              h("summary", { text: t("See the full text ({count} characters)", { count: req.detail!.length }) }),
              h("pre", { class: "bot-detail-json wrap", text: req.detail! }));
            box.addEventListener("toggle", () => {
              orderOpenFor = box.open ? req.requestId : "";
            });
            return box;
          })()
        : req.detail ? h("pre", { class: "bot-detail-json", text: req.detail }) : null,
      // With choices, a pick answers (same road as Permitir); otherwise Permitir / Denegar.
      req.options?.length
        ? buildChoices({ options: req.options, allowCustom: !!req.allowCustom, onPick: (a) => handlers.decide("allow", a) })
        : null,
      h("div", { class: "bot-detail-pending-actions" },
        h("button", { class: "btn secondary", onclick: () => handlers.decide("deny") }, h("span", { text: t("Deny") })),
        req.options?.length
          ? null
          : h("button", { class: "btn primary", onclick: () => handlers.decide("allow") }, h("span", { text: t("Allow") }))));
  }

  function draw(task: AgentTask, approvals: BotApproval[] | null, follow: boolean) {
    const entries = BotChat.list(slug);
    lastEntryId = entries.at(-1)?.id ?? "";
    const newestEdit = [...entries].reverse().find((e) => e.kind === "edit")?.id ?? "";
    // A newer edit takes over: the one before folds, whatever was done to it by hand.
    if (newestEdit !== lastEditId) open.delete(lastEditId);
    lastEditId = newestEdit;
    // My messages the Bot already answered (a step, a reply, a file or an edit after them).
    answeredIds = new Set();
    let answer = false;
    for (let i = entries.length - 1; i >= 0; i--) {
      const e = entries[i];
      if (e.kind === "bot" || e.kind === "step" || e.kind === "file" || e.kind === "edit") answer = true;
      else if (e.kind === "me" && answer) answeredIds.add(e.id);
    }
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

    const pending = State.pendingApproval?.pillId === taskId ? State.pendingApproval : null;
    const typing = !pending && botPhase(task.state)?.phase === "working";
    // Its latest step (bot-step), live under the timeline while it works.
    const live = typing ? BotLive.step(taskId)?.text ?? "" : "";
    const key = [
      items.map((i) => i.id).join(","),
      entries.map((e) => (e.kind === "me" ? `${e.status}${e.note ?? ""}` : "")).join(""),
      pending?.requestId ?? "", typing, live, entries.at(-1)?.at ?? 0, [...open].join(","),
    ].join("~");
    if (key === listKey) return;
    listKey = key;

    const nearEnd = timeline.scrollHeight - timeline.scrollTop - timeline.clientHeight < 48;
    clear(timeline);
    if (items.length === 0 && !pending) {
      const empty = task.id === CURSOR_AGENT_ID
        ? t("No activity from Cursor yet. Give it an order below or write in its window: you'll see your messages, its steps and its answers.")
        : t("You haven't talked yet. Write to {name} and its answer will show up here.", { name: task.name });
      timeline.append(h("div", { class: "bot-detail-empty", text: empty }));
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
      timeline.append(live
        ? h("div", { class: "bot-detail-live", title: t("{name} is working", { name: task.name }) },
            h("span", { class: "bot-detail-typing" }, h("i"), h("i"), h("i")),
            h("span", { class: "bot-detail-live-text", text: live }))
        : h("div", { class: "bot-detail-typing", title: t("{name} is working", { name: task.name }) }, h("i"), h("i"), h("i")));
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
        slug = chatSlug(task.id);
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
      // Its mood on the avatar's frame (style.css): pensando, recibido, listo…
      const mood = BotLive.avatarState(task);
      if (mascot.dataset.botState !== mood) mascot.dataset.botState = mood;
      const phase = botPhase(task.state) ?? { phase: null, label: t("Idle"), color: "#8e939c" };
      const hk = `${task.name}|${task.color}|${phase.label}`;
      if (hk !== headKey) {
        headKey = hk;
        cardEl.style.setProperty("--bot", task.color);
        cardEl.style.setProperty("--phase", phase.color);
        nameEl.textContent = task.name;
        dotEl.classList.toggle("live", phase.phase === "working");
        stateEl.textContent = phase.label;
        // Re-run the little pop on every state change.
        stateEl.classList.remove("bot-detail-pop");
        void stateEl.offsetWidth;
        stateEl.classList.add("bot-detail-pop");
      }
      // Cursor: orders go to its chat (cursorlink.rs); context and meetings are the Bots'.
      const isCursor = task.id === CURSOR_AGENT_ID;
      for (const el of [meetingBtn, ctxBtn]) el.style.display = isCursor ? "none" : "";
      // Files dropped anywhere on the open conversation go with the next message.
      cardEl.dataset.botDrop = task.id;
      syncChips(chips, note, slug, (text) => {
        input.value = input.value ? `${input.value}\n${text}` : text;
        autosize();
        input.focus();
      });
      connected = isCursor || !!State.settings.grokBots.find((b) => b.id === slug)?.url.trim();
      offline.style.display = connected ? "none" : "";
      composer.classList.toggle("off", !connected);
      input.disabled = !connected;
      input.placeholder = isCursor
        ? (botPhase(task.state)?.phase === "working" ? t("Give it an order: it gets it when it's done…") : t("Give Cursor an order…"))
        : t("Write to it…");
      sendBtn.toggleAttribute("disabled", !connected);
      shareBtn.title = isCursor
        ? (BotLive.cursorScreen ? t("Every order carries a capture of your screen. Tap to stop.") : t("Screen: every order you send carries a capture of your screen"))
        : t("Share your screen with the bot (one capture every 5 s)");
      // Voice engine not ready (or downloading): no microphone, no meeting.
      const blocked = BotLive.voiceBlocked();
      micBtn.toggleAttribute("disabled", !!blocked || !connected);
      micBtn.title = blocked ?? (connected ? t("Hold to dictate") : t("This bot isn't connected yet"));
      meetingBtn.toggleAttribute("disabled", !!blocked && !BotLive.meetingWith(slug));
      meetingBtn.title = blocked ?? t("Meeting: records and transcribes (microphone and PC sound)");
      meetingBtn.classList.toggle("on", BotLive.meetingWith(slug));
      shareBtn.classList.toggle("on", BotLive.sharingWith(slug));
      shareSwitch.classList.toggle("on", BotLive.sharingWith(slug));
      // Dictating: what has been heard so far, live in the box.
      if (BotLive.dictatingSlug === slug) {
        const heard = BotLive.dictation.get(slug) ?? "";
        input.value = [dictBase, heard].filter(Boolean).join(" ");
        autosize();
      }
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
      return get(id).totalRepos != null || readPulse(get(id)) != null;
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

  // With the pulse in, GitHub gets the Mac's richer card and its lists.
  if (task.id === "integration_github") {
    const d = get(task.id);
    const pulse = readPulse(d);
    if (pulse) {
      return hooks.detailOpen
        ? githubDetail(pulse, readStats(d), readActivity(d), hooks.closeDetail)
        : githubPulseCard(pulse, readStats(d), readActivity(d), hooks.openDetail);
    }
  }

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
