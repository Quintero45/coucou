// The island: DOM shell, sizing animation, Mochi placement, mouse handling.
// Mirrors IslandRootView.swift + IslandWindowController.swift.

import { Tracked, Spring, clamp } from "../core/anim";
import { Bridge, IS_TAURI, onDragDrop, onEvent } from "../core/bridge";
import {
  EXPANDED_CORNER, EXPANDED_W, NOTCH_W, PANEL_H, PANEL_W,
  ROUNDED_CORNER, VIEW_LAYOUTS, botGlowColor, botGlowOpacity, botPosition, chatPromptHeight,
  islandSize,
  type IslandMode, type IslandViewName,
} from "../core/layout";
import { Sound } from "../core/sound";
import { recordBotApproval } from "../core/botlog";
import { Outbox, attachPaths, botErrorText } from "../core/attachments";
import { sendToAll } from "../core/botchat";
import { saveSettingsMerged } from "../core/savesettings";
import { BotLive } from "../core/botlive";
import { registerBotEvents } from "./botevents";
import { registerCursorVoice } from "./cursorvoice";
import { ASSISTANT_ID, BOT_PREFIX, State } from "../core/state";
import { BotEngine, hexToRGB } from "../mochi/engine";
import { Greeting } from "../mochi/greeting";
import { createMiniBot, pruneMiniBots, syncMiniBotStates, tickMiniBots } from "../mochi/minibots";
import { UploadCanvas } from "../upload/canvas";
import { USC, UploadSeq } from "../upload/sequence";
import { botDetail, buildHeader, buildViews, type ViewActions, type ViewHost } from "../views/views";
import { botCanReply } from "../views/integrations";
import { botFx } from "../mochi/botfx";
import { h, clear } from "../views/dom";
import { IslandStateMachine } from "./fsm";

const BOT_OVERHANG = 40;
/** Same margin as the Rust hit test (src-tauri/src/island.rs). */
const HIT_MARGIN = 14;
/** Extra height while a Bot's quick-reply box is under its message. */
const REPLY_GROW = 34;
/** And a little more while files wait above it. */
const REPLY_CHIPS = 22;
/** Dragging over the overview but not over a Bot this long hands the file to Mochi. */
const DROP_DWELL_MS = 700;
/** The island just opened itself for a drag: this long to reach a Bot's pill first. */
const DROP_OPEN_GRACE_MS = 1500;
/** A drag that opened the island and left without dropping: fold back after this. */
const DROP_LEAVE_FOLD_MS = 1000;
/** Resting on the folded notch this long opens it, like a click. */
const HOVER_OPEN_MS = 250;
/** A Bot's answer toast next to the notch: width and how long it stays. */
const TOAST_W = 300;
const TOAST_MS = 6000;

/** The three views the drop sequence owns; leaving them stops the engine. */
const UPLOAD_VIEWS: ReadonlySet<IslandViewName> = new Set(["upload", "uploading", "choose"]);

/** Seconds between the drop and the moment the progress bar starts filling. */
const PRE_PROGRESS = USC.T_PROG_START - USC.T_DROP;

const modeOrder = (m: IslandMode) => (m === "hidden" ? 0 : m === "compact" ? 1 : 2);

/** The open Grok Bot detail takes the chat's geometry: same size, same Mochi spot. */
function layoutView(): IslandViewName {
  return botDetail.id && State.view === "overview" ? "prompt" : State.view;
}

/** A Bot's card was answered: a short glow on its mascot (mochi/botfx.ts). */
function flashBot(agentId: string) {
  botFx.flash(agentId);
}

export class Island {
  readonly fsm = new IslandStateMachine();

  private root: HTMLElement;
  private islandEl!: HTMLElement;
  private clipEl!: HTMLElement;
  private contentEl!: HTMLElement;
  private viewsEl!: HTMLElement;
  private botCanvas!: HTMLCanvasElement;
  private botGlow!: HTMLElement;
  private greetingCanvas!: HTMLCanvasElement;
  private miniGrid!: HTMLElement;
  private countdown!: HTMLElement;
  private wakeStrip!: HTMLElement;

  private header!: ViewHost;
  private views!: Map<IslandViewName, ViewHost>;
  actions!: ViewActions;
  private uploadCanvas!: UploadCanvas;

  private width = new Tracked(NOTCH_W);
  private height = new Tracked(0);
  private radius = new Tracked(ROUNDED_CORNER);
  private botCx = new Spring(46);
  private botCy = new Spring(16);
  private botSize = new Spring(10);

  private engine = new BotEngine();
  private greeting = new Greeting();

  private running = false;
  private lastFrame = 0;
  private dirty = true;
  private canvasPx = 0;

  // Rust starts the window at full size so the launch greeting has room.
  private collapsed = false;
  private collapseTimer: number | null = null;
  private wasInIsland = false;
  /** Last shape handed to Rust for the click-through test. */
  private pushedRect = { x: -1, y: -1, w: -1, h: -1 };
  private homeCollapseAt: number | null = null;

  // Bot hover → love (IslandWindowController.botHoverIn)
  private botHovering = false;
  private botHoverTimer: number | null = null;
  private lastLoveTime = 0;
  private botHoverStart = { x: 0, y: 0 };

  private confusedRecovery: number | null = null;
  private prevViewBeforeConfused: IslandViewName = "overview";
  private lastSyncedView: IslandViewName | null = null;
  /** A quick-reply box has the cursor: the island keeps the keyboard and stays open. */
  private replyHold = false;
  private lastReplyGrow = 0;
  /** "bots": a file is being dragged over the overview, looking for a Bot. */
  private dragMode: "bots" | null = null;
  private dropBot: string | null = null;
  private dropDwell: number | null = null;
  /** When a drag opened the island by itself (performance.now()), else null. */
  private dragOpenedAt: number | null = null;
  private dragFoldTimer: number | null = null;
  private lastOverLog = 0;
  /** Shift held (window keydown/keyup): a drop then goes to every Bot. */
  private shiftDown = false;
  private hoverOpenTimer: number | null = null;
  // Toast for a Bot that answered while the island was folded.
  private toastEl!: HTMLElement;
  private toastTimer: number | null = null;
  private toastShown = false;
  /** Last state seen per Bot pill, to spot "it just answered". */
  private botStates = new Map<string, string>();
  // Meeting / screen-share marks on the notch.
  private notchLive!: HTMLElement;
  private meetingTimer: number | null = null;

  /** Drop sequence bookkeeping: last tick played, and whether the ✓ has fired. */
  private uploadTens = 0;
  private uploadDone = false;

  constructor(root: HTMLElement) {
    this.root = root;
    this.build();
    this.wireFsm();
    this.wireInput();
    // Steps, files, meetings… pushed by Rust: listened from launch, folded or not.
    registerBotEvents();
    registerCursorVoice();
    State.subscribe(() => this.watchBots());
    this.engine.onDizzy = () => this.handleDizzy();
    this.greeting.onComplete = () => this.fsm.greetComplete();
    State.subscribe(() => {
      this.dirty = true;
      this.ensureRunning();
    });
  }

  // ── DOM ─────────────────────────────────────────────────────────────────────

  private build() {
    const actions: ViewActions = {
      setView: (v) => this.setView(v),
      collapse: () => this.collapse(),
      setFocus: (id) => {
        State.setFocus(id);
        Sound.play("blip");
      },
      openTerminal: () => {
        const cwd = State.focusTask?.sessionCwd ?? null;
        void Bridge.openInVSCode(cwd);
      },
      // The ↗ button — same targets as openAgentTarget() on macOS.
      openTarget: () => {
        const task = State.focusTask;
        if (!task) return;
        const urls: Record<string, string> = {
          integration_resend: "https://resend.com/emails",
          integration_vercel: "https://vercel.com/dashboard",
          integration_github: "https://github.com",
          integration_stripe: "https://dashboard.stripe.com/payments",
          integration_notion: "https://notion.so",
          integration_calcom: "https://app.cal.com/bookings",
        };
        if (task.id === "integration_claude") void Bridge.openInVSCode(task.sessionCwd ?? null);
        else if (task.id === "integration_n8n") void Bridge.openN8n();
        else if (urls[task.id]) void Bridge.openUrl(urls[task.id]);
      },
      openUrl: (url) => {
        if (url) void Bridge.openUrl(url);
      },
      // A tap on a Grok Bot: the island grows exactly as it does for the chat
      // (layoutView() below sizes the overview as "prompt" while it is open).
      openBotDetail: (id) => {
        botDetail.id = id;
        State.setFocus(id);
        State.lastActivity = performance.now();
        Sound.play("blip");
        this.animateGeometry(false);
        // The conversation has a text box: like the chat, the island takes the keyboard.
        void Bridge.focusWindow(true);
        State.notify();
      },
      closeBotDetail: () => {
        if (!botDetail.id) return;
        botDetail.id = null;
        this.animateGeometry(true);
        if (State.view !== "prompt" && !this.replyHold) void Bridge.focusWindow(false);
        State.notify();
      },
      botReplyFocus: (on) => this.holdForReply(on),
      decide: (d) => {
        const req = State.pendingApproval;
        void Bridge.log(`decide ${d} req=${req?.requestId ?? "none"}`);
        if (!req) return;
        if (d === "always" && !req.allowAlways) return;
        Sound.play(d === "deny" ? "blip" : "approve");
        void Bridge.approvalDecision(req.requestId, d);
        if (req.agentId.startsWith(BOT_PREFIX)) {
          recordBotApproval({
            bot: req.agentId.slice(BOT_PREFIX.length),
            name: State.tasks.find((t) => t.id === req.agentId)?.name ?? req.agentId.slice(BOT_PREFIX.length),
            tool: req.tool,
            decision: d,
            target: req.command,
            input: req.toolInput,
          });
          flashBot(req.agentId);
        }
        State.pendingApproval = null;
        State.isPinned = false;
        this.fsm.pinned = false;
        if (req.agentId === ASSISTANT_ID) {
          this.setView("prompt");
          return;
        }
        State.updateTask(req.agentId, "working");
        State.setPillBadge(req.agentId, null);
        this.setView(State.defaultView());
      },
      answerQuestions: (answers) => {
        const q = State.pendingQuestion;
        if (!q) return;
        void Bridge.log(`question ${answers ? "answered" : "to terminal"} req=${q.requestId}`);
        if (answers) {
          Sound.play("approve");
          void Bridge.questionAnswer(q.requestId, answers);
        } else {
          Sound.play("blip");
          void Bridge.approvalDecline(q.requestId);
        }
        State.pendingQuestion = null;
        State.isPinned = false;
        this.fsm.pinned = false;
        State.updateTask(q.agentId, "working");
        this.setView(State.defaultView());
      },
      showDiff: (taskId) => {
        State.diffTaskId = taskId;
        Sound.play("blip");
        this.setView("diff");
      },
      toggleSound: () => {
        State.settings.soundEnabled = !State.settings.soundEnabled;
        Sound.setEnabled(State.settings.soundEnabled);
        void saveSettingsMerged(State.settings);
        State.notify();
      },
      setVolume: (v) => {
        State.settings.soundVolume = v;
        Sound.setVolume(v);
        void saveSettingsMerged(State.settings);
        State.notify();
      },
      setAutoClose: (s) => {
        State.settings.autoCloseInterval = s;
        this.fsm.homeToPetitDelay = s;
        void saveSettingsMerged(State.settings);
        State.notify();
      },
      openSettingsWindow: () => void Bridge.openSettingsWindow(),
      blip: () => Sound.play("blip"),
    };
    this.actions = actions;

    this.wakeStrip = h("div", { id: "wake-strip" });
    this.botGlow = h("div", { id: "bot-glow" });
    this.botCanvas = h("canvas", { id: "bot-canvas" });
    this.greetingCanvas = h("canvas", { id: "greeting-canvas" });
    this.miniGrid = h("div", { id: "mini-grid" });
    this.countdown = h("div", { id: "countdown" });
    this.notchLive = h("div", { id: "notch-live" },
      h("i", { class: "notch-rec", title: "Reunión en curso" }),
      h("span", { class: "notch-rec-time" }),
      h("span", { class: "notch-share", title: "Compartiendo pantalla con un Bot" }, "Pantalla"));
    this.toastEl = h("div", { id: "bot-toast", role: "status" });
    this.toastEl.addEventListener("click", () => this.openFromToast());

    this.header = buildHeader(actions);
    this.views = buildViews(actions, () => this.animateGeometry(false));
    this.viewsEl = h("div", { id: "views" });
    for (const v of this.views.values()) this.viewsEl.append(v.el);
    this.contentEl = h("div", { id: "content" }, this.header.el, this.viewsEl);

    // The drop sequence draws the card, the bar and its own Mochi. It sits under
    // the header, which stays visible on top of it exactly as on macOS.
    this.uploadCanvas = new UploadCanvas({
      ask: () => {
        State.promptContext = State.droppedFile
          ? { kind: "file", name: State.droppedFile.name, path: State.droppedFile.path }
          : null;
        this.setView("prompt");
      },
      cancel: () => this.setView(State.defaultView()),
    });

    this.clipEl = h(
      "div",
      { id: "island-clip" },
      this.greetingCanvas,
      this.uploadCanvas.el,
      this.contentEl,
    );
    this.islandEl = h(
      "div",
      { id: "island" },
      this.clipEl,
      this.botGlow,
      this.botCanvas,
      this.miniGrid,
      this.countdown,
      this.notchLive,
    );

    const dpr = Math.min(2, window.devicePixelRatio || 1);
    this.greetingCanvas.width = Math.round(EXPANDED_W * dpr);
    this.greetingCanvas.height = Math.round(150 * dpr);
    this.greetingCanvas.style.width = `${EXPANDED_W}px`;
    this.greetingCanvas.style.height = "150px";

    this.root.append(this.wakeStrip, this.islandEl, this.toastEl);
    this.applyGeometry();
  }

  // ── FSM ─────────────────────────────────────────────────────────────────────

  private wireFsm() {
    this.fsm.homeToPetitDelay = State.settings.autoCloseInterval;
    this.fsm.onTransition = (from, to) => {
      switch (to) {
        case "hidden":
          this.setMode("hidden");
          break;
        case "petit":
          if (from === "coucou") this.greeting.interrupt();
          else if (from === "hidden") Sound.play("peek");
          this.setMode("compact");
          if (from === "coucou") State.view = State.defaultView();
          if (!this.wasInIsland) this.fsm.mouseLeft();
          break;
        case "home":
          this.expand(State.defaultView());
          if (!this.wasInIsland) this.fsm.mouseLeft();
          break;
        case "coucou":
          this.expand("greeting");
          this.greeting.start();
          break;
      }
      State.notify();
    };
  }

  launch() {
    this.fsm.launch();
  }

  // ── Mode / view ─────────────────────────────────────────────────────────────

  private setMode(mode: IslandMode) {
    const prev = State.mode;
    if (mode === prev) return;
    State.mode = mode;
    if (mode === "expanded") Sound.play("open");
    if (prev === "expanded") {
      Sound.play("close");
      State.isPinned = false;
      void Bridge.focusWindow(false);
    }
    if (mode !== "expanded") {
      this.engine.resetMorph();
      // Nothing can be seen of the sequence once the island is shut, and leaving
      // it running would keep the frame loop awake — the island must cost
      // nothing while hidden.
      UploadSeq.deactivate();
    }
    this.updateWindowCollapsed();
    this.animateGeometry(modeOrder(mode) < modeOrder(prev));
    State.notify();
  }

  /** True while the drop sequence owns the island body. */
  private get uploadActive(): boolean {
    return State.mode === "expanded" && UploadSeq.isActive && UPLOAD_VIEWS.has(State.view);
  }

  /** Navigating out of the drop flow ends the sequence, as on macOS. */
  private stopSequenceIfLeaving(view: IslandViewName) {
    if (UploadSeq.isActive && !UPLOAD_VIEWS.has(view)) UploadSeq.deactivate();
  }

  expand(view: IslandViewName) {
    this.stopSequenceIfLeaving(view);
    State.view = view;
    if (State.mode !== "expanded") this.setMode("expanded");
    else this.animateGeometry(false);
    State.lastActivity = performance.now();
    this.homeCollapseAt = null;
    State.notify();
  }

  setView(view: IslandViewName) {
    // The Bot detail belongs to the overview; any other view closes it.
    if (view !== "overview" && botDetail.id) {
      botDetail.id = null;
      if (view !== "prompt") void Bridge.focusWindow(false);
    }
    this.stopSequenceIfLeaving(view);
    if (State.mode !== "expanded") {
      this.fsm.forceHome();
      State.view = view;
      this.animateGeometry(false);
      State.notify();
      return;
    }
    const grew = VIEW_LAYOUTS[view].height >= VIEW_LAYOUTS[State.view].height;
    State.view = view;
    State.lastActivity = performance.now();
    this.animateGeometry(!grew);
    State.notify();
  }

  collapse() {
    State.isPinned = false;
    this.fsm.pinned = false;
    // Drive the state machine rather than the mode: setting the mode behind its
    // back left it thinking the island was still open, and a click on the compact
    // island then did nothing — the island could never be reopened.
    this.fsm.forcePetit();
  }

  /** Alert from the hook server: open on this view. Pinned alerts never auto-close. */
  alert(view: IslandViewName) {
    this.fsm.pinned = State.isPinned || this.replyHold;
    this.fsm.forceHome();
    this.expand(view);
  }

  /**
   * A Bot's quick-reply box got or lost the cursor. While it has it the island
   * takes the keyboard (as the chat does) and does not fold on its own; after,
   * it gives the keyboard back and the usual auto-close resumes.
   */
  private holdForReply(on: boolean) {
    if (on) {
      void Bridge.focusWindow(true);
      if (this.replyHold) return;
      this.replyHold = true;
      this.fsm.pinned = true;
      if (this.fsm.state === "home") this.fsm.cancelTimers();
      this.homeCollapseAt = null;
      return;
    }
    if (!this.replyHold) return;
    this.replyHold = false;
    this.fsm.pinned = State.isPinned;
    if (State.view !== "prompt" && !botDetail.id) void Bridge.focusWindow(false);
    // The mouse left while typing: start the countdown it would have started.
    if (!this.wasInIsland && this.fsm.state === "home" && !State.isPinned) {
      this.fsm.mouseLeft();
      this.homeCollapseAt = performance.now() + State.settings.autoCloseInterval * 1000;
    }
  }

  /** How much the island grows for a Bot's quick-reply box (0 when there is none). */
  private replyGrow(): number {
    if (State.mode !== "expanded") return 0;
    const v = State.view;
    const where = v === "finished" || v === "error" || (v === "overview" && !botDetail.id);
    const task = State.focusTask;
    if (!where || !botCanReply(task)) return 0;
    const slug = task.id.slice(BOT_PREFIX.length);
    return REPLY_GROW + (Outbox.list(slug).length > 0 || Outbox.notice(slug) ? REPLY_CHIPS : 0);
  }

  reveal() {
    this.fsm.reveal();
  }

  /** An alert stopped waiting for an answer: let the island auto-close again. */
  dropPin() {
    this.fsm.pinned = false;
  }

  // ── File drop ───────────────────────────────────────────────────────────────

  private onDragDrop(e: { type: string; paths?: string[]; position?: { x: number; y: number }; shift?: boolean }) {
    this.logDrag(e);
    if (State.paused) return;
    switch (e.type) {
      case "enter":
      case "over": {
        this.cancelDragFold();
        if (State.fileDragOver) return;
        // There are Bots to drop on: open the island on the overview (even from
        // folded) and let the drag find one, lit in its colour, before Mochi
        // takes the file.
        if (this.dragMode === "bots" || this.hasBotTargets()) {
          if (this.dragMode !== "bots") this.openForDrag();
          this.dragMode = "bots";
          const [x, y] = this.dragPoint(e);
          this.trackDropTarget(x, y);
          break;
        }
        this.startMochiDrag();
        break;
      }
      case "leave": {
        if (this.dragMode === "bots") {
          const opened = this.dragOpenedAt != null;
          this.endBotDrag();
          // It opened by itself for this drag: fold back unless the mouse stays.
          if (opened) this.scheduleDragFold();
          return;
        }
        if (!State.fileDragOver) return;
        State.fileDragOver = false;
        this.engine.animateMorph(0);
        // The island deliberately stays open: the drag session is still alive.
        UploadSeq.exitZone();
        State.notify();
        break;
      }
      case "drop": {
        this.cancelDragFold();
        // Shift + drop: the files go to every Bot. Rust says whether Shift was
        // held (payload.shift); the keydown/keyup tracking only stands in when
        // the field is missing.
        const shift = typeof e.shift === "boolean" ? e.shift : this.shiftDown;
        if (shift && e.paths?.length && State.settings.grokBots.length > 0) {
          this.endBotDrag();
          State.fileDragOver = false;
          this.engine.animateMorph(0);
          void this.dropToAll(e.paths);
          return;
        }
        if (this.dragMode === "bots") {
          const [x, y] = this.dragPoint(e);
          const id = this.botUnder(x, y) ?? this.dropBot;
          this.endBotDrag();
          if (id && e.paths?.length) {
            this.dropOnBot(id, e.paths);
            return;
          }
          // Not on a Bot: Mochi eats it, exactly as before.
          if (e.paths?.length) this.startMochiDrag();
        }
        State.fileDragOver = false;
        const path = e.paths?.[0];
        if (!path) {
          this.engine.animateMorph(0);
          this.setView(State.defaultView());
          return;
        }
        this.swallow(path);
        break;
      }
    }
  }

  /** Mochi's drop sequence takes the drag (the island opens on `upload`). */
  private startMochiDrag() {
    State.fileDragOver = true;
    this.engine.animateMorph(1);
    // enterZone must run before the island expands, so the sequence is
    // already active by the time the view becomes `upload`.
    UploadSeq.enterZone(State.mouseInIsland.x, State.mouseInIsland.y);
    this.alert("upload");
  }

  /** "ui drag …" lines in coucou.log; over at most twice a second. */
  private logDrag(e: { type: string; paths?: string[]; position?: { x: number; y: number } }) {
    const now = performance.now();
    if (e.type === "over") {
      if (now - this.lastOverLog < 500) return;
      this.lastOverLog = now;
    }
    const p = e.position ? `${Math.round(e.position.x)},${Math.round(e.position.y)}` : "-";
    const line = `drag ${e.type} files=${e.paths?.length ?? 0} pos=${p} mode=${State.mode} view=${State.view} drag=${this.dragMode ?? (State.fileDragOver ? "mochi" : "none")} bot=${this.dropBot ?? "-"}`;
    console.debug(`[coucou] ${line}`);
    void Bridge.log(line);
  }

  /** A Bot is (or will be, once the island opens on the overview) on screen to drop on. */
  private hasBotTargets(): boolean {
    if (State.mode === "expanded" && State.view === "overview" && botDetail.id) return true;
    const focus = State.focusTask;
    if (focus?.id.startsWith(BOT_PREFIX)) return true;
    return State.otherTasks.slice(0, 4).some((t) => t.id.startsWith(BOT_PREFIX));
  }

  /**
   * Opens the island on the overview for a drag, with the usual expand
   * animation (as an alert does), unless it already shows it.
   */
  private openForDrag() {
    // Already on the overview: keep the time a drag-hover may have just opened it.
    if (State.mode === "expanded" && State.view === "overview") return;
    this.dragOpenedAt = performance.now();
    void Bridge.log(`drag open-overview from=${State.mode}/${State.view}`);
    // Folded or compact: the same opening an alert gets. Already open on
    // another view: just switch to the overview.
    if (State.mode !== "expanded") this.alert("overview");
    else this.setView("overview");
  }

  /** drag-hover from Rust: the same opening as the enter, a moment earlier. */
  private onDragHover(p: { x?: number; y?: number; collapsed?: boolean }) {
    void Bridge.log(`drag hover-evt collapsed=${p?.collapsed ?? "?"} pos=${Math.round(p?.x ?? 0)},${Math.round(p?.y ?? 0)} mode=${State.mode} view=${State.view}`);
    if (State.paused || State.fileDragOver || this.dragMode) return;
    if (!this.hasBotTargets()) return;
    this.cancelDragFold();
    this.openForDrag();
    // No enter followed (the drag went elsewhere): forget this opening.
    const at = this.dragOpenedAt;
    if (at != null) {
      window.setTimeout(() => {
        if (this.dragMode == null && this.dragOpenedAt === at) this.dragOpenedAt = null;
      }, 4000);
    }
  }

  private scheduleDragFold() {
    this.cancelDragFold();
    this.dragFoldTimer = window.setTimeout(() => {
      this.dragFoldTimer = null;
      if (this.dragMode || State.fileDragOver || this.replyHold) return;
      if (this.wasInIsland || State.mode !== "expanded") return;
      void Bridge.log("drag fold (left without dropping)");
      this.collapse();
    }, DROP_LEAVE_FOLD_MS);
  }

  private cancelDragFold() {
    if (this.dragFoldTimer != null) window.clearTimeout(this.dragFoldTimer);
    this.dragFoldTimer = null;
  }

  /** Drag position in window-logical px: the event's own, else the cursor poll. */
  private dragPoint(e: { position?: { x: number; y: number } }): [number, number] {
    const p = e.position;
    if (p && (p.x !== 0 || p.y !== 0)) {
      const dpr = window.devicePixelRatio || 1;
      return [p.x / dpr, p.y / dpr];
    }
    return [State.mouse.x, State.mouse.y];
  }

  /** The Bot (pill, focused card or open conversation) under a point, if any. */
  private botUnder(x: number, y: number): string | null {
    const hit = document.elementFromPoint(x, y)?.closest<HTMLElement>("[data-bot-drop]");
    return hit?.dataset.botDrop ?? null;
  }

  /** Lights the Bot under the drag; off any Bot for a while, Mochi takes over. */
  private trackDropTarget(x: number, y: number) {
    const id = this.botUnder(x, y);
    if (id !== this.dropBot) {
      this.dropBot = id;
      this.viewsEl.querySelectorAll(".bot-drop-target").forEach((el) => el.classList.remove("bot-drop-target"));
      if (id) {
        this.viewsEl
          .querySelectorAll(`[data-bot-drop="${CSS.escape(id)}"]`)
          .forEach((el) => el.classList.add("bot-drop-target"));
      }
    }
    if (id) {
      if (this.dropDwell != null) window.clearTimeout(this.dropDwell);
      this.dropDwell = null;
    } else if (this.dropDwell == null) {
      // Right after the island opened by itself, give time to reach the pill.
      const sinceOpen = this.dragOpenedAt == null ? Infinity : performance.now() - this.dragOpenedAt;
      const wait = Math.max(DROP_DWELL_MS, DROP_OPEN_GRACE_MS - sinceOpen);
      this.dropDwell = window.setTimeout(() => {
        this.dropDwell = null;
        if (this.dragMode !== "bots" || this.dropBot) return;
        this.endBotDrag();
        this.startMochiDrag();
      }, wait);
    }
  }

  private endBotDrag() {
    if (this.dropDwell != null) window.clearTimeout(this.dropDwell);
    this.dropDwell = null;
    this.dropBot = null;
    this.dragMode = null;
    this.dragOpenedAt = null;
    this.viewsEl.querySelectorAll(".bot-drop-target").forEach((el) => el.classList.remove("bot-drop-target"));
  }

  /** Files dropped on a Bot: into the inbox, then chips in its conversation. */
  /** Shift + drop: into the inbox once, then the same files to every Bot. */
  private async dropToAll(paths: string[]) {
    try {
      const files = await Bridge.ingestFiles(paths);
      const names = files.map((f) => f.name).join(", ");
      const items = files.map((f, i) => ({ key: `all${i}`, kind: "file" as const, id: f.id, name: f.name, mime: f.mime, size: f.size }));
      const r = await sendToAll(`Te comparto ${files.length === 1 ? "un archivo" : `${files.length} archivos`}: ${names}`, items);
      this.showToast({ id: null, color: r.ok ? "#22C55E" : "#F4505E", name: "Todos los bots", text: r.message });
    } catch (err) {
      this.showToast({ id: null, color: "#F4505E", name: "Todos los bots", text: botErrorText(err) });
    }
  }

  // ── Answer toast (a Bot replied while the island was folded) ─────────────────

  /** Watches the Bots' states: one that just answered while folded gets a toast. */
  private watchBots() {
    for (const t of State.tasks) {
      if (!t.id.startsWith(BOT_PREFIX)) continue;
      const prev = this.botStates.get(t.id);
      this.botStates.set(t.id, t.state);
      if (prev === undefined || prev === t.state) continue;
      const answered = t.state === "finished" || t.state === "question" || t.state === "error";
      if (!answered || State.mode === "expanded") continue;
      const first = (t.lastMessage ?? t.steps.at(-1) ?? "").split("\n").find((l) => l.trim()) ?? "";
      const text = first.replace(/[*_`#>]/g, "").trim() || (t.state === "error" ? "Algo falló" : "Terminó");
      void Bridge.log(`notify bot=${t.id.slice(BOT_PREFIX.length)} state=${t.state}`);
      // The hook already played its sound for this answer.
      this.showToast({ id: t.id, color: t.color, name: t.name, text });
    }
  }

  private toastFor: string | null = null;

  private showToast(n: { id: string | null; color: string; name: string; text: string }) {
    this.toastFor = n.id;
    clear(this.toastEl);
    this.toastEl.style.setProperty("--bot", n.color);
    this.toastEl.append(
      h("i", { class: "bot-toast-dot" }),
      h("div", { class: "bot-toast-body" },
        h("b", { class: "bot-toast-name", text: n.name }),
        h("span", { class: "bot-toast-text", text: n.text.slice(0, 160) })),
    );
    this.toastEl.title = n.id ? "Abrir la conversación" : "";
    // A folded island has no room for it: come out to the compact notch first.
    if (State.mode === "hidden") this.reveal();
    this.toastShown = true;
    this.toastEl.classList.remove("out");
    this.toastEl.classList.add("on");
    this.applyGeometry();
    if (this.toastTimer != null) window.clearTimeout(this.toastTimer);
    this.toastTimer = window.setTimeout(() => this.hideToast(), TOAST_MS);
  }

  private hideToast() {
    if (this.toastTimer != null) window.clearTimeout(this.toastTimer);
    this.toastTimer = null;
    if (!this.toastShown) return;
    this.toastShown = false;
    this.toastEl.classList.remove("on");
    this.toastEl.classList.add("out");
    this.applyGeometry();
  }

  private openFromToast() {
    const id = this.toastFor;
    this.hideToast();
    if (!id) return;
    if (State.mode !== "expanded") this.alert("overview");
    else this.setView("overview");
    this.actions.openBotDetail(id);
  }

  /** Red ring + timer while a meeting records, a mark while the screen is shared. */
  private syncNotchLive() {
    const meeting = !!BotLive.meeting?.active;
    const sharing = !!BotLive.screenShare?.active;
    this.islandEl.classList.toggle("meeting-live", meeting);
    this.islandEl.classList.toggle("sharing-live", sharing);
    this.notchLive.style.display = (meeting || sharing) && State.mode !== "hidden" ? "" : "none";
    const timeEl = this.notchLive.querySelector<HTMLElement>(".notch-rec-time")!;
    const tick = () => {
      const since = BotLive.meeting?.since;
      // since: epoch seconds or ms.
      const start = since ? (since < 1e12 ? since * 1000 : since) : null;
      const s = start ? Math.max(0, Math.floor((Date.now() - start) / 1000)) : 0;
      timeEl.textContent = start ? `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}` : "REC";
    };
    if (meeting) {
      tick();
      if (this.meetingTimer == null) this.meetingTimer = window.setInterval(tick, 1000);
    } else if (this.meetingTimer != null) {
      window.clearInterval(this.meetingTimer);
      this.meetingTimer = null;
    }
  }

  private dropOnBot(id: string, paths: string[]) {
    const slug = id.slice(BOT_PREFIX.length);
    if (botDetail.id !== id) this.actions.openBotDetail(id);
    else void Bridge.focusWindow(true);
    void attachPaths(slug, paths).then(() => {
      window.setTimeout(() => this.views.get("overview")?.focus?.(), 60);
    });
  }

  /**
   * Mochi eats the file. Nothing here waits on the file system: the copy into
   * the inbox runs in the background and swaps the path in when it lands, so a
   * slow disk can never stall the animation — same as FileDropHandler on macOS.
   */
  private swallow(path: string) {
    const name = path.split(/[\\/]/).pop() || "file";
    State.droppedFile = { name, path };
    State.promptContext = { kind: "file", name, path };
    State.chatHistory = [];
    void Bridge.chatReset();

    UploadSeq.performDrop(State.uploadDuration);
    this.uploadTens = 0;
    this.uploadDone = false;

    this.engine.gulp();
    Sound.play("approve");
    this.engine.triggerEmote("happy");
    this.engine.animateMorph(0);

    State.uploadProgress = 0;
    this.setView("uploading");
    this.ensureRunning();

    void Bridge.ingestFile(path)
      .then((file) => {
        State.droppedFile = { name: file.name, path: file.path };
        State.promptContext = { kind: "file", name: file.name, path: file.path };
        State.notify();
      })
      .catch((err) => {
        UploadSeq.deactivate();
        State.noteMessage = String(err).replace(/^Error:\s*/, "");
        this.engine.animateMorph(0);
        this.setView("note");
        Sound.play("error");
        window.setTimeout(() => this.setView(State.defaultView()), 2400);
      });
  }

  /**
   * Sounds and view changes hung off the canvas timeline: a `tick` every 10 %,
   * the ✓ chime when the bar completes, then `choose` once Mochi has grown back.
   */
  private stepSequence() {
    const since = UploadSeq.sinceDrop();
    if (since == null) return;
    const dur = State.uploadDuration;
    const p = Math.max(0, Math.min(1, (since - PRE_PROGRESS) / dur));

    const tens = Math.floor(p * 10);
    if (tens > this.uploadTens && tens < 10) {
      this.uploadTens = tens;
      Sound.play("tick");
    }

    if (!this.uploadDone && since >= PRE_PROGRESS + dur) {
      this.uploadDone = true;
      Sound.play("approve");
      this.engine.triggerEmote("happy");
    }
    // The extra second is the grow-back, after which the choose card is up.
    if (since >= PRE_PROGRESS + dur + 1 && State.view === "uploading") {
      this.setView("choose");
    }
  }

  // ── Geometry ────────────────────────────────────────────────────────────────

  private targetSize(): { w: number; h: number; r: number } {
    // A Bot conversation takes the chat's full-grown size (chatPromptHeight's cap).
    const chatCount = botDetail.id && State.view === "overview" ? 99 : State.chatHistory.length;
    const { w, h } = islandSize(State.mode, layoutView(), chatCount);
    const r = State.mode === "expanded" ? EXPANDED_CORNER : ROUNDED_CORNER;
    const grow = this.replyGrow();
    return { w, h: grow ? Math.min(PANEL_H, h + grow) : h, r };
  }

  private animateGeometry(shrinking: boolean) {
    const { w, h, r } = this.targetSize();
    if (shrinking) {
      this.width.curveTowards(w);
      this.height.curveTowards(h);
      this.radius.curveTowards(r);
    } else {
      this.width.springTo(w);
      this.height.springTo(h);
      this.radius.springTo(r);
    }
    this.ensureRunning();
  }

  private applyGeometry() {
    const w = this.width.value;
    const hh = this.height.value;
    const r = this.radius.value;
    this.islandEl.style.width = `${w}px`;
    this.islandEl.style.height = `${hh}px`;
    this.islandEl.style.borderRadius = `0 0 ${r}px ${r}px`;
    this.islandEl.style.transform = `translateX(-50%)`;
    // These follow the island as it resizes, so they belong here rather than in
    // the state-driven DOM sync.
    this.miniGrid.style.left = `${w - 40 - 14.5}px`;
    this.miniGrid.style.top = `${hh / 2 - 14.5}px`;
    this.greetingCanvas.style.left = `${(w - EXPANDED_W) / 2}px`;
    this.uploadCanvas.el.style.left = `${(w - EXPANDED_W) / 2}px`;

    let rect = { x: (PANEL_W - w) / 2, y: 0, w, h: hh };
    // The toast under the notch must take clicks too: widen the hit rect to it.
    if (this.toastShown) {
      const top = Math.max(hh, 6) + 8;
      this.toastEl.style.top = `${top}px`;
      const th = this.toastEl.offsetHeight || 52;
      rect = { x: Math.min(rect.x, (PANEL_W - TOAST_W) / 2), y: 0, w: Math.max(rect.w, TOAST_W), h: top + th };
    }
    const p = this.pushedRect;
    if (Math.abs(p.x - rect.x) > 0.5 || Math.abs(p.w - rect.w) > 0.5 || Math.abs(p.h - rect.h) > 0.5) {
      this.pushedRect = rect;
      void Bridge.setIslandRect(rect.x, rect.y, rect.w, rect.h);
    }
  }

  /** Island rect in window coordinates (origin top-left of the 720×320 window). */
  private islandRect(): { x: number; y: number; w: number; h: number } {
    const w = this.width.value;
    const hh = this.height.value;
    return { x: (PANEL_W - w) / 2, y: 0, w, h: hh };
  }

  // ── Window collapse (hidden → tiny wake strip, zero polling) ────────────────

  private updateWindowCollapsed() {
    if (this.collapseTimer != null) {
      window.clearTimeout(this.collapseTimer);
      this.collapseTimer = null;
    }
    if (State.mode === "hidden") {
      // Let the island finish retracting, then drop the window to the wake strip:
      // from there the OS delivers no cursor events, so nothing polls at all.
      this.collapseTimer = window.setTimeout(() => {
        this.collapseTimer = null;
        if (State.mode !== "hidden") return;
        this.collapsed = true;
        void Bridge.setCollapsed(true);
      }, 420);
    } else if (this.collapsed) {
      // Grow the window back before the island animates open.
      this.collapsed = false;
      void Bridge.setCollapsed(false);
    }
  }

  // ── Input ───────────────────────────────────────────────────────────────────

  private wireInput() {
    // The wake strip is the only thing the OS can hit while the island is hidden.
    this.wakeStrip.addEventListener("mouseenter", () => {
      Sound.resume();
      if (State.mode === "hidden") this.fsm.mouseEntered();
    });
    // The folded notch stays drawn over the strip: it wakes the island too.
    this.islandEl.addEventListener("mouseenter", () => {
      if (State.mode === "hidden") this.fsm.mouseEntered();
    });
    // Shift while dropping sends the files to every Bot. Only seen while the
    // island has the keyboard; the mouse events below catch it otherwise.
    window.addEventListener("keydown", (e) => { if (e.key === "Shift") this.shiftDown = true; });
    window.addEventListener("keyup", (e) => { if (e.key === "Shift") this.shiftDown = false; });
    window.addEventListener("blur", () => { this.shiftDown = false; });
    window.addEventListener("mousemove", (e) => { this.shiftDown = e.shiftKey; });

    this.islandEl.addEventListener("mousedown", (e) => {
      Sound.resume();
      State.lastActivity = performance.now();
      if (State.mode !== "expanded") {
        this.fsm.click();
        return;
      }
      if (this.isBotHit(e.clientX, e.clientY)) {
        this.cancelBotHover();
        this.engine.slap();
      }
    });

    window.addEventListener("keydown", (e) => {
      if (e.key === "Escape" && State.mode === "expanded" && !State.isPinned) this.collapse();
      // The Y / N / A hints on the approval buttons. Only while the card is on
      // screen and the island has focus — a key press is as explicit as a click.
      if (State.mode === "expanded" && State.view === "approval" && State.pendingApproval) {
        const k = e.key.toLowerCase();
        if (k === "y") this.actions.decide("allow");
        else if (k === "n") this.actions.decide("deny");
        else if (k === "a" && State.pendingApproval.allowAlways) this.actions.decide("always");
      }
      State.lastActivity = performance.now();
    });

    void onDragDrop((e) => this.onDragDrop(e));
    // Rust's early notice of a drag (once, before Tauri's enter), sent even
    // while the island is folded: open on the Bots in time.
    void onEvent<{ x?: number; y?: number; collapsed?: boolean }>("drag-hover", (p) => this.onDragHover(p));

    // Outside Tauri (plain browser) drive the cursor from DOM events so the
    // island can be inspected with `npm run dev`.
    if (!IS_TAURI) this.followPageCursor();
  }

  /**
   * Takes the cursor from the page's own mouse events instead of Rust's poll.
   * Used where the OS has no global cursor position (Wayland): the events only
   * fire while the pointer is over the island, so leaving the window is
   * reported as a cursor far away, which is what the poll would have said.
   */
  followPageCursor() {
    window.addEventListener("mousemove", (e) => this.onCursor(e.clientX, e.clientY));
    window.addEventListener("mouseout", (e) => {
      if (e.relatedTarget == null) this.onCursor(-10_000, -10_000);
    });
  }

  /** Cursor in window-logical coordinates. */
  onCursor(x: number, y: number) {
    State.mouse = { x, y };
    const rect = this.islandRect();
    State.mouseInIsland = { x: x - rect.x, y: y - rect.y };
    // Windows may send no position with the drag itself: follow the poll.
    if (this.dragMode === "bots") this.trackDropTarget(x, y);

    // Windows sends no cursor position with an OLE drag, so the drop sequence is
    // fed from the Win32 cursor poll instead — it runs throughout the drag.
    if (UploadSeq.isActive && !UploadSeq.dropped) {
      UploadSeq.updateCursor(State.mouseInIsland.x, State.mouseInIsland.y);
    }

    const inIsland =
      x >= rect.x - HIT_MARGIN && x <= rect.x + rect.w + HIT_MARGIN &&
      y >= rect.y - HIT_MARGIN && y <= rect.y + rect.h + HIT_MARGIN;

    if (inIsland && !this.wasInIsland) {
      if (this.fsm.state === "coucou") this.greeting.hover();
      this.fsm.mouseEntered();
      this.homeCollapseAt = null;
    }
    if (!inIsland && this.wasInIsland) {
      this.fsm.mouseLeft();
      if (this.fsm.state === "home" && !State.isPinned) {
        this.homeCollapseAt = performance.now() + State.settings.autoCloseInterval * 1000;
      }
    }
    this.wasInIsland = inIsland;

    // Resting on the folded notch opens it (≈250 ms), as a click would.
    const canHoverOpen = inIsland && this.fsm.state === "petit" && !this.dragMode && !State.fileDragOver;
    if (canHoverOpen && this.hoverOpenTimer == null) {
      this.hoverOpenTimer = window.setTimeout(() => {
        this.hoverOpenTimer = null;
        if (this.wasInIsland && this.fsm.state === "petit" && !this.dragMode && !State.fileDragOver) this.fsm.click();
      }, HOVER_OPEN_MS);
    } else if (!canHoverOpen && this.hoverOpenTimer != null) {
      window.clearTimeout(this.hoverOpenTimer);
      this.hoverOpenTimer = null;
    }

    // Bot hover → love
    const overBot = State.mode === "expanded" && State.stateOverride == null && this.isBotHit(x, y);
    if (overBot && !this.botHovering) this.botHoverIn(x, y);
    if (!overBot && this.botHovering) this.cancelBotHover();
    this.botHovering = overBot;
    if (this.botHovering) {
      const d = Math.hypot(x - this.botHoverStart.x, y - this.botHoverStart.y);
      if (d > 40) {
        this.botHoverStart = { x, y };
        this.scheduleLove();
      }
    }

    this.ensureRunning();
  }

  private isBotHit(x: number, y: number): boolean {
    const rect = this.islandRect();
    const cx = rect.x + this.botCx.value;
    const cy = rect.y + this.botCy.value;
    const radius = this.botSize.value / 2;
    return (x - cx) ** 2 + (y - cy) ** 2 <= radius * radius;
  }

  private botHoverIn(x: number, y: number) {
    if (performance.now() / 1000 - this.lastLoveTime < 6) return;
    this.botHoverStart = { x, y };
    this.engine.blink();
    this.engine.tgEs = 1.08;
    Sound.play("hover");
    this.scheduleLove();
  }

  private scheduleLove() {
    if (this.botHoverTimer != null) window.clearTimeout(this.botHoverTimer);
    this.botHoverTimer = window.setTimeout(() => {
      this.botHoverTimer = null;
      if (!this.botHovering || State.stateOverride != null) return;
      if (performance.now() / 1000 - this.lastLoveTime < 6) return;
      this.lastLoveTime = performance.now() / 1000;
      this.engine.triggerEmote("love");
      Sound.play("love");
    }, 1900);
  }

  private cancelBotHover() {
    if (this.botHoverTimer != null) window.clearTimeout(this.botHoverTimer);
    this.botHoverTimer = null;
    this.engine.tgEs = 1;
  }

  /** Three slaps → dizzy + confused view for 3.3 s, then back. */
  private handleDizzy() {
    this.prevViewBeforeConfused = State.view;
    State.stateOverride = "dizzy";
    this.engine.setState("dizzy");
    Sound.play("dizzy");
    this.alert("confused");
    if (this.confusedRecovery != null) window.clearTimeout(this.confusedRecovery);
    this.confusedRecovery = window.setTimeout(() => {
      this.confusedRecovery = null;
      State.stateOverride = null;
      this.engine.setState(State.effectiveState);
      if (State.view === "confused") {
        const fallback = State.defaultView();
        this.setView(this.prevViewBeforeConfused === "confused" ? fallback : this.prevViewBeforeConfused);
      }
      this.engine.triggerEmote("happy");
    }, 3300);
  }

  // ── Frame loop ──────────────────────────────────────────────────────────────

  ensureRunning() {
    if (this.running) return;
    this.running = true;
    this.lastFrame = performance.now();
    requestAnimationFrame(this.frame);
  }

  private frame = (nowMs: number) => {
    const dt = Math.min(0.05, (nowMs - this.lastFrame) / 1000);
    this.lastFrame = nowMs;

    this.width.step(dt, nowMs);
    this.height.step(dt, nowMs);
    this.radius.step(dt, nowMs);
    this.applyGeometry();

    if (this.dirty) {
      this.dirty = false;
      this.syncDom();
    }

    this.updateBotTargets();
    this.botCx.step(dt);
    this.botCy.step(dt);
    this.botSize.step(dt);

    const greetingActive = State.mode === "expanded" && State.view === "greeting";
    if (greetingActive) {
      const gctx = this.greetingCanvas.getContext("2d");
      if (gctx) {
        const dpr = Math.min(2, window.devicePixelRatio || 1);
        gctx.setTransform(dpr, 0, 0, dpr, 0, 0);
        this.greeting.draw(gctx);
      }
    } else {
      // Kept running even while the drop canvas is up, so the island's own Mochi
      // is already in the right place the moment the canvas fades out.
      this.drawBot(dt);
    }

    const uploadActive = this.uploadActive;
    if (uploadActive) this.uploadCanvas.draw(UploadSeq.frame(), nowMs / 1000);
    this.uploadCanvas.el.classList.toggle("on", uploadActive);
    this.viewsEl.classList.toggle("hidden-by-upload", uploadActive);

    tickMiniBots(dt);
    this.views.get(State.view)?.tick?.(nowMs);
    if (UploadSeq.isActive) this.stepSequence();
    this.updateCountdown(nowMs);

    // Nothing is drawn while the island is hidden, so nothing may keep the loop
    // alive either. This used to read `... || this.engine.busy || State.mode !==
    // "hidden"`, and engine.busy is permanently true for any state with a
    // looping animation — breathing, ratelimit sweat, sleeping z's, the search
    // sweep — so a hidden island went on burning frames in exactly the states it
    // spends most of its life in. Geometry still has to finish retracting.
    const settling =
      this.width.animating || this.height.animating || this.radius.animating;
    const busy = State.mode === "hidden"
      ? settling
      : settling ||
        !this.botCx.settled || !this.botCy.settled || !this.botSize.settled ||
        greetingActive || this.engine.busy || UploadSeq.isActive;

    if (busy) {
      requestAnimationFrame(this.frame);
    } else {
      this.running = false;
      Sound.idle();
    }
  };

  private updateBotTargets() {
    const p = botPosition(State.mode, layoutView(), this.height.value, State.uploadProgress);
    this.botCx.target = p.cx;
    this.botCy.target = p.cy;
    this.botSize.target = p.diameter / 0.6;

    const greetingActive = State.mode === "expanded" && State.view === "greeting";
    // The drop canvas draws its own Mochi; two of them would overlap.
    const visible = p.opacity > 0 && !greetingActive && !this.uploadActive;
    this.botCanvas.style.opacity = visible ? "1" : "0";

    if (State.mode === "expanded" && State.view !== "uploading" && !greetingActive && !this.uploadActive) {
      const d = p.diameter;
      const color = botGlowColor(State.effectiveState);
      this.botGlow.style.display = "block";
      this.botGlow.style.width = `${d * 2.2}px`;
      this.botGlow.style.height = `${d * 2.2}px`;
      this.botGlow.style.left = `${this.botCx.value - d * 1.1}px`;
      this.botGlow.style.top = `${this.botCy.value - d * 1.1}px`;
      this.botGlow.style.background = `radial-gradient(circle, ${color} 0%, transparent 62%)`;
      this.botGlow.style.opacity = String(botGlowOpacity(State.effectiveState));
    } else {
      this.botGlow.style.display = "none";
    }
  }

  private drawBot(dt: number) {
    const size = this.botSize.value;
    const w = Math.max(1, Math.round(size));
    const hCss = w + BOT_OVERHANG;
    const dpr = Math.min(2, window.devicePixelRatio || 1);
    if (this.canvasPx !== w) {
      this.canvasPx = w;
      this.botCanvas.width = Math.round(w * dpr);
      this.botCanvas.height = Math.round(hCss * dpr);
      this.botCanvas.style.width = `${w}px`;
      this.botCanvas.style.height = `${hCss}px`;
    }
    this.botCanvas.style.left = `${this.botCx.value - w / 2}px`;
    this.botCanvas.style.top = `${this.botCy.value - BOT_OVERHANG / 2 - hCss / 2}px`;

    const ctx = this.botCanvas.getContext("2d");
    if (!ctx) return;

    const focus = State.focusTask;
    this.engine.bodyColor = focus?.isIntegration ? hexToRGB(focus.color) : null;
    this.engine.particleOverhang = BOT_OVERHANG;
    this.engine.lookX = this.lookX();
    this.engine.lookY = this.lookY();
    if (this.engine.morph > 0.3) {
      this.engine.slotHTarget = State.fileDragOver ? 0.2 : 0;
    } else {
      this.engine.slotHTarget = 0;
      if (this.engine.morph < 0.05) {
        this.engine.slotH = 0;
        this.engine.slotHVel = 0;
      }
    }
    this.engine.update(dt);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, hCss);
    this.engine.draw(ctx, w, hCss);
  }

  /** BotCanvasView.lookX / lookY — tanh of the distance to the bot. */
  private lookX(): number {
    const rect = this.islandRect();
    const botScreenX = rect.x + this.botCx.value;
    return Math.tanh((State.mouse.x - botScreenX) / 260);
  }

  private lookY(): number {
    return -Math.tanh((State.mouse.y - this.botCy.value) / 200);
  }

  private updateCountdown(nowMs: number) {
    if (State.mode !== "expanded" || State.isPinned || this.homeCollapseAt == null) {
      this.countdown.style.width = "0px";
      return;
    }
    const autoClose = State.settings.autoCloseInterval;
    const windowS = Math.min(10, autoClose * 0.6);
    const remaining = (this.homeCollapseAt - nowMs) / 1000;
    this.countdown.style.width =
      remaining < windowS ? `${Math.max(0, clamp(remaining / windowS, 0, 1) * 160)}px` : "0px";
  }

  // ── DOM sync ────────────────────────────────────────────────────────────────

  private syncDom() {
    const expanded = State.mode === "expanded";
    const greetingActive = expanded && State.view === "greeting";

    this.contentEl.style.opacity = expanded && !greetingActive ? "1" : "0";
    this.contentEl.style.pointerEvents = expanded && !greetingActive ? "auto" : "none";
    this.greetingCanvas.style.display = greetingActive ? "block" : "none";

    this.header.sync();
    for (const [name, view] of this.views) {
      const on = name === State.view;
      view.el.classList.toggle("on", on);
      if (on) view.sync();
    }

    // The chat is the only view with a text field, so it is the only time the
    // island is allowed to take keyboard focus.
    if (this.lastSyncedView !== State.view) {
      const wasChat = this.lastSyncedView === "prompt";
      this.lastSyncedView = State.view;
      if (State.view === "prompt") {
        void Bridge.focusWindow(true);
        window.setTimeout(() => this.views.get("prompt")?.focus?.(), 120);
      } else if (wasChat) {
        void Bridge.focusWindow(false);
      }
    }

    // Compact mini grid
    const showGrid = State.mode === "compact";
    this.miniGrid.style.opacity = showGrid ? "1" : "0";
    if (showGrid) {
      const others = State.otherTasks.slice(0, 4);
      const key = others.map((t) => t.id).join("|");
      if (this.miniGrid.dataset.key !== key) {
        this.miniGrid.dataset.key = key;
        this.miniGrid.replaceChildren();
        for (const t of others) {
          this.miniGrid.append(createMiniBot(t, 13));
        }
        pruneMiniBots();
      }
    }

    // A quick-reply box appearing or going away resizes the island.
    const grow = this.replyGrow();
    if (grow !== this.lastReplyGrow) {
      const shrinking = grow < this.lastReplyGrow;
      this.lastReplyGrow = grow;
      this.animateGeometry(shrinking);
    }

    this.syncNotchLive();
    syncMiniBotStates(State.tasks);
    this.engine.setState(State.effectiveState);
  }

  /** Applies settings coming from Rust at boot. */
  applySettings() {
    Sound.setEnabled(State.settings.soundEnabled);
    Sound.setVolume(State.settings.soundVolume);
    this.fsm.homeToPetitDelay = State.settings.autoCloseInterval;
    State.notify();
  }

  get panelSize() {
    return { w: PANEL_W, h: PANEL_H };
  }

  get chatHeight() {
    return chatPromptHeight(State.chatHistory.length);
  }
}
