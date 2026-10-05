// App state — mirror of AppState.swift (the parts the island needs).

import type { BotEmoteName, BotStateName, IslandMode, IslandViewName } from "./layout";
import type { EyeShape } from "../mochi/engine";

export type AgentSource = "claudeCode" | "n8n" | "agent";
export type PillBadge = "approval" | "finished" | "error";

export interface AgentTask {
  id: string;
  name: string;
  color: string;
  state: BotStateName;
  stepIndex: number;
  steps: string[];
  source: AgentSource;
  isIntegration: boolean;
  emote?: BotEmoteName | null;
  miniEye?: EyeShape | null;
  pillBadge?: PillBadge | null;
  sessionCwd?: string | null;
  /** Last thing the agent said (Cursor afterAgentResponse, Claude last_assistant_message). */
  lastMessage?: string | null;
  /** Most recent file edit, for the live diff card. */
  lastDiff?: FileDiff | null;
}

export interface DiffLine {
  kind: "add" | "del" | "ctx";
  text: string;
}

export interface FileDiff {
  file: string;
  added: number;
  removed: number;
  lines: DiffLine[];
}

export interface ApprovalInfo {
  requestId: string;
  sessionId: string;
  tool: string;
  command: string;
  /** The pill the request belongs to (Claude Code, Cursor, Codex, a Grok Bot). */
  agentId: string;
  /** Claude Code when it suggested a rule; Mochi's own tools for the session. */
  allowAlways: boolean;
  /** Full text under review (a skill's code, a diff) — Mochi's requests only. */
  detail?: string | null;
  /** Wrap the detail as prose (a Bot's question, its arguments) instead of code. */
  detailWrap?: boolean;
  /** The raw tool_input, for a Grok Bot's log line (botlog.ts sanitises it). */
  toolInput?: Record<string, unknown> | null;
}

export interface QuestionOption {
  label: string;
  description?: string;
}

export interface QuestionItem {
  question: string;
  header?: string;
  options: QuestionOption[];
  multiSelect: boolean;
}

/** Claude Code's AskUserQuestion, waiting for answers from the island. */
export interface QuestionInfo {
  requestId: string;
  agentId: string;
  questions: QuestionItem[];
}

export interface ChatMessage {
  id: number;
  role: "user" | "assistant";
  content: string;
  /** Tools the assistant ran while writing this reply. */
  steps?: string[];
}

/** Owner of the approval cards raised by Mochi's own tools (agent.rs). */
export const ASSISTANT_ID = "assistant";

/** How Mochi signs its own approval cards. */
export const MOCHI_TASK: AgentTask = {
  id: ASSISTANT_ID, name: "Mochi", color: "#A78BFA", state: "approval", stepIndex: 0, steps: [],
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

const task = (
  id: string, name: string, color: string, source: AgentSource,
): AgentTask => ({
  id, name, color, state: "idle", stepIndex: 0, steps: [], source, isIntegration: true,
});

/** AgentTask.integrationAgents — same ids, names and colours as macOS. */
export const INTEGRATION_AGENTS: AgentTask[] = [
  task("integration_claude", "VS Code", "#F5F6F8", "claudeCode"),
  task("integration_resend", "Resend", "#22C55E", "n8n"),
  task("integration_n8n", "n8n", "#F29B38", "n8n"),
  task("integration_vercel", "Vercel", "#7C5CFF", "n8n"),
  task("integration_github", "GitHub", "#F4505E", "n8n"),
  task("integration_notion", "Notion", "#8C8C8C", "n8n"),
  task("integration_calcom", "Cal.com", "#C9956A", "n8n"),
  task("integration_stripe", "Stripe", "#0570DE", "n8n"),
];

/** Agent pills from PillCatalog.swift — same ids, names and colours as macOS. */
export const AGENT_PILLS: AgentTask[] = [
  { ...task("agent_cursor", "Cursor", "#C0C4CC", "agent"), isIntegration: false },
  { ...task("agent_codex", "Codex", "#2DD4BF", "agent"), isIntegration: false },
  { ...task("agent_gemini", "Gemini CLI", "#8AB4F8", "agent"), isIntegration: false },
  { ...task("agent_antigravity", "Antigravity", "#E879F9", "agent"), isIntegration: false },
];

export const TOGGLEABLE_INTEGRATION_IDS = [
  "integration_resend", "integration_n8n", "integration_vercel", "integration_github",
  "integration_notion", "integration_calcom", "integration_stripe",
];

/** Pills that can be declared in Settings → Active pills, agents included. */
export const DECLARABLE_PILL_IDS = [...AGENT_PILLS.map((t) => t.id), ...TOGGLEABLE_INTEGRATION_IDS];

/** Agent name (coucou_agent) → catalog pill, when there is one. */
export function catalogAgent(name: string): AgentTask | null {
  return AGENT_PILLS.find((t) => t.id === `agent_${name}`) ?? null;
}

/** One of the owner's Grok Bots (grokbot.rs). The webhook key stays in the Credential Manager. */
export interface GrokBot {
  id: string;
  name: string;
  color: string;
  url: string;
}

/** Grok Bot pills: `agent_bot-<id>`, fed by `coucou-hook --bot`. */
export const BOT_PREFIX = "agent_bot-";

export function botPillId(bot: GrokBot): string {
  return `${BOT_PREFIX}${bot.id}`;
}

/** How a Grok Bot pill names its state, in the island's words. */
export function botPhase(state: BotStateName): { label: string; color: string } | null {
  switch (state) {
    case "working":
    case "thinking":
    case "searching":
      return { label: "trabajando", color: "#60A5FA" };
    case "approval":
    case "question":
      return { label: "pregunta", color: "#F5A524" };
    case "finished":
      return { label: "listo", color: "#22C55E" };
    case "error":
      return { label: "error", color: "#F4505E" };
    default:
      return null;
  }
}

export const CURSOR_AGENT_ID = "agent_cursor";

/** Claude Code (VS Code), Cursor IDE, Codex, Gemini CLI and Antigravity. */
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
  activeIntegrations: string[];
  screen: "primary" | "cursor";
  autostart: boolean;
  hooksInstalled: boolean;
  /** Claude model used by the chat. */
  model: string;
  /** The pill the island opens on and lists first. */
  mainPill: string;
  /** Global keyboard shortcuts on/off. */
  shortcutsEnabled: boolean;
  /** AI provider the assistant talks to (providers.rs). */
  provider: string;
  /** Model per non-Anthropic provider; Anthropic keeps `model`. */
  providerModels: Record<string, string>;
  /** Base URL per local provider (Ollama, LM Studio). */
  providerUrls: Record<string, string>;
  /** Let the assistant use tools (files, PowerShell, apps, MCP…). */
  assistantTools: boolean;
  /** Claude Code (VS Code), Codex and Gemini CLI pills and settings. */
  showAgents: boolean;
  /** The Cursor IDE agent: pill, questions and hooks. */
  showCursorAgent: boolean;
  /** The owner's Grok Bots. */
  grokBots: GrokBot[];
}

export const DEFAULT_SETTINGS: Settings = {
  soundEnabled: true,
  soundVolume: 0.12,
  autoCloseInterval: 15,
  absenceInterval: 180,
  activeIntegrations: [
    "integration_resend", "integration_n8n", "integration_vercel", "integration_github",
  ],
  screen: "primary",
  autostart: false,
  hooksInstalled: false,
  model: "claude-opus-5",
  mainPill: "integration_claude",
  shortcutsEnabled: true,
  provider: "cursor",
  providerModels: {},
  providerUrls: {},
  assistantTools: true,
  showAgents: false,
  showCursorAgent: true,
  grokBots: [],
};

type Listener = () => void;

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
  pendingQuestion: QuestionInfo | null = null;
  /** The diff card shows this task's lastDiff. */
  diffTaskId: string | null = null;

  integrations: Record<string, IntegrationInfo> = {};

  lastActivity = performance.now();

  settings: Settings = { ...DEFAULT_SETTINGS };

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
    t.pillBadge = null;
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
    t.steps.push(step);
    if (t.steps.length > 20) t.steps.shift();
    t.stepIndex = t.steps.length - 1;
    this.notify();
  }

  setPillBadge(id: string, badge: PillBadge | null) {
    const t = this.tasks.find((x) => x.id === id);
    if (!t) return;
    t.pillBadge = badge;
    this.notify();
  }

  /** Declared pills stay on screen (idle) when their session ends. */
  isDeclared(id: string): boolean {
    if (id.startsWith(BOT_PREFIX)) return this.settings.grokBots.some((b) => botPillId(b) === id);
    if (isHiddenAgent(id, this.settings)) return false;
    return id === "integration_claude" || id === this.settings.mainPill ||
      this.settings.activeIntegrations.includes(id);
  }

  /** loadIntegrationTasks() — the Grok Bots always, VS Code when agents are shown, the rest opt-in (max 4). */
  loadIntegrationTasks() {
    const bots = this.settings.grokBots;
    for (const bot of bots) {
      const id = botPillId(bot);
      const existing = this.tasks.find((t) => t.id === id);
      if (existing) {
        existing.name = bot.name;
        existing.color = bot.color;
      } else {
        this.tasks.push({
          id, name: bot.name, color: bot.color, state: "idle", stepIndex: 0, steps: [],
          source: "agent", isIntegration: false,
        });
      }
    }
    this.tasks = this.tasks.filter((t) => !t.id.startsWith(BOT_PREFIX) || bots.some((b) => botPillId(b) === t.id));
    for (const proto of [...INTEGRATION_AGENTS, ...AGENT_PILLS]) {
      const shouldLoad = this.isDeclared(proto.id);
      const idx = this.tasks.findIndex((t) => t.id === proto.id);
      if (shouldLoad && idx < 0) this.tasks.push({ ...proto, steps: [] });
      // An agent pill with a live session stays until the session ends.
      const live = idx >= 0 && this.tasks[idx].state !== "idle";
      const keepLive = proto.id.startsWith("agent_") && live && !isHiddenAgent(proto.id, this.settings);
      if (!shouldLoad && idx >= 0 && !keepLive) this.tasks.splice(idx, 1);
    }
    // Order: the main pill first, then integration_claude, then agent_* pills
    // (visible in slice(0,4)), then other integrations in declaration order.
    const main = this.settings.mainPill;
    const order = [...INTEGRATION_AGENTS, ...AGENT_PILLS].map((t) => t.id);
    this.tasks.sort((a, b) => {
      if (a.id === main) return -1;
      if (b.id === main) return 1;
      const isAgentA = a.id.startsWith("agent_");
      const isAgentB = b.id.startsWith("agent_");
      if (a.id === "integration_claude") return -1;
      if (b.id === "integration_claude") return 1;
      if (isAgentA && !isAgentB) return -1;
      if (isAgentB && !isAgentA) return 1;
      if (isAgentA && isAgentB) return 0;
      return order.indexOf(a.id) - order.indexOf(b.id);
    });
    if (!this.focusId || !this.tasks.some((t) => t.id === this.focusId)) {
      this.focusId = this.tasks.some((t) => t.id === main) ? main : this.tasks[0]?.id ?? null;
    }
    this.notify();
  }

  /** Ends an agent session: declared pills go idle, the others leave. */
  endSession(id: string) {
    const t = this.tasks.find((x) => x.id === id);
    if (!t) return;
    if (this.isDeclared(id)) {
      t.state = "idle";
      t.pillBadge = null;
      t.steps = [];
      t.stepIndex = 0;
      this.notify();
    } else {
      this.removeTask(id);
    }
  }

  removeTask(id: string) {
    const idx = this.tasks.findIndex((t) => t.id === id);
    if (idx < 0) return;
    this.tasks.splice(idx, 1);
    if (this.focusId === id) this.focusId = this.tasks[0]?.id ?? "integration_claude";
    this.notify();
  }

  /** Creates a dynamic agent_ pill on first event; no-ops if it already exists.
   *  Inserted right after integration_claude so it appears in the visible slice(0,4). */
  upsertExternalAgent(id: string, name: string, color: string) {
    if (this.tasks.some((t) => t.id === id)) return;
    const at = this.tasks.findIndex((t) => t.id === "integration_claude") + 1;
    this.tasks.splice(at, 0, {
      id, name, color,
      state: "idle", stepIndex: 0, steps: [],
      source: "agent", isIntegration: false,
    });
    if (!this.focusId) this.focusId = id;
    this.notify();
  }

  toggleIntegration(id: string) {
    if (id === "integration_claude") return;
    const active = this.settings.activeIntegrations;
    if (active.includes(id)) {
      this.settings.activeIntegrations = active.filter((x) => x !== id);
      if (this.focusId === id) this.focusId = "integration_claude";
    } else {
      if (active.length >= 4) return;
      this.settings.activeIntegrations = [...active, id];
    }
    this.loadIntegrationTasks();
  }

  defaultView(): IslandViewName {
    return this.tasks.length === 0 ? "empty" : "overview";
  }
}

export const State = new AppState();
