// Island views — DOM ports of IslandViewContent.swift. Paddings, font sizes,
// colours and wording are copied from the Swift views so both platforms read
// identically.

import { h, svg, clear, dot } from "./dom";
import { ICONS } from "./icons";
import { Ticker } from "./ticker";
import { ASSISTANT_ID, BOT_PREFIX, CURSOR_AGENT_ID, MOCHI_TASK, State, aiProvider, botPhase, type AgentTask } from "../core/state";
import { washRGBA, type IslandViewName, type Wash } from "../core/layout";
import { createMiniBot, pruneMiniBots } from "../mochi/minibots";
import { buildPrompt } from "./chat";
import { buildChoose, buildUpload, buildUploading } from "./upload";
import {
  botCanReply, createBotChat, createBotReply, reducedMotion, renderIntegrationCard,
  type BotReplyHandlers, type IntegrationCardHooks,
} from "./integrations";
import { pillDefinition, sessionSubtitle } from "../core/pills";
import {
  PlanCard, buildPlanPill, claudePillVisible, codexPillVisible, planCardOpen, refreshCodexPlanUsage,
} from "./usage";
import { buildDiffCard } from "./diff";
import { lastTextStep } from "../core/diff";
import { Bridge } from "../core/bridge";
import { buildRecap } from "./recap";
import { buildWardrobe } from "./wardrobe";
import type { Outfit, OutfitSelection } from "../mochi/wardrobe";
import { hasChat, sendWithOutbox } from "../core/botchat";
import { loadBotApprovals, type BotApproval } from "../core/botlog";
import { BotLive } from "../core/botlive";
import { renderMarkdown } from "./markdown";
import { buildChoices } from "./options";
import { CURSOR_WRITE } from "../core/botcmds";
import { language, t, tl, type Msg } from "../i18n/i18n";

export interface ViewActions {
  setView(v: IslandViewName): void;
  /** "Cancel" on a dropped file: forgets it and goes back home. */
  cancelDrop(): void;
  collapse(): void;
  /** Folds a waiting card to the compact island without answering it. */
  foldApproval(): void;
  setFocus(id: string): void;
  openTerminal(): void;
  /** The ↗ button: opens whatever the focused pill points at (not on Grok Bots). */
  openTarget(): void;
  openUrl(url: string): void;
  /** `answer`: the option picked (or text typed) when the request came with choices. */
  decide(d: "allow" | "deny" | "always", answer?: string): void;
  /** Answers the questions an agent asked: question text → chosen label(s). */
  answer(answers: Record<string, string | string[]>): void;
  /** Hands the pending request back to the terminal. */
  answerInTerminal(): void;
  toggleSound(): void;
  setVolume(v: number): void;
  setAutoClose(seconds: number): void;
  openSettingsWindow(): void;
  blip(): void;
  /** Wardrobe click: keeps the outfit ("auto" and "none" included). */
  chooseOutfit(selection: OutfitSelection): void;
  /** Wardrobe hover: shows an outfit on Mochi without keeping it; null ends it. */
  previewOutfit(outfit: Outfit | null): void;
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
  /** Called every frame while the view is on screen. True = needs another frame. */
  tick?(nowMs: number): boolean | void;
}

// ── Shared pieces ─────────────────────────────────────────────────────────────

function card(wash: Wash, ...children: (Node | string)[]): HTMLElement {
  const el = h("div", { class: wash ? "card wash" : "card" }, ...children);
  if (wash) el.style.setProperty("--wash", washRGBA(wash));
  return el;
}

function btn(
  label: string | Msg,
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
  const tabHome = h("button", { class: "tab", title: tl("Overview"), onclick: () => go("overview") }, svg(ICONS.house, 13));
  const tabChat = h("button", { class: "tab", title: tl("Ask"), onclick: () => go("prompt") }, svg(ICONS.bubble, 13));
  const tabDrop = h("button", { class: "tab", title: tl("Drop"), onclick: () => go("upload") }, svg(ICONS.plus, 13));

  const gearBtn = h("button", { title: tl("Settings"), onclick: () => go("settings") }, svg(ICONS.gear, 14));
  const soundBtn = h("button", { title: tl("Mute"), onclick: () => actions.toggleSound() }, svg(ICONS.speakerOn, 14));
  // Plan usage pills (off by default): before the gear, Claude first, as on the Mac.
  const claudePill = buildPlanPill(false);
  const codexPill = buildPlanPill(true);
  const planPills = h("div", { class: "plan-pills" }, claudePill.el, codexPill.el);
  let codexShown = false;

  function go(v: IslandViewName) {
    actions.blip();
    actions.setView(v);
  }

  const el = h(
    "div",
    { id: "header" },
    h("div", { class: "tabs" }, tabHome, tabChat, tabDrop),
    h("div", { class: "header-actions" }, planPills, gearBtn, soundBtn),
  );
  const headerActions = el.lastElementChild as HTMLElement;

  return {
    el,
    sync() {
      const v = State.view;
      tabHome.classList.toggle("on", v === "overview" || v === "empty");
      tabChat.classList.toggle("on", v === "prompt");
      tabDrop.classList.toggle("on", v === "upload");
      gearBtn.classList.toggle("on", v === "settings");
      clear(gearBtn);
      gearBtn.append(svg(v === "settings" ? ICONS.gearFill : ICONS.gear, 14));
      clear(soundBtn);
      soundBtn.append(svg(State.settings.soundEnabled ? ICONS.speakerOn : ICONS.speakerOff, 14));
      syncPlanPills();
      el.style.opacity = v === "confused" ? "0" : "1";
    },
  };

  function syncPlanPills() {
    const claudeOn = claudePillVisible();
    const codexOn = codexPillVisible();
    claudePill.el.style.display = claudeOn ? "" : "none";
    codexPill.el.style.display = codexOn ? "" : "none";
    planPills.classList.toggle("on", claudeOn || codexOn);
    // Both pills: the right side tightens so it still clears the screen edge.
    headerActions.classList.toggle("both-plans", claudeOn && codexOn);
    if (claudeOn) claudePill.sync();
    if (codexOn) codexPill.sync();
    // Codex is asked when its pill comes into view (stale answers only).
    const shown = codexOn && State.mode === "expanded";
    if (shown && !codexShown) refreshCodexPlanUsage();
    codexShown = shown;
  }
}

// ── Overview ──────────────────────────────────────────────────────────────────

function buildOverview(actions: ViewActions): ViewHost {
  /** The diff open in the left card (a FileDiff id), as activeDiffId on macOS. */
  let activeDiffId: number | null = null;
  const closeDiff = () => {
    if (activeDiffId == null) return;
    activeDiffId = null;
    State.notify();
  };
  const ticker = new Ticker((diffId) => {
    actions.blip();
    activeDiffId = diffId;
    State.notify();
  });
  const who = h("div", { class: "who" });
  const tickerBody = h("div", { class: "card-body" }, who, ticker.el);
  const leftBody = h("div", { class: "left-body" });
  const jump = h(
    "button",
    // Grok Bots have no ↗: their whole card opens the conversation.
    { class: "icon-btn jump", title: tl("Open"), onclick: () => actions.openTarget() },
    svg(ICONS.arrowUpRight, 8),
  );
  // Answering a Bot's message right under it, without opening the conversation.
  const reply = createBotReply(replyHandlers(actions));
  const replySlot = h("div", { class: "bot-reply-slot" }, reply.el);
  replySlot.style.display = "none";
  const left = card(null, leftBody, jump, replySlot);
  // The whole card of a focused Grok Bot (or Cursor) opens its conversation;
  // the buttons and boxes inside it keep their own clicks.
  left.addEventListener("click", (e) => {
    const id = State.focusTask?.id;
    if (!id || !hasChat(id) || botDetail.id || activeDiffId != null || planCardOpen()) return;
    if ((e.target as Element).closest("button, a, input, textarea, select, .bot-reply")) return;
    actions.openBotDetail(id);
  });
  const pills = h("div", { class: "pills" });
  const right = card(null, pills);
  // Opened from a plan pill in the header: stands in for the left card.
  const plan = new PlanCard();
  let planTimer: number | null = null;

  // The expanded Grok Bot detail sits over the overview while it is open.
  const detail = createBotChat({
    back: () => closeDetailAnimated(),
    send: (slug, text) => void sendWithOutbox(slug, text),
    // Exactly the approval card's decide(): audit line, botFx flash, sound.
    decide: (d, answer) => actions.decide(d, answer),
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
  let mode: "ticker" | "card" | "plan" | "diff" | null = null;
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

  // Leaving the overview or folding the island closes the diff, as on macOS.
  State.subscribe(() => {
    if (activeDiffId != null && (State.view !== "overview" || State.mode !== "expanded")) {
      activeDiffId = null;
    }
  });
  // Escape steps back out of the diff before it closes the island.
  window.addEventListener(
    "keydown",
    (e) => {
      if (e.key !== "Escape" || activeDiffId == null || State.view !== "overview") return;
      e.stopImmediatePropagation();
      closeDiff();
    },
    true,
  );

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

  /** The countdowns move every 30 s while a card is open, and only then. */
  function syncPlanTimer(open: boolean) {
    const stop = () => {
      if (planTimer != null) window.clearInterval(planTimer);
      planTimer = null;
    };
    if (!open) return stop();
    if (planTimer != null) return;
    planTimer = window.setInterval(() => {
      if (planCardOpen() && State.mode === "expanded") State.notify();
      else stop();
    }, 30_000);
  }

  return {
    el,
    /** Files dropped on a Bot: the cursor goes to the open conversation's box. */
    focus() {
      if (layer.style.display !== "none") detail.focus();
    },
    tick(nowMs: number) {
      if (mode !== "ticker") return false;
      ticker.tick(nowMs);
      return ticker.animating;
    },
    sync() {
      const task = State.focusTask;
      if (task?.id !== lastFocus) {
        lastFocus = task?.id ?? null;
        detailOpen = false;
        activeDiffId = null;
        cardKey = "";
        mode = null;
      }

      // A workspace or agent pill with a live session keeps the ticker; every
      // other pill shows its own card, exactly like IntegrationCardView.
      const sessionActive = task != null && hasSessionTicker(task);
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
      // Files dragged over this card go to the Bot or Cursor (island.ts reads data-bot-drop).
      if (task && hasChat(task.id) && !showDetail) left.dataset.botDrop = task.id;
      else delete left.dataset.botDrop;

      const planOpen = planCardOpen();
      if (mode === "plan" && !planOpen) {
        mode = null;
        cardKey = "";
      }
      syncPlanTimer(planOpen);

      // A diff that has since been dropped (cap, expiry, session end) just closes.
      const diff = task && activeDiffId != null ? State.findDiff(task.id, activeDiffId) : null;
      if (!diff) activeDiffId = null;

      const replying = isBot && !showDetail && !planOpen && !diff && botCanReply(task);
      replySlot.style.display = replying ? "" : "none";
      if (replying && task) reply.sync(task);

      if (planOpen) {
        if (mode !== "plan") {
          clear(leftBody);
          leftBody.append(plan.el);
          mode = "plan";
        }
        plan.sync();
      } else if (task && diff) {
        const key = `diff~${task.id}~${diff.id}`;
        if (key !== cardKey) {
          cardKey = key;
          mode = "diff";
          clear(leftBody);
          leftBody.append(buildDiffCard(diff, {
            dismiss: () => {
              actions.blip();
              closeDiff();
            },
            open: (path) => void Bridge.openFileInVSCode(path),
          }));
        }
      } else if (task && sessionActive) {
        if (mode !== "ticker") {
          clear(leftBody);
          leftBody.append(tickerBody);
          mode = "ticker";
          cardKey = "";
        }
        clear(who);
        // The agent's name is already the pill's: the label says what kind of
        // pill it is, as on the Mac (PillDefinition.sessionSubtitle).
        who.append(
          dot(task.color, 7),
          h("span", { class: "name", text: task.name, style: isBot ? `color:${task.color}` : undefined }),
          h("span", { class: "tool", text: isBot ? botLabel(task) : t(sessionSubtitle(task.id)) }),
        );
        if (task.steps.length > 1) {
          who.append(h("span", {
            class: "count",
            text: `${Math.min(task.stepIndex + 1, task.steps.length)}/${task.steps.length}`,
          }));
        }
        ticker.sync(task);
      } else if (task) {
        const info = State.integrations[task.id];
        const key = [
          language(), task.id, detailOpen, task.state, task.steps.join("|"),
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

      jump.style.display = detailOpen || isBot || mode === "plan" || mode === "diff" ? "none" : "";

      const others = State.otherTasks.slice(0, 4);
      const noBots = State.settings.grokBots.length === 0;
      // A Bot pill also redraws when its state word changes.
      const pillKey = others
        .map((t) => `${t.id}:${t.pillBadge ?? ""}:${BotLive.avatarState(t)}:${t.id.startsWith(BOT_PREFIX) ? `${t.state}:${t.color}:${t.name}:${BotLive.step(t.id)?.text ?? ""}` : ""}`)
        .join("|") + (noBots ? "|+bot" : "") + `|${language()}`;
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

/**
 * IntegrationCardView.agentSessionActive: a workspace tool or an agent — or
 * any other tagged agent — with something going on.
 */
export function hasSessionTicker(task: AgentTask): boolean {
  // A Grok Bot at rest shows its card (last answer, quick reply), not a ticker.
  if (task.id.startsWith(BOT_PREFIX)) return task.state !== "idle" || task.steps.length > 0;
  // A pill made for an agent's session (Gemini CLI, Codex… not declared) only
  // exists while that session does: it keeps the ticker from the first event.
  if (task.source === "agent" && !task.isIntegration) return true;
  const category = pillDefinition(task.id)?.category;
  const isSession = category === "workspace" || category === "agent" ||
    (category == null && task.id.startsWith("agent_"));
  return isSession && (task.state !== "idle" || task.steps.length > 0);
}

/** A Grok Bot's grey label: its latest live step while it works, else its phase. */
function botLabel(task: AgentTask): string {
  const phase = botPhase(task.state);
  // Working: its latest live step (bot-step) says more than "trabajando".
  const live = phase?.label === "trabajando" ? BotLive.step(task.id)?.text : null;
  if (live) return live;
  return phase ? `${t("Grok Bot")} · ${phase.label}` : t("Grok Bot");
}

function buildPill(task: AgentTask, actions: ViewActions): HTMLElement {
  const label = task.id === "integration_claude" ? "VS Code" : task.name;
  const canvas = createMiniBot(task, 24);
  const isBotPill = task.id.startsWith(BOT_PREFIX);
  // Grok Bots say what they are doing under their name: trabajando, pregunta,
  // listo, error. Nothing when idle.
  const mood = BotLive.avatarState(task);
  // «Recibido» for a moment after a message reached it (no reply yet).
  const phase = isBotPill
    ? (mood === "recibido" ? { label: "recibido", color: "#22C55E" } : botPhase(task.state))
    : null;
  const lbl = phase
    ? h("span", { class: "lbl two" },
        h("span", { class: "lbl-name", text: label, style: `color:${task.color}` }),
        h("span", { class: "pill-state" }, h("i", { style: `background:${phase.color}` }),
          // Working: the latest live step instead of just "trabajando".
          h("span", { class: "pill-step", text: (phase.label === "trabajando" ? BotLive.step(task.id)?.text : null) ?? phase.label })))
    : h("span", { class: "lbl", text: label, style: isBotPill ? `color:${task.color}` : undefined });
  const pill = h(
    "div",
    {
      class: isBotPill ? "pill pill-bot" : "pill",
      title: phase ? `${label} · ${phase.label}` : label,
      // A Grok Bot's pill opens its conversation straight away.
      onclick: () => (hasChat(task.id) ? actions.openBotDetail(task.id) : actions.setFocus(task.id)),
    },
    canvas,
    lbl,
  );
  // The avatar's mood (style.css animates the mini-bot's frame, not its canvas).
  pill.dataset.botState = mood;
  // Each pill nods on its own rhythm, so a row of them never moves in step.
  pill.style.setProperty("--nod-delay", `${(Math.random() * 6).toFixed(2)}s`);
  pill.style.setProperty("--nod-period", `${(6 + Math.random() * 4).toFixed(2)}s`);
  // A Grok Bot pill wears its own colour (Mis Bots de Grok); the state word stays neutral.
  if (isBotPill) {
    pill.style.borderColor = `${task.color}99`;
    pill.style.background = `${task.color}1F`;
    // A file dragged over the pill lights it up in the Bot's colour (island.ts).
    pill.dataset.botDrop = task.id;
    pill.style.setProperty("--bot", task.color);
  } else {
    pill.style.borderColor = `${task.color}24`;
    // Files dropped on Cursor's pill go with its next order.
    if (task.id === CURSOR_AGENT_ID) {
      pill.dataset.botDrop = task.id;
      pill.style.setProperty("--bot", task.color);
    }
  }
  pill.addEventListener("mouseenter", () => {
    pill.style.background = `${task.color}2e`;
    pill.style.borderColor = `${task.color}8c`;
    pill.style.boxShadow = `0 2px 10px ${task.color}59`;
    (pill.querySelector(".lbl") as HTMLElement).style.color = lighten(task.color, 0.3);
  });
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
    { class: "pill pill-add", title: tl("Settings → My Grok Bots"), onclick: () => actions.openSettingsWindow() },
    h("span", { class: "pill-add-icon" }, svg(ICONS.plus, 12)),
    h("span", { class: "lbl", text: tl("Connect a Grok Bot") }),
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
      h("div", { class: "title", text: tl("Nothing running right now.") }),
      h("div", { class: "sub", text: tl("Drop a file or window, or ask me anything.") }),
    ),
    h("div", { class: "grow" }),
    btn(tl("Ask Mochi"), "primary", () => actions.setView("prompt")),
  );
  return { el: h("div", { class: "view" }, card(null, body)), sync() {} };
}

// ── Approval ──────────────────────────────────────────────────────────────────

/** How long a fresh permission card ignores clicks on its buttons. */
const CLICK_GUARD_MS = 600;

/**
 * The ⌃ in the corner of a waiting card: folds the island to its compact size
 * and leaves the request waiting — nothing is answered (Mac #290). Opening the
 * island again brings the card back.
 */
function foldButton(actions: ViewActions): HTMLElement {
  return h(
    "button",
    { class: "icon-btn fold", title: tl("Later — keep it waiting"), onclick: () => actions.foldApproval() },
    svg(ICONS.chevronUp, 8, { stroke: 2.4 }),
  );
}

function buildApproval(actions: ViewActions): ViewHost {
  const who = h("div");
  const code = h("div", { class: "code" });
  const detail = h("pre", { class: "approval-detail" });
  // "Escribir en Cursor": the order can be long; «Ver todo» gives it more room.
  const more = h("button", { class: "link-btn approval-more", text: tl("See all") });
  more.addEventListener("click", () => {
    const open = !detail.classList.contains("open");
    detail.classList.toggle("open", open);
    more.textContent = open ? t("See less") : t("See all");
  });
  const row = h("div", { class: "actions" });
  const choices = h("div", { class: "approval-choices" });
  const el = h("div", { class: "view" },
    card("amber", stack(116, 16, who, code, detail, more, choices, row), foldButton(actions)));
  let rowKey = "";
  let detailFor = "";
  let choicesFor = "";
  // The card pops up under a cursor that was busy with something else: a click
  // meant for the window underneath must not land on Allow. Clicks in the first
  // moments after a new request appears are ignored.
  let shownFor: string | null = null;
  let shownAt = 0;
  const guarded = (d: "allow" | "deny" | "always", answer?: string) => {
    if (performance.now() - shownAt < CLICK_GUARD_MS) return;
    actions.decide(d, answer);
  };
  return {
    el,
    sync() {
      const req = State.pendingApproval;
      if ((req?.requestId ?? null) !== shownFor) {
        shownFor = req?.requestId ?? null;
        shownAt = performance.now();
      }
      clear(who);
      const isBot = !!req?.pillId.startsWith(BOT_PREFIX);
      const isQuestion = req?.tool === "Pregunta";
      if (req?.pillId === ASSISTANT_ID) {
        who.append(agentWho({ ...MOCHI_TASK }, t("wants to use {tool}", { tool: req.tool.replace(/_/g, " ") })));
      } else if (req && isBot) {
        // A Grok Bot: its name and colour, then what it wants — a yes/no
        // question (`--status ask`) or a tool on this PC with its arguments.
        const owner = State.tasks.find((x) => x.id === req.pillId) ?? null;
        who.append(agentWho(owner, isQuestion
          ? `${t("Grok Bot")} · ${t("is asking")}`
          : `${t("Grok Bot")} · ${t("wants to use {tool}", { tool: req.tool })}`));
      } else {
        const owner = State.tasks.find((x) => x.id === req?.pillId) ?? State.focusTask;
        who.append(agentWho(owner, isQuestion ? t("is asking") : t("needs permission")));
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
        const order = req?.tool === CURSOR_WRITE.tool && !!req.detail;
        detail.classList.toggle("order", order);
        detail.classList.remove("open");
        more.textContent = t("See all");
        more.style.display = order ? "" : "none";
        detail.scrollTop = 0;
      }
      // Choices: one button each (the answer goes with "allow"), built once per request.
      const hasChoices = !!req?.options?.length;
      if (choicesFor !== (hasChoices ? req!.requestId : "")) {
        choicesFor = hasChoices ? req!.requestId : "";
        clear(choices);
        if (hasChoices) {
          choices.append(buildChoices({
            options: req!.options!, allowCustom: !!req!.allowCustom, onPick: (a) => guarded("allow", a),
          }));
        }
        choices.style.display = hasChoices ? "" : "none";
      }
      // Built once per request shape. Rebuilding them between a mouse-down and a
      // mouse-up would swallow the click. "Always" only exists when Claude Code
      // suggested a rule to remember (or for Mochi's own tools).
      const key = `${language()}:${hasChoices ? "choices" : req?.allowAlways ? "always" : "plain"}`;
      if (rowKey === key) return;
      rowKey = key;
      clear(row);
      row.append(btn(tl("Deny"), "secondary", () => guarded("deny"), "N"));
      if (hasChoices) return;
      if (req?.allowAlways) row.append(btn(tl("Always"), "secondary", () => guarded("always"), "A"));
      row.append(btn(tl("Allow"), "primary", () => guarded("allow"), "Y"));
    },
  };
}

// ── Question ──────────────────────────────────────────────────────────────────

function buildQuestion(actions: ViewActions): ViewHost {
  const who = h("div");
  const title = h("div", { class: "title question-text" });
  const row = h("div", { class: "actions options" });
  const fold = foldButton(actions);
  const el = h("div", { class: "view" }, card("cyan", stack(116, 16, who, title, row), fold));

  // Where we are in the request on screen: which question, what is answered so
  // far, and what is ticked in a pick-several question.
  let requestId = "";
  let index = 0;
  let answers: Record<string, string | string[]> = {};
  let picked = new Set<string>();
  // The buttons are only rebuilt when what they show changes: rebuilding them
  // between a mouse-down and a mouse-up would swallow the click.
  let rowKey = "";

  const next = (question: string, answer: string | string[], total: number) => {
    answers[question] = answer;
    picked = new Set();
    index += 1;
    if (index >= total) actions.answer(answers);
    else State.notify();
  };

  return {
    el,
    sync() {
      const questions = State.pendingApproval?.questions;
      clear(who);
      // Only a request that is waiting can be folded away and come back.
      fold.style.display = State.pendingApproval ? "" : "none";

      // A question that arrived as a notification has nothing to pick from.
      if (!questions) {
        who.append(agentWho(State.focusTask, t("is asking a question")));
        const task = State.focusTask;
        title.textContent = (task && lastTextStep(task.steps)) ?? t("Claude needs an answer.");
        if (rowKey !== "terminal") {
          rowKey = "terminal";
          clear(row);
          row.append(h("div", { class: "sub", text: tl("Answer it in your terminal.") }));
        }
        return;
      }

      if (State.pendingApproval!.requestId !== requestId) {
        requestId = State.pendingApproval!.requestId;
        index = 0;
        answers = {};
        picked = new Set();
      }
      const q = questions[Math.min(index, questions.length - 1)];
      const asking = questions.length > 1
        ? t("is asking ({index} of {total})", { index: index + 1, total: questions.length })
        : t("is asking");
      const owner = State.tasks.find((x) => x.id === State.pendingApproval!.pillId) ?? State.focusTask;
      who.append(agentWho(owner, asking));
      title.textContent = q.question;
      title.title = q.question;

      const key = `${language()}:${requestId}:${index}:${[...picked].join("|")}`;
      if (rowKey === key) return;
      rowKey = key;
      clear(row);
      for (const option of q.options) {
        const on = picked.has(option.label);
        const button = btn(option.label, on ? "primary" : "secondary", () => {
          if (!q.multiSelect) {
            next(q.question, option.label, questions.length);
            return;
          }
          if (on) picked.delete(option.label);
          else picked.add(option.label);
          State.notify();
        });
        if (option.description) button.title = option.description;
        row.append(button);
      }
      if (q.multiSelect) {
        const done = btn(tl("Done"), "primary", () => {
          if (picked.size > 0) next(q.question, [...picked], questions.length);
        });
        if (picked.size === 0) done.classList.add("off");
        row.append(done);
      }
      // Cursor's questions are answered in its own chat, not in a terminal.
      const inCursor = State.pendingApproval!.pillId === CURSOR_AGENT_ID && State.pendingApproval!.tool === "AskUserQuestion";
      row.append(
        h("button", {
          class: "link-btn",
          style: "color:#8e939c",
          text: inCursor ? tl("Answer in Cursor") : tl("Answer in terminal"),
          onclick: () => actions.answerInTerminal(),
        }),
      );
    },
  };
}

// ── Error ─────────────────────────────────────────────────────────────────────

function buildError(actions: ViewActions): ViewHost {
  const who = h("div");
  const title = h("div", { class: "title" });
  const detail = h("div", { class: "detail" });
  const reply = createBotReply(replyHandlers(actions));
  const row = h("div", { class: "actions" },
    btn(tl("Retry"), "primary", () => actions.setView(State.defaultView())),
    btn(tl("Open in n8n"), "secondary", () => actions.openUrl("")),
  );
  const el = h("div", { class: "view" }, card("red", stack(116, 16, who, title, detail, reply.el, row)));
  return {
    el,
    sync() {
      const task = State.focusTask;
      clear(who);
      // agentWho already shows an agent's name: its label is just the kind.
      const whoLabel = task?.source === "n8n" ? "n8n" : task?.source === "agent" ? t("Agent") : "Claude Code";
      who.append(agentWho(task, whoLabel));
      title.textContent = task?.source === "n8n" ? t("Workflow stopped.") : t("Session stopped on an error.");
      detail.textContent = (task && lastTextStep(task.steps)) ?? t("No detail available.");
      const can = botCanReply(task);
      reply.el.style.display = can ? "" : "none";
      if (can && task) reply.sync(task);
    },
  };
}

// ── Finished ──────────────────────────────────────────────────────────────────

function buildFinished(actions: ViewActions): ViewHost {
  const who = h("div");
  const title = h("div", { class: "title one-line" });
  const open = btn(tl("Open terminal"), "primary", () => actions.openTerminal());
  const row = h("div", { class: "actions" },
    open,
    btn(tl("OK"), "secondary", () => actions.collapse()),
  );
  const reply = createBotReply(replyHandlers(actions));
  const el = h("div", { class: "view" }, card("green", stack(116, 16, who, title, reply.el, row)));
  return {
    el,
    sync() {
      const task = State.focusTask;
      const isBot = !!task?.id.startsWith(BOT_PREFIX);
      // A Grok Bot works in the cloud: there is no terminal to open.
      open.style.display = isBot ? "none" : "";
      clear(who);
      const agent = task?.source === "agent";
      who.append(agentWho(task, agent ? t("finished") : t("Claude Code finished")));
      // A Bot's answer is the message itself, whole and with its formatting
      // (bold, code, lists, links), not the step it left.
      const said = isBot ? task?.lastMessage : null;
      clear(title);
      title.classList.toggle("one-line", !said);
      title.classList.toggle("bot-reply-msg", isBot);
      if (said) renderMarkdown(title, said);
      // The final message, else the last step that is not a diff (FinishedView).
      else title.textContent = task?.finalLine || (task && lastTextStep(task.steps)) || t("Session finished");
      // Sessions from the Claude desktop app live there, not in a terminal.
      const label = task?.id === "agent_claude-desktop" ? t("Open Claude") : t("Open terminal");
      const span = open.firstElementChild as HTMLElement;
      if (span.textContent !== label) span.textContent = label;
      const can = botCanReply(task);
      reply.el.style.display = can ? "" : "none";
      if (can && task) reply.sync(task);
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
    h("div", { class: "title", text: tl("Too many hits at once.") }),
    h("div", { class: "sub", text: tl("Give me a sec — back to work in three seconds.") }),
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
    h("div", { class: "settings-row" }, soundSwitch, h("span", { text: tl("Sound") }), volume),
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
        text: tl("Settings…"),
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
      autoLabel.textContent = t("Auto-close · {seconds}s", { seconds: Math.round(s.autoCloseInterval) });
      segButtons.forEach((b, i) => b.classList.toggle("on", s.autoCloseInterval === [10, 15, 30][i]));
      clear(claudeBadge);
      claudeBadge.style.display = s.showAgents ? "" : "none";
      claudeBadge.append(
        dot(s.hooksInstalled ? "#22C55E" : "#F4505E", 6),
        h("span", { text: "Claude Code" }),
      );
      clear(apiBadge);
      // Mochi's AI provider (Ajustes → Mochi).
      const provider = aiProvider(s.provider);
      apiBadge.append(dot(provider.color, 6), h("span", { text: provider.name }));
    },
  };
}

// ── Placeholders filled in later stages ───────────────────────────────────────

function buildPlaceholder(title: Msg, sub: string): ViewHost {
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
  map.set("error", buildError(actions));
  map.set("finished", buildFinished(actions));
  map.set("confused", buildConfused());
  map.set("note", buildNote());
  map.set("settings", buildSettings(actions));
  map.set("prompt", buildPrompt(onChatHeightChange));
  map.set("upload", buildUpload());
  map.set("uploading", buildUploading());
  map.set("choose", buildChoose(actions));
  map.set("recap", buildRecap(actions));
  map.set("wardrobe", buildWardrobe(actions));
  // Not in the Windows v1: sending a file by email, window attach + web result.
  map.set("mail", buildPlaceholder(tl("Sending by email isn't in this version."), ""));
  map.set("searching", buildPlaceholder(tl("Searching…"), ""));
  map.set("result", buildPlaceholder(tl("Result"), ""));
  return map;
}
