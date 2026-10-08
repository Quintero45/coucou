// App state — mirror of AppState.swift (the parts the island needs).

import type { BotEmoteName, BotStateName, IslandMode, IslandViewName } from "./layout";
import type { EyeShape } from "../aria/engine";
import {
  DEFAULT_MAIN_PILL, HOST_OS, availablePills, orderPills, pillDefinition, sanitizeDeclared,
  toggleDeclared, type HostOs, type PillDefinition,
} from "./pills";
import type { CodexPlanUsage, PlanUsage } from "./plan";
import type { FileDiff } from "./diff";
import type { Bindings } from "./shortcuts";
import { DEFAULT_OUTFIT, type Outfit } from "../aria/wardrobe";
import { t } from "../i18n/i18n";

export type AgentSource = "claudeCode" | "n8n" | "agent";
export type PillBadge = "approval" | "finished" | "error";

export interface AgentTask {
  id: string;
  name: string;
  color: string;
  state: BotStateName;
  stepIndex: number;
  steps: string[];
  /**
   * Position of the newest step in the whole session. `steps` is capped, so
   * `stepIndex` stops moving once it is full; this keeps counting.
   */
  stepSeq?: number;
  source: AgentSource;
  isIntegration: boolean;
  emote?: BotEmoteName | null;
  miniEye?: EyeShape | null;
  pillBadge?: PillBadge | null;
  sessionCwd?: string | null;
  /** Claude Code's session, so "Open terminal" can find the window it runs in. */
  sessionId?: string | null;
  /** Claude's final message after Stop, one line; cleared when a new turn starts. */
  finalLine?: string | null;
  /** Last thing the agent said in full (Cursor afterAgentResponse, Claude last_assistant_message). */
  lastMessage?: string | null;
}

export interface ApprovalInfo {
  requestId: string;
  sessionId: string;
  /** The pill the request belongs to: VS Code, Cursor, an agent (Codex…), a Grok Bot or ARIA. */
  pillId: string;
  tool: string;
  command: string;
  /** Set when an agent is asking questions rather than for a permission. */
  questions?: AskedQuestion[];
  /** Claude Code when it suggested a rule; ARIA's own tools for the session. */
  allowAlways?: boolean;
  /** Full text under review (a skill's code, a diff) — ARIA's requests only. */
  detail?: string | null;
  /** Wrap the detail as prose (a Bot's question, its arguments) instead of code. */
  detailWrap?: boolean;
  /** The raw tool_input, for a Grok Bot's log line (botlog.ts sanitises it). */
  toolInput?: Record<string, unknown> | null;
  /** One question with choices (botcmds.ts QUESTION_OPTIONS): one button each. */
  options?: QuestionOption[] | null;
  /** Offer an "Otra respuesta" box next to the choices. */
  allowCustom?: boolean;
}

export interface QuestionOption {
  label: string;
  description?: string;
}

/** One question of an AskUserQuestion call (Claude Code, or Cursor's AskQuestion). */
export interface AskedQuestion {
  question: string;
  options: { label: string; description: string }[];
  multiSelect: boolean;
}

export interface ChatMessage {
  id: number;
  role: "user" | "assistant";
  content: string;
  /** Tools the assistant ran while writing this reply. */
  steps?: string[];
}

/** Owner of the approval cards raised by ARIA's own tools (agent.rs). */
export const ASSISTANT_ID = "assistant";

/** How ARIA signs its own approval cards. */
export const ARIA_TASK: AgentTask = {
  id: ASSISTANT_ID, name: "ARIA", color: "#A78BFA", state: "approval", stepIndex: 0, steps: [],
  source: "agent", isIntegration: false,
};

export interface AiProvider {
  id: string;
  pill: string;
  name: string;
  color: string;
  /** Credential Manager key, null for local servers. */
  key: string | null;
  defaultModel: string;
  /** Local servers have an editable base URL. */
  defaultUrl?: string;
}

/** Same ids as providers.rs; pill ids from PillCatalog.swift, plus ai_xai. */
export const AI_PROVIDERS: AiProvider[] = [
  { id: "cursor", pill: "ai_cursor", name: "Cursor (Grok)", color: "#38BDF8", key: "cursor-api-key", defaultModel: "grok" },
  { id: "anthropic", pill: "ai_anthropic", name: "Anthropic", color: "#E07950", key: "anthropic-api-key", defaultModel: "claude-opus-5" },
  { id: "xai", pill: "ai_xai", name: "xAI Grok", color: "#F5F5F5", key: "xai-api-key", defaultModel: "grok-4" },
  { id: "openai", pill: "ai_openai", name: "OpenAI", color: "#10A37F", key: "openai-api-key", defaultModel: "gpt-5" },
  { id: "google", pill: "ai_google", name: "Google AI", color: "#4285F4", key: "google-api-key", defaultModel: "gemini-2.5-pro" },
  { id: "ollama", pill: "ai_ollama", name: "Ollama", color: "#FACC15", key: null, defaultModel: "llama3.2", defaultUrl: "http://localhost:11434/v1" },
  { id: "lmstudio", pill: "ai_lmstudio", name: "LM Studio", color: "#A3E635", key: null, defaultModel: "local-model", defaultUrl: "http://localhost:1234/v1" },
];

export function aiProvider(id: string): AiProvider {
  return AI_PROVIDERS.find((p) => p.id === id) ?? AI_PROVIDERS[0];
}

export type PromptContext =
  | { kind: "window"; appName: string; title: string; url?: string }
  | { kind: "file"; name: string; path?: string };

export interface ResultItem {
  label: string;
  detail: string;
  url?: string;
}

export interface SearchResult {
  title: string;
  items: ResultItem[];
  note?: string;
}

/** A fresh, idle task for a catalog pill. */
function taskFor(def: PillDefinition, name = def.name): AgentTask {
  return {
    id: def.id, name, color: def.color, state: "idle", stepIndex: 0, steps: [],
    source: def.source, isIntegration: true,
  };
}

/** Agent name (aria_agent) → its catalog pill, when there is one. */
export function catalogAgent(name: string): AgentTask | null {
  const def = pillDefinition(`agent_${name}`);
  return def ? { ...taskFor(def), isIntegration: false } : null;
}

/** One of the owner's Grok Bots (grokbot.rs). The webhook key stays in the Credential Manager. */
export interface GrokBot {
  id: string;
  name: string;
  color: string;
  url: string;
}

/** The family's default colours: a new Bot with one of these names starts with
    its colour; the picker in Ajustes always wins once a colour is saved. */
export const BOT_COLORS: Readonly<Record<string, string>> = {
  aegon: "#E5484D",
  aerys: "#3E8EF7",
  daemond: "#8E4EC6",
  daemon: "#8E4EC6",
};

/** The default colour for a Bot name/id (family colours), or null. */
export function defaultBotColor(name: string, id = ""): string | null {
  const norm = (s: string) => s.normalize("NFD").replace(/[\u0300-\u036f]/g, "").trim().toLowerCase();
  return BOT_COLORS[norm(id)] ?? BOT_COLORS[norm(name)] ?? null;
}

/** A Bot's colour in the island: the one chosen in Ajustes; the family default only if none is valid. */
export function botColor(bot: { id: string; name: string; color: string }): string {
  if (/^#[0-9a-fA-F]{6}$/.test(bot.color ?? "")) return bot.color;
  return defaultBotColor(bot.name, bot.id) ?? "#38BDF8";
}

/** Grok Bot pills: `agent_bot-<id>`, fed by `aria-hook --bot`. */
export const BOT_PREFIX = "agent_bot-";

export function botPillId(bot: GrokBot): string {
  return `${BOT_PREFIX}${bot.id}`;
}

export type BotPhaseName = "working" | "asking" | "done" | "error";

/** How a Grok Bot pill names its state, in the island's words (`label` is translated). */
export function botPhase(state: BotStateName): { phase: BotPhaseName; label: string; color: string } | null {
  switch (state) {
    case "working":
    case "thinking":
    case "searching":
      return { phase: "working", label: t("Working"), color: "#60A5FA" };
    case "approval":
    case "question":
      return { phase: "asking", label: t("Asking"), color: "#F5A524" };
    case "finished":
      return { phase: "done", label: t("Finished"), color: "#22C55E" };
    case "error":
      return { phase: "error", label: t("Error"), color: "#F4505E" };
    default:
      return null;
  }
}

export const CURSOR_AGENT_ID = "agent_cursor";

/** Claude Code (VS Code), Cursor IDE and the other coding agents — not a Grok Bot. */
export function isCodingAgentPill(id: string): boolean {
  return id === "integration_claude" || (id.startsWith("agent_") && !id.startsWith(BOT_PREFIX));
}

/** The Cursor agent follows showCursorAgent; the other coding agents follow showAgents. */
export function isHiddenAgent(id: string, s: Settings): boolean {
  if (id === CURSOR_AGENT_ID) return !s.showCursorAgent;
  return !s.showAgents && isCodingAgentPill(id);
}

/** What an integration poller last reported. */
export interface IntegrationInfo {
  data: Record<string, unknown>;
  error: string | null;
  loaded: boolean;
  configured: boolean;
}

export interface Settings {
  soundEnabled: boolean;
  soundVolume: number;
  autoCloseInterval: number;
  absenceInterval: number;
  /** Declared pills next to the main one (at most 4), in the order they were added. */
  activeIntegrations: string[];
  /** The always-on workspace pill: VS Code, Cursor, Codex or Antigravity. */
  mainPill: string;
  /** "primary", "cursor", or `at:<x>,<y>` for one display (logical origin). */
  screen: string;
  autostart: boolean;
  hooksInstalled: boolean;
  /** Claude model used by the assistant with Anthropic. */
  model: string;
  /** Holding Space alone shows the island (keyhold.rs). */
  spaceHold: boolean;
  /** AI provider the assistant talks to (providers.rs). */
  provider: string;
  /** Model per non-Anthropic provider; Anthropic keeps `model`. */
  providerModels: Record<string, string>;
  /** Base URL per local provider (Ollama, LM Studio). */
  providerUrls: Record<string, string>;
  /** Let the assistant use tools (files, PowerShell, apps, MCP…). */
  assistantTools: boolean;
  /** Claude Code (VS Code), Codex, Gemini CLI and the other agents: pills and settings. */
  showAgents: boolean;
  /** The Cursor IDE agent: pill, questions and hooks. */
  showCursorAgent: boolean;
  /** The owner's Grok Bots. */
  grokBots: GrokBot[];
  /** Show the Claude plan pill (5 h and weekly limits) in the island's header. */
  showPlanInNotch: boolean;
  /** ARIA's status line relay is installed in Claude Code's settings. */
  planRelayInstalled: boolean;
  /** Show the Codex plan pill in the island's header. */
  showCodexPlanInNotch: boolean;
  /** Global shortcuts the user changed, by action id (see core/shortcuts.ts). */
  shortcuts: Bindings;
  /**
   * ARIA's outfit: "auto" (dresses for the season), "none" or an outfit id.
   * Same raw values as the Mac's "ariaOutfit"; read it through parseOutfit.
   */
  ariaOutfit: string;
  /**
   * Interface language: "" follows the system (when ARIA has its language,
   * else English), or one of src/i18n's ten codes ("fr", "pt-BR", "zh-Hans"…).
   */
  language: string;
  /** ARIA on the desktop. Rust owns it: whatever the page sends back is ignored. */
  desktopAria?: {
    onDesktop: boolean;
    spot: { x: number; y: number; space: string } | null;
  };
}

export const DEFAULT_SETTINGS: Settings = {
  soundEnabled: true,
  soundVolume: 0.12,
  autoCloseInterval: 15,
  absenceInterval: 180,
  activeIntegrations: [
    "integration_resend", "integration_n8n", "integration_vercel", "integration_github",
  ],
  mainPill: DEFAULT_MAIN_PILL,
  screen: "primary",
  autostart: false,
  hooksInstalled: false,
  model: "claude-opus-5",
  spaceHold: true,
  provider: "cursor",
  providerModels: {},
  providerUrls: {},
  assistantTools: true,
  showAgents: false,
  showCursorAgent: true,
  grokBots: [],
  showPlanInNotch: false,
  planRelayInstalled: false,
  showCodexPlanInNotch: false,
  shortcuts: {},
  ariaOutfit: DEFAULT_OUTFIT,
  language: "",
};

type Listener = () => void;

/** Live diffs kept per pill (oldest dropped first) — same cap as macOS. */
export const MAX_DIFFS_PER_PILL = 50;
/** A pill's diffs are forgotten after an hour without a new one, as on macOS. */
export const DIFF_TTL_MS = 3_600_000;

class AppState {
  mode: IslandMode = "hidden";
  view: IslandViewName = "overview";

  tasks: AgentTask[] = [];
  focusId: string | null = null;

  stateOverride: BotStateName | null = null;

  /** Cursor in logical screen pixels, origin top-left (like AppState.mousePosition). */
  mouse = { x: 0, y: 0 };
  /** Cursor relative to the island's top-left corner. */
  mouseInIsland = { x: 0, y: 0 };

  isPinned = false;
  paused = false;

  uploadProgress = 0;
  uploadDuration = 2.4;
  fileDragOver = false;

  promptContext: PromptContext | null = null;
  droppedFile: { name: string; path: string } | null = null;
  noteMessage: string | null = null;
  /** Text to put in the chat input the next time it shows ("@Bot "). */
  chatDraft: string | null = null;
  searchResult: SearchResult | null = null;
  chatHistory: ChatMessage[] = [];
  pendingApproval: ApprovalInfo | null = null;
  /** The pill that was in front when the card came up; it comes back after. */
  focusBeforeApproval: string | null = null;

  integrations: Record<string, IntegrationInfo> = {};

  /** Claude's 5 h / weekly limits, from the status line (null until the first call). */
  planUsage: PlanUsage | null = null;
  /** Codex's limits, from `codex app-server` (null until it has answered). */
  codexPlanUsage: CodexPlanUsage | null = null;
  /** A plan card is open in place of the overview's left card. */
  showingPlanDetail = false;
  /** Which one: the Codex card rather than Claude's. */
  planDetailIsCodex = false;
  /** Per-pill file diffs, in order of reception. Steps carry their ids. */
  sessionDiffs = new Map<string, FileDiff[]>();
  private sessionDiffTimers = new Map<string, number>();
  /** Never reset, so an id can never point at a newer diff than the one tapped. */
  /** From 1: a diff id of 0 is a FileDiff that was never stored. */
  private nextDiffId = 1;
  /**
   * ARIA is out of the island — on the desktop, flying, or being dragged
   * there — so the island's own ARIA is hidden (AppState.ariaOnDesktop).
   */
  ariaOnDesktop = false;

  /** Outfit shown on ARIA while the pointer rests on a wardrobe button. */
  wardrobePreview: Outfit | null = null;

  lastActivity = performance.now();

  settings: Settings = { ...DEFAULT_SETTINGS };

  /** Which pills this build offers depends on it (Claude Desktop is Windows only). */
  os: HostOs = HOST_OS;

  private listeners = new Set<Listener>();

  subscribe(fn: Listener): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  /** Marks the UI dirty; the island re-renders on the next frame. */
  notify() {
    for (const fn of this.listeners) fn();
  }

  get focusTask(): AgentTask | null {
    return this.tasks.find((t) => t.id === this.focusId) ?? this.tasks[0] ?? null;
  }

  get effectiveState(): BotStateName {
    return this.stateOverride ?? this.focusTask?.state ?? "idle";
  }

  get otherTasks(): AgentTask[] {
    return this.tasks.filter((t) => t.id !== this.focusId);
  }

  setFocus(id: string) {
    const t = this.tasks.find((x) => x.id === id);
    if (!t) return;
    this.focusId = id;
    this.showingPlanDetail = false;
    t.pillBadge = null;
    this.notify();
  }

  /**
   * A permission card or a question comes up: its pill comes to the front, and
   * the pill that was there is remembered (HookServer.focusBeforeApproval).
   */
  beginApproval(info: ApprovalInfo) {
    this.pendingApproval = info;
    this.isPinned = true;
    if (this.focusBeforeApproval == null) this.focusBeforeApproval = this.focusId;
    this.setFocus(info.pillId);
  }

  /**
   * The card has its answer, or is withdrawn: the session carries on, and the
   * pill you were on comes back — unless you moved to another one meanwhile.
   */
  endApproval() {
    const req = this.pendingApproval;
    if (!req) return;
    this.pendingApproval = null;
    this.isPinned = false;
    this.updateTask(req.pillId, "working");
    this.setPillBadge(req.pillId, null);
    const previous = this.focusBeforeApproval;
    this.focusBeforeApproval = null;
    if (previous && this.focusId === req.pillId && this.tasks.some((t) => t.id === previous)) {
      this.focusId = previous;
    }
    this.notify();
  }

  updateTask(id: string, state: BotStateName) {
    const t = this.tasks.find((x) => x.id === id);
    if (!t) return;
    t.state = state;
    this.notify();
  }

  appendStep(id: string, step: string) {
    const t = this.tasks.find((x) => x.id === id);
    if (!t) return;
    const newest = t.stepSeq ?? t.steps.length - 1;
    t.steps.push(step);
    if (t.steps.length > 20) t.steps.shift();
    t.stepIndex = t.steps.length - 1;
    t.stepSeq = newest + 1;
    this.notify();
  }

  setPillBadge(id: string, badge: PillBadge | null) {
    const t = this.tasks.find((x) => x.id === id);
    if (!t) return;
    t.pillBadge = badge;
    this.notify();
  }

  /** Stores a diff for a pill and returns its id (for the ticker step). */
  appendSessionDiff(pillId: string, diff: FileDiff): number {
    const id = this.nextDiffId++;
    const list = this.sessionDiffs.get(pillId) ?? [];
    list.push({ ...diff, id, at: Date.now() });
    while (list.length > MAX_DIFFS_PER_PILL) list.shift();
    this.sessionDiffs.set(pillId, list);
    // One timer per pill, re-armed on every diff — nothing polls.
    const prev = this.sessionDiffTimers.get(pillId);
    if (prev != null) window.clearTimeout(prev);
    this.sessionDiffTimers.set(
      pillId,
      window.setTimeout(() => this.clearSessionDiffs(pillId), DIFF_TTL_MS),
    );
    return id;
  }

  findDiff(pillId: string, id: number): FileDiff | null {
    return this.sessionDiffs.get(pillId)?.find((d) => d.id === id) ?? null;
  }

  latestDiff(pillId: string): FileDiff | null {
    return this.sessionDiffs.get(pillId)?.at(-1) ?? null;
  }

  clearSessionDiffs(pillId: string) {
    const timer = this.sessionDiffTimers.get(pillId);
    if (timer != null) window.clearTimeout(timer);
    this.sessionDiffTimers.delete(pillId);
    this.sessionDiffs.delete(pillId);
  }

  /** The always-on workspace pill, once the setting has been checked. */
  get mainPillId(): string {
    return sanitizeDeclared(this.settings, this.os).mainPill;
  }

  /**
   * True for a pill that stays when its session ends: the main pill, the
   * declared ones and every Grok Bot go back to idle instead of going away. A
   * coding agent hidden in Ajustes never stays.
   */
  isKept(id: string): boolean {
    if (id.startsWith(BOT_PREFIX)) return this.settings.grokBots.some((b) => botPillId(b) === id);
    if (isHiddenAgent(id, this.settings)) return false;
    const d = sanitizeDeclared(this.settings, this.os);
    return id === d.mainPill || d.activeIntegrations.includes(id);
  }

  /**
   * Loads the catalog pills: the main pill always, the declared ones, and none
   * of the others — a pill that is mid-session stays until its session ends.
   * The Grok Bots are always there. Safe to call any number of times.
   * AppState.loadIntegrationTasks on macOS.
   */
  loadIntegrationTasks() {
    const bots = this.settings.grokBots;
    for (const bot of bots) {
      const id = botPillId(bot);
      const existing = this.tasks.find((t) => t.id === id);
      if (existing) {
        existing.name = bot.name;
        existing.color = botColor(bot);
      } else {
        this.tasks.push({
          id, name: bot.name, color: botColor(bot), state: "idle", stepIndex: 0, steps: [],
          source: "agent", isIntegration: false,
        });
      }
    }
    this.tasks = this.tasks.filter((t) => !t.id.startsWith(BOT_PREFIX) || bots.some((b) => botPillId(b) === t.id));

    const d = sanitizeDeclared(this.settings, this.os);
    this.settings.mainPill = d.mainPill;
    this.settings.activeIntegrations = d.activeIntegrations;
    for (const def of availablePills(this.os)) {
      const hidden = isHiddenAgent(def.id, this.settings);
      const shouldLoad = !hidden && (def.id === d.mainPill || d.activeIntegrations.includes(def.id));
      const idx = this.tasks.findIndex((t) => t.id === def.id);
      if (shouldLoad && idx < 0) this.tasks.push(taskFor(def));
      const busy = idx >= 0 && (this.tasks[idx].state !== "idle" || this.tasks[idx].steps.length > 0);
      if (!shouldLoad && idx >= 0 && (hidden || !busy)) this.tasks.splice(idx, 1);
    }
    this.tasks = orderPills(this.tasks, d.mainPill);
    if (!this.focusId || !this.tasks.some((t) => t.id === this.focusId)) {
      this.focusId = this.tasks.some((t) => t.id === d.mainPill) ? d.mainPill : this.tasks[0]?.id ?? null;
    }
    this.notify();
  }

  /**
   * A session is over. The main and declared pills are put back as they were;
   * any other pill goes away (AppState.removeTask on macOS).
   */
  removeTask(id: string) {
    const idx = this.tasks.findIndex((t) => t.id === id);
    if (idx < 0) return;
    if (this.isKept(id)) {
      const t = this.tasks[idx];
      t.state = "idle";
      t.steps = [];
      t.stepIndex = 0;
      delete t.stepSeq;
      t.pillBadge = null;
      t.finalLine = null;
      const def = pillDefinition(id);
      if (def) t.name = def.name;
      this.clearSessionDiffs(id);
      this.notify();
      return;
    }
    this.tasks.splice(idx, 1);
    this.clearSessionDiffs(id);
    if (this.focusId === id) this.focusId = this.tasks[0]?.id ?? this.mainPillId;
    this.notify();
  }

  /**
   * Creates the pill of a tagged agent on its first event; no-op if it exists.
   * Inserted right after the main pill so it is in the visible slice(0,4). A
   * catalog agent wears its catalog colour, as on macOS.
   */
  upsertExternalAgent(id: string, name: string, color: string) {
    if (this.tasks.some((t) => t.id === id)) return;
    const def = pillDefinition(id);
    this.insertAfterMain({
      id, name, color: def?.color ?? color,
      state: "idle", stepIndex: 0, steps: [],
      source: "agent", isIntegration: false,
    });
  }

  /**
   * The pill a Claude Code session belongs to (VS Code or Cursor). It is made
   * for the session when it is neither the main pill nor declared, as
   * upsertWorkspaceTask does on macOS.
   */
  upsertWorkspacePill(id: string, name: string, cwd: string): AgentTask | null {
    let t = this.tasks.find((x) => x.id === id);
    if (!t) {
      const def = pillDefinition(id);
      if (!def) return null;
      t = taskFor(def, name);
      this.insertAfterMain(t);
    }
    t.name = name;
    if (cwd) t.sessionCwd = cwd;
    return t;
  }

  private insertAfterMain(t: AgentTask) {
    const at = this.tasks.findIndex((x) => x.id === this.mainPillId) + 1;
    this.tasks.splice(at, 0, t);
    if (!this.focusId) this.focusId = t.id;
    this.notify();
  }

  /** Declares or undeclares a pill (max 4 next to the main one). */
  toggleIntegration(id: string) {
    const next = toggleDeclared(sanitizeDeclared(this.settings, this.os), id, this.os);
    if (!next) return;
    this.settings.activeIntegrations = next;
    if (!next.includes(id) && this.focusId === id) this.focusId = this.mainPillId;
    this.loadIntegrationTasks();
  }

  /**
   * What the island opens on. A card waiting for an answer comes first, so
   * reopening a folded island shows it again (Mac #117, #290).
   */
  defaultView(): IslandViewName {
    if (this.pendingApproval) return this.pendingApproval.questions ? "question" : "approval";
    return this.tasks.length === 0 ? "empty" : "overview";
  }
}

export const State = new AppState();
