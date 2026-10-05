// Island views — DOM ports of IslandViewContent.swift. Paddings, font sizes,
// colours and wording are copied from the Swift views so both platforms read
// identically.

import { h, svg, clear, dot } from "./dom";
import { ICONS } from "./icons";
import { Ticker } from "./ticker";
import { ASSISTANT_ID, BOT_PREFIX, CURSOR_AGENT_ID, MOCHI_TASK, State, aiProvider, botPhase, type AgentTask } from "../core/state";
import { setQuestionHeight, washRGBA, type IslandViewName, type Wash } from "../core/layout";
import { createMiniBot, pruneMiniBots } from "../mochi/minibots";
import { buildPrompt } from "./chat";
import {
  botCanReply, createBotChat, createBotReply, reducedMotion, renderIntegrationCard,
  type BotReplyHandlers, type IntegrationCardHooks,
} from "./integrations";
import { sendWithOutbox } from "../core/botchat";
import { loadBotApprovals, type BotApproval } from "../core/botlog";
import { BotLive } from "../core/botlive";
import { renderMarkdown } from "./markdown";

export interface ViewActions {
  setView(v: IslandViewName): void;
  collapse(): void;
  setFocus(id: string): void;
  openTerminal(): void;
  /** The ↗ button: opens whatever the focused pill points at (not on Grok Bots). */
  openTarget(): void;
  openUrl(url: string): void;
  decide(d: "allow" | "deny" | "always"): void;
  /** AskUserQuestion: `{ question: label }`, or null to answer in the terminal. */
  answerQuestions(answers: Record<string, string> | null): void;
  showDiff(taskId: string): void;
  toggleSound(): void;
  setVolume(v: number): void;
  setAutoClose(seconds: number): void;
  openSettingsWindow(): void;
  blip(): void;
  /** A tap on a Grok Bot (its pill or its card): grow the island to the chat's size and open the conversation. */
  openBotDetail(id: string): void;
  /** Back to the overview's normal size. */
  closeBotDetail(): void;
  /** A quick-reply box got (true) or lost (false) the cursor: keyboard + stay open. */
  botReplyFocus(on: boolean): void;
}

/**
 * The Grok Bot whose detail is open (overview only). The island reads it to
 * size itself exactly like the chat; the overview reads it to draw the
 * conversation with that Bot.
 */
export const botDetail: { id: string | null } = { id: null };

export interface ViewHost {
  el: HTMLElement;
  sync(): void;
  /** Called when the view becomes active, for views with a text field. */
  focus?(): void;
  /** Called every frame while the view is on screen. */
  tick?(nowMs: number): void;
}

// ── Shared pieces ─────────────────────────────────────────────────────────────

function card(wash: Wash, ...children: (Node | string)[]): HTMLElement {
  const el = h("div", { class: wash ? "card wash" : "card" }, ...children);
  if (wash) el.style.setProperty("--wash", washRGBA(wash));
  return el;
}

function btn(
  label: string,
  kind: "primary" | "secondary",
  onClick: () => void,
  kbd?: string,
): HTMLElement {
  return h(
    "button",
    { class: `btn ${kind}`, onclick: onClick },
    h("span", { text: label }),
    kbd ? h("span", { class: "kbd", text: kbd }) : null,
  );
}

/** AgentWho — coloured dot + task name + grey label. */
function agentWho(task: AgentTask | null, label: string): HTMLElement {
  const row = h("div", { class: "who-row" });
  if (task) {
    row.append(dot(task.color, 8), h("span", { class: "n", text: task.name }));
  }
  row.append(h("span", { text: label }));
  return row;
}

function stack(padLeft: number, padRight: number, ...children: Node[]): HTMLElement {
  const el = h("div", { class: "stack" }, ...children);
  el.style.padding = `4px ${padRight}px 4px ${padLeft}px`;
  return el;
}

// ── Header ────────────────────────────────────────────────────────────────────

export function buildHeader(actions: ViewActions): ViewHost {
  const tabHome = h("button", { class: "tab", title: "Resumen", onclick: () => go("overview") }, svg(ICONS.house, 13));
  const tabChat = h("button", { class: "tab", title: "Preguntar", onclick: () => go("prompt") }, svg(ICONS.bubble, 13));

  const gearBtn = h("button", { title: "Ajustes", onclick: () => go("settings") }, svg(ICONS.gear, 14));
  const soundBtn = h("button", { title: "Silenciar", onclick: () => actions.toggleSound() }, svg(ICONS.speakerOn, 14));

  function go(v: IslandViewName) {
    actions.blip();
    actions.setView(v);
  }

  const el = h(
    "div",
    { id: "header" },
    h("div", { class: "tabs" }, tabHome, tabChat),
    h("div", { class: "header-actions" }, gearBtn, soundBtn),
  );

  return {
    el,
    sync() {
      const v = State.view;
      tabHome.classList.toggle("on", v === "overview" || v === "empty");
      tabChat.classList.toggle("on", v === "prompt");
      gearBtn.classList.toggle("on", v === "settings");
      clear(gearBtn);
      gearBtn.append(svg(v === "settings" ? ICONS.gearFill : ICONS.gear, 14));
      clear(soundBtn);
      soundBtn.append(svg(State.settings.soundEnabled ? ICONS.speakerOn : ICONS.speakerOff, 14));
      el.style.opacity = v === "confused" ? "0" : "1";
    },
  };
}

// ── Overview ──────────────────────────────────────────────────────────────────

function buildOverview(actions: ViewActions): ViewHost {
  const ticker = new Ticker();
  const who = h("div", { class: "who" });
  const tickerBody = h("div", { class: "card-body" }, who, ticker.el);
  const leftBody = h("div", { class: "left-body" });
  const jump = h(
    "button",
    {
      class: "icon-btn jump",
      title: "Abrir",
      // Grok Bots have no ↗: their whole card opens the conversation.
      onclick: () => actions.openTarget(),
    },
    svg(ICONS.arrowUpRight, 8),
  );
  // Answering a Bot's message right under it, without opening the conversation.
  const reply = createBotReply(replyHandlers(actions));
  const replySlot = h("div", { class: "bot-reply-slot" }, reply.el);
  replySlot.style.display = "none";
  const left = card(null, leftBody, jump, replySlot);
  // The whole card of a focused Grok Bot opens its conversation; the buttons
  // and boxes inside it keep their own clicks.
  left.addEventListener("click", (e) => {
    const id = State.focusTask?.id;
    if (!id?.startsWith(BOT_PREFIX) || botDetail.id) return;
    if ((e.target as Element).closest("button, a, input, textarea, select, .bot-reply")) return;
    actions.openBotDetail(id);
  });
  const pills = h("div", { class: "pills" });
  const right = card(null, pills);

  // The expanded Grok Bot detail sits over the overview while it is open.
  const detail = createBotChat({
    back: () => closeDetailAnimated(),
    send: (slug, text) => void sendWithOutbox(slug, text),
    // Exactly the approval card's decide(): audit line, botFx flash, sound.
    decide: (d) => actions.decide(d),
  });
  const layer = h("div", { class: "bot-detail-layer" }, detail.el);
  layer.style.display = "none";

  const el = h("div", { class: "view overview" },
    h("div", { class: "left" }, left),
    h("div", { class: "right" }, right),
    layer,
  );

  let closing: number | null = null;
  /** Plays the way out, then lets the island shrink back. */
  function closeDetailAnimated() {
    if (closing != null) return;
    if (reducedMotion()) {
      actions.closeBotDetail();
      return;
    }
    layer.classList.add("out");
    closing = window.setTimeout(() => {
      closing = null;
      actions.closeBotDetail();
    }, 170);
  }

  let pillIds = "";
  let detailOpen = false;
  let lastFocus: string | null = null;
  let mode: "ticker" | "card" | null = null;
  let cardKey = "";
  let botApprovals: BotApproval[] | null = null;
  /** Bot state + steps the approvals were last read for. */
  let approvalsFor = "";

  function refreshBotApprovals() {
    void loadBotApprovals().then((list) => {
      botApprovals = list;
      State.notify();
    });
  }

  const hooks: IntegrationCardHooks = {
    get detailOpen() {
      return detailOpen;
    },
    openDetail() {
      detailOpen = true;
      cardKey = "";
      State.notify();
    },
    closeDetail() {
      detailOpen = false;
      cardKey = "";
      State.notify();
    },
    openSettings: () => actions.openSettingsWindow(),
  };

  return {
    el,
    /** Files dropped on a Bot: the cursor goes to the open conversation's box. */
    focus() {
      if (layer.style.display !== "none") detail.focus();
    },
    tick(nowMs: number) {
      if (mode === "ticker") ticker.tick(nowMs);
    },
    sync() {
      const task = State.focusTask;
      if (task?.id !== lastFocus) {
        lastFocus = task?.id ?? null;
        detailOpen = false;
        cardKey = "";
        mode = null;
      }

      // A live agent session (Claude Code, Cursor, Codex…) keeps the ticker;
      // every other pill shows its own card, exactly like IntegrationCardView.
      const isAgent = task?.id === "integration_claude" || task?.source === "agent";
      const sessionActive = isAgent && !!task && (task.state !== "idle" || task.steps.length > 0);
      const isBot = !!task?.id.startsWith(BOT_PREFIX);

      // The expanded Bot detail: only for the focused Bot, only while expanded.
      const openId = botDetail.id;
      if (openId && (openId !== task?.id || State.mode !== "expanded")) {
        // Focus moved or the island folded: close without waiting for a frame.
        queueMicrotask(() => actions.closeBotDetail());
      }
      const showDetail = !!openId && openId === task?.id && State.mode === "expanded";
      if (showDetail && task) {
        if (layer.style.display === "none") {
          // Fresh entrance: reload everything and replay the way in.
          botApprovals = null;
          approvalsFor = "";
          layer.classList.remove("out");
          layer.style.display = "";
          el.classList.add("bot-detail-open");
        }
        // Live: a new step or state re-reads the permissions (a click may have landed).
        const k = `${task.id}:${task.state}:${task.steps.length}`;
        if (k !== approvalsFor) {
          approvalsFor = k;
          refreshBotApprovals();
        }
        const entering = !el.classList.contains("bot-detail-live");
        el.classList.add("bot-detail-live");
        detail.sync(task, botApprovals);
        if (entering) window.setTimeout(() => detail.focus(), 140);
      } else if (layer.style.display !== "none") {
        if (closing != null) {
          window.clearTimeout(closing);
          closing = null;
        }
        layer.style.display = "none";
        layer.classList.remove("out");
        el.classList.remove("bot-detail-open", "bot-detail-live");
        detail.detach();
      }

      // The focused card wears a Grok Bot's colour, like its pill does.
      left.style.borderColor = isBot && task ? `${task.color}99` : "";
      left.style.background = isBot && task ? `linear-gradient(${task.color}1F, ${task.color}1F), var(--card)` : "";
      left.classList.toggle("bot-tap", isBot && !showDetail);
      if (isBot && task) left.style.setProperty("--bot", task.color);
      else left.style.removeProperty("--bot");
      // Files dragged over this card go to the Bot (island.ts reads data-bot-drop).
      if (isBot && task && !showDetail) left.dataset.botDrop = task.id;
      else delete left.dataset.botDrop;

      const replying = isBot && !showDetail && botCanReply(task);
      replySlot.style.display = replying ? "" : "none";
      if (replying && task) reply.sync(task);

      if (task && sessionActive) {
        if (mode !== "ticker") {
          clear(leftBody);
          leftBody.append(tickerBody);
          mode = "ticker";
          cardKey = "";
        }
        clear(who);
        who.append(
          dot(task.color, 7),
          h("span", { class: "name", text: task.name, style: isBot ? `color:${task.color}` : undefined }),
          h("span", { class: "tool", text: toolLabel(task) }),
        );
        if (task.steps.length > 1) {
          who.append(h("span", {
            class: "count",
            text: `${Math.min(task.stepIndex + 1, task.steps.length)}/${task.steps.length}`,
          }));
        }
        if (task.lastDiff) {
          const d = task.lastDiff;
          const id = task.id;
          who.append(h("button", {
            class: "link-btn diff-chip",
            title: "Ver el diff",
            onclick: () => actions.showDiff(id),
          }, h("span", { class: "add", text: `+${d.added}` }), h("span", { class: "del", text: ` −${d.removed}` })));
        }
        ticker.sync(task);
      } else if (task) {
        const info = State.integrations[task.id];
        const key = [
          task.id, detailOpen, task.state, task.steps.join("|"),
          isBot ? task.lastMessage ?? "" : "",
          info?.loaded, info?.error, info?.configured,
          JSON.stringify(info?.data ?? {}),
        ].join("~");
        if (key !== cardKey) {
          cardKey = key;
          mode = "card";
          clear(leftBody);
          leftBody.append(renderIntegrationCard(task, hooks));
        }
      }

      jump.style.display = detailOpen || isBot ? "none" : "";

      const others = State.otherTasks.slice(0, 4);
      const noBots = State.settings.grokBots.length === 0;
      // A Bot pill also redraws when its state word changes.
      const pillKey = others
        .map((t) => `${t.id}:${t.pillBadge ?? ""}:${t.id.startsWith(BOT_PREFIX) ? `${t.state}:${t.color}:${t.name}:${BotLive.step(t.id)?.text ?? ""}` : ""}`)
        .join("|") + (noBots ? "|+bot" : "");
      if (pillKey !== pillIds) {
        pillIds = pillKey;
        clear(pills);
        for (const t of others) pills.append(buildPill(t, actions));
        if (noBots && others.length < 4) pills.append(addBotPill(actions));
        pruneMiniBots();
      }
    },
  };
}

/** Which agent a session pill belongs to, for the grey label. */
function toolLabel(task: AgentTask): string {
  if (task.source === "claudeCode") return "Claude Code";
  if (task.id.startsWith(BOT_PREFIX)) {
    const phase = botPhase(task.state);
    // Working: its latest live step (bot-step) says more than "trabajando".
    const live = phase?.label === "trabajando" ? BotLive.step(task.id)?.text : null;
    if (live) return live;
    return phase ? `Bot de Grok · ${phase.label}` : "Bot de Grok";
  }
  if (task.source === "agent") return task.name;
  return "n8n";
}

/** "Claude Code finished", "Cursor needs permission"… */
function agentName(task: AgentTask | null): string {
  if (!task) return "Claude Code";
  return task.source === "agent" ? task.name : "Claude Code";
}

function buildPill(task: AgentTask, actions: ViewActions): HTMLElement {
  const label = task.id === "integration_claude" ? "VS Code" : task.name;
  const canvas = createMiniBot(task, 24);
  // Grok Bots say what they are doing under their name: trabajando, pregunta,
  // listo, error. Nothing when idle.
  const phase = task.id.startsWith(BOT_PREFIX) ? botPhase(task.state) : null;
  const lbl = phase
    ? h("span", { class: "lbl two" },
        h("span", { class: "lbl-name", text: label, style: `color:${task.color}` }),
        h("span", { class: "pill-state" }, h("i", { style: `background:${phase.color}` }),
          // Working: the latest live step instead of just "trabajando".
          h("span", { class: "pill-step", text: (phase.label === "trabajando" ? BotLive.step(task.id)?.text : null) ?? phase.label })))
    : h("span", { class: "lbl", text: label, style: task.id.startsWith(BOT_PREFIX) ? `color:${task.color}` : undefined });
  const pill = h(
    "div",
    {
      class: task.id.startsWith(BOT_PREFIX) ? "pill pill-bot" : "pill",
      title: phase ? `${label} · ${phase.label}` : label,
      // A Grok Bot's pill opens its conversation straight away.
      onclick: () => (task.id.startsWith(BOT_PREFIX) ? actions.openBotDetail(task.id) : actions.setFocus(task.id)),
    },
    canvas,
    lbl,
  );
  // A Grok Bot pill wears its own colour (Mis Bots de Grok); the state word stays neutral.
  if (task.id.startsWith(BOT_PREFIX)) {
    pill.style.borderColor = `${task.color}99`;
    pill.style.background = `${task.color}1F`;
    // A file dragged over the pill lights it up in the Bot's colour (island.ts).
    pill.dataset.botDrop = task.id;
    pill.style.setProperty("--bot", task.color);
  } else {
    pill.style.borderColor = `${task.color}24`;
  }
  pill.addEventListener("mouseenter", () => {
    pill.style.background = `${task.color}2e`;
    pill.style.borderColor = `${task.color}8c`;
    pill.style.boxShadow = `0 2px 10px ${task.color}59`;
    (pill.querySelector(".lbl") as HTMLElement).style.color = lighten(task.color, 0.3);
  });
  const isBotPill = task.id.startsWith(BOT_PREFIX);
  pill.addEventListener("mouseleave", () => {
    pill.style.background = isBotPill ? `${task.color}1F` : "";
    pill.style.borderColor = isBotPill ? `${task.color}99` : `${task.color}24`;
    pill.style.boxShadow = "";
    // An idle Bot pill's name is the .lbl itself: give its colour back.
    (pill.querySelector(".lbl") as HTMLElement).style.color = isBotPill && !phase ? task.color : "";
  });

  if (task.pillBadge) {
    const colors = { approval: "#F5A524", finished: "#22C55E", error: "#F4505E" } as const;
    const icons = { approval: ICONS.bang, finished: ICONS.check, error: ICONS.xmark } as const;
    const inner = h("i", { style: `background:${colors[task.pillBadge]}` }, svg(icons[task.pillBadge], 6, { stroke: task.pillBadge === "finished" ? 3 : 0 }));
    const badge = h("div", { class: "pill-badge" }, inner);
    badge.style.boxShadow = `0 0 4px ${colors[task.pillBadge]}99`;
    pill.append(badge);
  }
  return pill;
}

/** Stands in for the Grok Bots until the owner connects one. */
function addBotPill(actions: ViewActions): HTMLElement {
  return h(
    "div",
    { class: "pill pill-add", title: "Ajustes → Mis Bots de Grok", onclick: () => actions.openSettingsWindow() },
    h("span", { class: "pill-add-icon" }, svg(ICONS.plus, 12)),
    h("span", { class: "lbl", text: "Conectar un Bot de Grok" }),
  );
}

function lighten(hex: string, amount: number): string {
  const v = parseInt(hex.replace("#", ""), 16);
  const c = [(v >> 16) & 255, (v >> 8) & 255, v & 255].map((x) =>
    Math.min(255, Math.round(x + amount * 255)),
  );
  return `rgb(${c[0]},${c[1]},${c[2]})`;
}

// ── Empty ─────────────────────────────────────────────────────────────────────

function buildEmpty(actions: ViewActions): ViewHost {
  const body = h(
    "div",
    { class: "stack", style: "padding:0 18px 0 118px;flex-direction:row;align-items:center;gap:16px" },
    h(
      "div",
      { style: "display:flex;flex-direction:column;gap:5px" },
      h("div", { class: "title", text: "Nada en marcha ahora mismo." }),
      h("div", { class: "sub", text: "Suelta un archivo o pregúntame lo que quieras." }),
    ),
    h("div", { class: "grow" }),
    btn("Preguntar a Mochi", "primary", () => actions.setView("prompt")),
  );
  return { el: h("div", { class: "view" }, card(null, body)), sync() {} };
}

// ── Approval ──────────────────────────────────────────────────────────────────

function buildApproval(actions: ViewActions): ViewHost {
  const who = h("div");
  const code = h("div", { class: "code" });
  const detail = h("pre", { class: "approval-detail" });
  const row = h("div", { class: "actions" });
  const el = h("div", { class: "view" }, card("amber", stack(116, 16, who, code, detail, row)));
  let rowKey = "";
  let detailFor = "";
  return {
    el,
    sync() {
      const req = State.pendingApproval;
      clear(who);
      const isBot = !!req?.agentId.startsWith(BOT_PREFIX);
      if (req?.agentId === ASSISTANT_ID) {
        who.append(agentWho({ ...MOCHI_TASK }, `quiere usar ${req.tool.replace(/_/g, " ")}`));
      } else if (req && isBot) {
        // A Grok Bot: its name and colour, then what it wants — a yes/no
        // question (`--status ask`) or a tool on this PC with its arguments.
        const owner = State.tasks.find((t) => t.id === req.agentId) ?? null;
        who.append(agentWho(owner, req.tool === "Pregunta" ? "Bot de Grok · pregunta" : `Bot de Grok · quiere usar ${req.tool}`));
      } else {
        const owner = State.tasks.find((t) => t.id === req?.agentId) ?? State.focusTask;
        who.append(agentWho(owner, "pide permiso"));
      }
      // The whole point of approving here rather than in the terminal: this line
      // is the command, the file path or the URL being authorised, not just the
      // name of the tool asking.
      code.textContent = req?.command || req?.tool || "…";
      // A skill's code or a diff: the whole text, scrollable, reset per request.
      if (detailFor !== (req?.requestId ?? "")) {
        detailFor = req?.requestId ?? "";
        detail.textContent = req?.detail ?? "";
        detail.style.display = req?.detail ? "" : "none";
        detail.classList.toggle("wrap", !!req?.detailWrap);
        detail.scrollTop = 0;
      }
      // Built once per request shape. Rebuilding them between a mouse-down and a
      // mouse-up would swallow the click. "Always" only exists when Claude Code
      // suggested a rule to remember — Codex and Cursor have no such thing.
      const key = req?.allowAlways ? "always" : isBot ? "bot" : "plain";
      if (rowKey === key) return;
      rowKey = key;
      clear(row);
      row.append(btn(isBot ? "Denegar" : "Rechazar", "secondary", () => actions.decide("deny"), "N"));
      if (req?.allowAlways) row.append(btn("Siempre", "secondary", () => actions.decide("always"), "A"));
      row.append(btn("Permitir", "primary", () => actions.decide("allow"), "Y"));
    },
  };
}

// ── Question ──────────────────────────────────────────────────────────────────

/**
 * AskUserQuestion, answered from the island. One question at a time; single
 * choice answers on click, multi-select toggles then "Next". "Reply in
 * terminal" hands it back to Claude Code untouched.
 */
function buildQuestion(actions: ViewActions): ViewHost {
  const who = h("div");
  const title = h("div", { class: "title" });
  const options = h("div", { class: "q-options" });
  const row = h("div", { class: "actions" });
  const el = h("div", { class: "view" }, card("cyan", stack(116, 16, who, title, options, row)));

  let key = "";
  let index = 0;
  let answers: Record<string, string> = {};
  let picked = new Set<string>();

  function next(label: string | null) {
    const q = State.pendingQuestion?.questions[index];
    if (!q) return;
    answers[q.question] = label ?? [...picked].join(", ");
    picked = new Set();
    index++;
    if (index >= (State.pendingQuestion?.questions.length ?? 0)) {
      const done = answers;
      key = "";
      actions.answerQuestions(done);
    } else {
      key = "";
      State.notify();
    }
  }

  return {
    el,
    sync() {
      const pq = State.pendingQuestion;
      if (!pq) {
        // Legacy path: a Notification that ends in "?" — read-only.
        const task = State.focusTask;
        const k = `note:${task?.steps.at(-1) ?? ""}`;
        if (k === key) return;
        key = k;
        clear(who);
        who.append(agentWho(task, `${agentName(task)} tiene una pregunta`));
        title.textContent = task?.steps.at(-1) ?? "Necesita una respuesta.";
        clear(options);
        clear(row);
        row.append(h("div", { class: "sub", text: "Responde en tu terminal." }));
        setQuestionHeight(160);
        return;
      }
      const k = `${pq.requestId}:${index}:${[...picked].join("|")}`;
      if (k === key) return;
      if (!key.startsWith(pq.requestId)) {
        index = 0;
        answers = {};
        picked = new Set();
      }
      key = `${pq.requestId}:${index}:${[...picked].join("|")}`;
      const q = pq.questions[index];
      if (!q) return;
      const owner = State.tasks.find((t) => t.id === pq.agentId) ?? State.focusTask;
      clear(who);
      const count = pq.questions.length > 1 ? ` · ${index + 1}/${pq.questions.length}` : "";
      who.append(agentWho(owner, `${q.header || "pregunta"}${count}`));
      title.textContent = q.question;
      clear(options);
      for (const o of q.options) {
        const on = picked.has(o.label);
        const b = h("button", {
          class: on ? "q-opt on" : "q-opt",
          title: o.description ?? "",
          onclick: () => {
            if (q.multiSelect) {
              if (picked.has(o.label)) picked.delete(o.label);
              else picked.add(o.label);
              key = "";
              State.notify();
            } else {
              next(o.label);
            }
          },
        }, h("span", { class: "q-label", text: o.label }),
        o.description ? h("span", { class: "q-desc", text: o.description }) : null);
        options.append(b);
      }
      clear(row);
      const elsewhere = pq.agentId === CURSOR_AGENT_ID ? "Responder en Cursor" : "Responder en la terminal";
      row.append(btn(elsewhere, "secondary", () => {
        key = "";
        actions.answerQuestions(null);
      }));
      if (q.multiSelect) {
        const nextBtn = btn(index + 1 < pq.questions.length ? "Siguiente" : "Enviar", "primary", () => next(null));
        if (picked.size === 0) nextBtn.setAttribute("disabled", "");
        row.append(nextBtn);
      }
      setQuestionHeight(118 + q.options.length * 30 + 44);
    },
  };
}

// ── Diff ──────────────────────────────────────────────────────────────────────

function buildDiff(actions: ViewActions): ViewHost {
  const head = h("div", { class: "who-row" });
  const box = h("div", { class: "diff-box" });
  const back = btn("Volver", "secondary", () => actions.setView(State.defaultView()));
  const el = h("div", { class: "view" },
    card(null, stack(84, 16, head, box, h("div", { class: "actions" }, back))));
  let key = "";
  return {
    el,
    sync() {
      const task = State.tasks.find((t) => t.id === State.diffTaskId) ?? State.focusTask;
      const d = task?.lastDiff;
      const k = `${task?.id}:${d?.file}:${d?.added}:${d?.removed}:${d?.lines.length}`;
      if (k === key) return;
      key = k;
      clear(head);
      clear(box);
      if (!task || !d) {
        head.append(h("span", { text: "Todavía no hay ediciones." }));
        return;
      }
      head.append(dot(task.color, 8), h("span", { class: "n", text: d.file }),
        h("span", { class: "add", text: ` +${d.added}` }), h("span", { class: "del", text: ` −${d.removed}` }));
      for (const line of d.lines) {
        const sign = line.kind === "add" ? "+ " : line.kind === "del" ? "- " : "  ";
        box.append(h("div", { class: `dl ${line.kind}`, text: sign + line.text }));
      }
    },
  };
}

// ── Error ─────────────────────────────────────────────────────────────────────

function buildError(actions: ViewActions): ViewHost {
  const who = h("div");
  const title = h("div", { class: "title", text: "El flujo se detuvo." });
  const detail = h("div", { class: "detail" });
  const reply = createBotReply(replyHandlers(actions));
  const row = h("div", { class: "actions" },
    btn("Reintentar", "primary", () => actions.setView(State.defaultView())),
    btn("Abrir en n8n", "secondary", () => actions.openUrl("")),
  );
  const el = h("div", { class: "view" }, card("red", stack(116, 16, who, title, detail, reply.el, row)));
  return {
    el,
    sync() {
      const task = State.focusTask;
      clear(who);
      who.append(agentWho(task, task?.source === "n8n" ? "n8n" : agentName(task)));
      title.textContent = task?.source === "n8n" ? "El flujo se detuvo." : "La sesión se detuvo por un error.";
      detail.textContent = task?.steps.at(-1) ?? "Sin más detalles.";
      const can = botCanReply(task);
      reply.el.style.display = can ? "" : "none";
      if (can) reply.sync(task);
    },
  };
}

// ── Finished ──────────────────────────────────────────────────────────────────

function buildFinished(actions: ViewActions): ViewHost {
  const who = h("div");
  const title = h("div", { class: "title" });
  const terminal = btn("Abrir terminal", "primary", () => actions.openTerminal());
  const row = h("div", { class: "actions" },
    terminal,
    btn("OK", "secondary", () => actions.collapse()),
  );
  const reply = createBotReply(replyHandlers(actions));
  const el = h("div", { class: "view" }, card("green", stack(116, 16, who, title, reply.el, row)));
  return {
    el,
    sync() {
      // A Grok Bot works in the cloud: there is no terminal to open.
      terminal.style.display = State.focusTask?.id.startsWith(BOT_PREFIX) ? "none" : "";
      clear(who);
      who.append(agentWho(State.focusTask, `${agentName(State.focusTask)} terminó`));
      const task = State.focusTask;
      const isBot = !!task?.id.startsWith(BOT_PREFIX);
      // A Bot's answer is the message itself, whole, not the step it left.
      const said = isBot ? task?.lastMessage : null;
      // A Bot's answer keeps its formatting (bold, code, lists, links).
      clear(title);
      if (said) title.append(renderMarkdown(said));
      else title.textContent = task?.steps.at(-1) ?? "Sesión terminada";
      title.classList.toggle("bot-reply-msg", isBot);
      const can = botCanReply(task);
      reply.el.style.display = can ? "" : "none";
      if (can) reply.sync(task);
    },
  };
}

/** The quick reply sends like the conversation does and holds the island while typing. */
function replyHandlers(actions: ViewActions): BotReplyHandlers {
  return {
    send: (slug, text) => sendWithOutbox(slug, text),
    focus: (on) => actions.botReplyFocus(on),
  };
}

// ── Confused ──────────────────────────────────────────────────────────────────

function buildConfused(): ViewHost {
  const body = h(
    "div",
    { class: "stack", style: "padding:0 18px 0 128px" },
    h("div", { class: "title", text: "¡Demasiados golpes a la vez!" }),
    h("div", { class: "sub", text: "Dame un segundo: vuelvo al trabajo en tres." }),
  );
  return { el: h("div", { class: "view" }, card("pink", body)), sync() {} };
}

// ── Note ──────────────────────────────────────────────────────────────────────

function buildNote(): ViewHost {
  const title = h("div", { class: "title" });
  const el = h("div", { class: "view" }, card(null, h("div", { class: "stack", style: "padding:0 18px 0 98px" }, title)));
  return {
    el,
    sync() {
      title.textContent = State.noteMessage ?? "";
    },
  };
}

// ── In-island settings ────────────────────────────────────────────────────────

function buildSettings(actions: ViewActions): ViewHost {
  const soundSwitch = h("button", { class: "switch", onclick: () => actions.toggleSound() });
  const volume = h("input", {
    type: "range", min: "0", max: "0.2", step: "0.005",
    oninput: (e: Event) => actions.setVolume(Number((e.target as HTMLInputElement).value)),
  }) as HTMLInputElement;
  const autoLabel = h("span", {});
  const segButtons = [10, 15, 30].map((s) =>
    h("button", { onclick: () => actions.setAutoClose(s) }, `${s}s`),
  );
  const claudeBadge = h("span", { class: "status-badge" });
  const apiBadge = h("span", { class: "status-badge" });

  const rows = h(
    "div",
    { class: "settings-rows" },
    h("div", { class: "settings-row" }, soundSwitch, h("span", { text: "Sonido" }), volume),
    h(
      "div",
      { class: "settings-row" },
      svg(ICONS.timer, 12),
      autoLabel,
      h("div", { class: "seg" }, ...segButtons),
    ),
    h(
      "div",
      { class: "settings-row", style: "gap:14px" },
      claudeBadge,
      apiBadge,
      h("div", { class: "grow" }),
      h("button", {
        class: "link-btn",
        style: "color:#8e939c;font-size:11.5px",
        text: "Ajustes…",
        onclick: () => actions.openSettingsWindow(),
      }),
    ),
  );

  const el = h("div", { class: "view" },
    card(null, h("div", { class: "stack", style: "padding:14px 16px 14px 84px" }, rows)));

  return {
    el,
    sync() {
      const s = State.settings;
      soundSwitch.classList.toggle("on", s.soundEnabled);
      volume.value = String(s.soundVolume);
      volume.style.opacity = s.soundEnabled ? "1" : "0.4";
      autoLabel.textContent = `Cierre automático · ${Math.round(s.autoCloseInterval)}s`;
      segButtons.forEach((b, i) => b.classList.toggle("on", s.autoCloseInterval === [10, 15, 30][i]));
      clear(claudeBadge);
      claudeBadge.style.display = s.showAgents ? "" : "none";
      claudeBadge.append(
        dot(s.hooksInstalled ? "#22C55E" : "#F4505E", 6),
        h("span", { text: "Claude Code" }),
      );
      clear(apiBadge);
      const provider = aiProvider(s.provider);
      apiBadge.append(dot(provider.color, 6), h("span", { text: provider.name }));
    },
  };
}

// ── Placeholders filled in later stages ───────────────────────────────────────

function buildPlaceholder(title: string, sub: string): ViewHost {
  const body = h(
    "div",
    { class: "stack", style: "padding:0 18px 0 118px" },
    h("div", { class: "title", text: title }),
    h("div", { class: "sub", text: sub }),
  );
  return { el: h("div", { class: "view" }, card(null, body)), sync() {} };
}

// ── Registry ──────────────────────────────────────────────────────────────────

export function buildViews(
  actions: ViewActions,
  onChatHeightChange: () => void,
): Map<IslandViewName, ViewHost> {
  const map = new Map<IslandViewName, ViewHost>();
  map.set("overview", buildOverview(actions));
  map.set("empty", buildEmpty(actions));
  map.set("approval", buildApproval(actions));
  map.set("question", buildQuestion(actions));
  map.set("diff", buildDiff(actions));
  map.set("error", buildError(actions));
  map.set("finished", buildFinished(actions));
  map.set("confused", buildConfused());
  map.set("note", buildNote());
  map.set("settings", buildSettings(actions));
  map.set("prompt", buildPrompt(onChatHeightChange));
  // Not in the Windows v1: sending a file by email, window attach + web result.
  map.set("mail", buildPlaceholder("Enviar por correo no está en esta versión.", ""));
  map.set("searching", buildPlaceholder("Buscando…", ""));
  map.set("result", buildPlaceholder("Resultado", ""));
  return map;
}
