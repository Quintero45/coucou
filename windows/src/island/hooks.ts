// Agent hook events → island state.
// Port of HookServer.processEvent / processPermissionRequest from the macOS app.
// Difference from macOS: no terminal filter. On Windows the hook fires from any
// terminal (Windows Terminal, VS Code, PowerShell…) and all of them are handled.
// Cursor, Codex, Gemini CLI and Antigravity arrive already normalised by the
// relay (windows/hook/src/normalize.rs) and are told apart by `coucou_agent`.

import { Bridge, onEvent } from "../core/bridge";
import { buildFileDiff } from "../core/diff";
import { setApprovalDetail, setQuestionHeight } from "../core/layout";
import { Sound } from "../core/sound";
import { BOT_PREFIX, State, botPillId, catalogAgent, isHiddenAgent, type QuestionItem } from "../core/state";
import type { Island } from "./island";

const CLAUDE_ID = "integration_claude";

/** Agents whose permission requests get an approval card; Grok Bots too. */
const APPROVING_AGENTS = new Set([CLAUDE_ID, "agent_cursor", "agent_codex"]);

function approves(agentId: string): boolean {
  return APPROVING_AGENTS.has(agentId) || agentId.startsWith(BOT_PREFIX);
}

/** Clears the approval / question card if no decision was made before the hook gave up. */
let pendingTimeout: number | null = null;

interface HookPayload {
  hook_event_name?: string;
  request_id?: string;
  session_id?: string;
  cwd?: string;
  message?: string;
  /** UserPromptSubmit carries `prompt`; `message` belongs to Notification/Stop. */
  prompt?: string;
  tool_name?: string;
  tool_input?: Record<string, unknown>;
  /** Optional agent tag: lowercase, digits and hyphens, ≤ 24 chars. */
  coucou_agent?: string;
  /** `ask_user_question` for Claude Code's AskUserQuestion. */
  coucou_kind?: string;
  /** Claude Code's suggested rules for "Always". */
  permission_suggestions?: unknown[];
  /** Claude Code's final answer, on Stop. */
  last_assistant_message?: string;
  /** `coucou-hook --bot`: the Bot's display name and what it reports. */
  coucou_bot?: string;
  bot_status?: "working" | "done" | "needs" | "error";
}

/** Same rule as HookServer.validateAgent on macOS. "claude" is reserved. */
function validateAgent(raw: string | undefined): string | null {
  if (!raw || raw.length > 24 || raw === "claude") return null;
  if (!/^[a-z0-9-]+$/.test(raw)) return null;
  return raw;
}

const FALLBACK_COLORS = ["#22C55E", "#EAB308", "#60A5FA", "#E879F9"];

function agentColor(name: string): string {
  let h = 0;
  for (let i = 0; i < name.length; i++) {
    h = (Math.imul(31, h) + name.charCodeAt(i)) | 0;
  }
  return FALLBACK_COLORS[Math.abs(h) % FALLBACK_COLORS.length];
}

const PROJECT_ALIASES: Record<string, string> = {
  "notch-buddy": "Notch Buddy",
  notchbuddy: "Notch Buddy",
  notch_buddy: "Notch Buddy",
};

function aliasProjectName(name: string): string {
  return PROJECT_ALIASES[name.toLowerCase()] ?? name;
}

function lastPathComponent(p: string): string {
  const cleaned = p.replace(/[\\/]+$/, "");
  const idx = Math.max(cleaned.lastIndexOf("\\"), cleaned.lastIndexOf("/"));
  return idx >= 0 ? cleaned.slice(idx + 1) : cleaned;
}

/** Step labels for each agent's tools. */
const TOOL_LABELS: Record<string, string> = {
  Bash: "Ejecuta",
  Read: "Lee",
  Write: "Escribe",
  Edit: "Modifica",
  Glob: "Busca",
  Grep: "Busca en",
  WebSearch: "Búsqueda web",
  WebFetch: "Descarga",
  TodoWrite: "Tareas",
  Task: "Agente",
  LS: "Lista",
  MultiEdit: "Modifica",
  NotebookEdit: "Notebook",
  PowerShell: "Ejecuta",
  // Cursor
  Shell: "Ejecuta",
  StrReplace: "Modifica",
  Delete: "Elimina",
  SemanticSearch: "Busca",
  ReadLints: "Lints",
  // Codex
  apply_patch: "Modifica",
  update_plan: "Plan",
  spawn_agent: "Agente",
};

function stepLabel(tool: string, input: Record<string, unknown>): string {
  const label = tool.startsWith("MCP: ") ? `MCP · ${tool.slice(5)}` : TOOL_LABELS[tool] ?? tool;
  const str = (k: string) => (typeof input[k] === "string" ? (input[k] as string) : null);
  const cmd = str("command");
  if (cmd) return `${label} · ${cmd.slice(0, 40)}`;
  const path = str("path");
  if (path) return `${label} · ${lastPathComponent(path)}`;
  const file = str("file_path");
  if (file) return `${label} · ${lastPathComponent(file)}`;
  const query = str("query");
  if (query) return `${label} · ${query.slice(0, 40)}`;
  // Codex apply_patch: "*** Update File: path"
  const patch = str("input") ?? str("patch");
  const m = patch?.match(/\*\*\* (?:Update|Add|Delete) File: (.+)/);
  if (m) return `${label} · ${lastPathComponent(m[1].trim())}`;
  return label;
}

/**
 * What the Allow button actually authorises. Approving "Write" tells you nothing
 * — approving `Write · C:\…\.env` tells you everything, and the difference is
 * the whole point of approving from the island rather than blind.
 *
 * Ordered by how specific the field is, so an unfamiliar tool still shows
 * whatever identifying string it carries instead of falling back to its name.
 */
const APPROVAL_FIELDS = [
  "command", // Bash, PowerShell, Cursor Shell
  "file_path", // Write, Edit, MultiEdit, NotebookEdit
  "path", // Read, LS
  "url", // WebFetch
  "query", // WebSearch
  "pattern", // Glob, Grep
  "prompt", // Task
] as const;

function approvalTarget(tool: string, input: Record<string, unknown>): string {
  for (const field of APPROVAL_FIELDS) {
    const value = input[field];
    if (typeof value === "string" && value.trim()) {
      return `${tool} · ${value.trim()}`;
    }
  }
  // MCP calls: show the arguments, they are what is being authorised.
  if (tool.startsWith("MCP: ")) {
    const args = JSON.stringify(input);
    if (args && args !== "{}") return `${tool} · ${args.slice(0, 160)}`;
  }
  return tool;
}

/** AskUserQuestion's tool_input.questions, validated. At most four, as on macOS. */
function parseQuestions(input: Record<string, unknown>): QuestionItem[] {
  const raw = Array.isArray(input.questions) ? (input.questions as Record<string, unknown>[]) : [];
  const out: QuestionItem[] = [];
  for (const q of raw.slice(0, 4)) {
    const question = typeof q.question === "string" ? q.question : "";
    const options = Array.isArray(q.options)
      ? (q.options as Record<string, unknown>[])
          .map((o) => ({
            label: typeof o.label === "string" ? o.label : "",
            description: typeof o.description === "string" ? o.description : undefined,
          }))
          .filter((o) => o.label)
      : [];
    if (!question || options.length === 0) continue;
    out.push({
      question,
      header: typeof q.header === "string" ? q.header : undefined,
      options,
      multiSelect: q.multiSelect === true,
    });
  }
  return out;
}

function upsert(projectName: string, cwd: string) {
  const t = State.tasks.find((x) => x.id === CLAUDE_ID);
  if (!t) return;
  t.name = projectName;
  if (cwd) t.sessionCwd = cwd;
}

function clearSession() {
  const t = State.tasks.find((x) => x.id === CLAUDE_ID);
  if (!t) return;
  t.steps = [];
  t.stepIndex = 0;
  t.name = "VS Code";
  t.pillBadge = null;
  t.lastMessage = null;
}

export function registerHookHandlers(island: Island) {
  void onEvent<HookPayload>("hook", (payload) => handleHook(island, payload));
}

/** The card stopped waiting: put the pill and the view back. */
function releaseCard(island: Island, agentId: string) {
  State.pendingApproval = null;
  State.pendingQuestion = null;
  State.isPinned = false;
  island.dropPin();
  State.updateTask(agentId, "working");
  State.setPillBadge(agentId, null);
  if (State.view === "approval" || State.view === "question") island.setView(State.defaultView());
  State.notify();
}

function handleHook(island: Island, payload: HookPayload) {
  if (State.paused) {
    // Silence here used to cost Claude Code nearly two minutes: the relay waited
    // for a decision from an island that had already decided not to look. Say so,
    // and the terminal takes the question immediately.
    if (payload.request_id) void Bridge.approvalDecline(payload.request_id);
    return;
  }

  const name = payload.hook_event_name ?? "";
  const cwd = payload.cwd ?? "";
  const raw = lastPathComponent(cwd);
  const projectName = aliasProjectName(raw || "Sesión");

  // Route to the right pill. Valid coucou_agent → "agent_<name>" pill, with the
  // catalog's name and colour when it is a known agent (Cursor, Codex…), or the
  // owner's Grok Bot for "bot-<id>". "claude" is reserved; absent or invalid →
  // Claude Code pill unchanged.
  const validAgent = validateAgent(payload.coucou_agent);
  const agentId = validAgent ? `agent_${validAgent}` : CLAUDE_ID;
  const isExternalAgent = validAgent !== null;

  // Coding agents are hidden unless the owner turned them back on: hand any
  // question straight back so the agent asks in its own UI.
  if (isHiddenAgent(agentId, State.settings)) {
    if (payload.request_id) void Bridge.approvalDecline(payload.request_id);
    return;
  }

  const focused = State.focusId === agentId;

  /** Alerts force the island open; work events only reveal the compact island. */
  const surface = (view: Parameters<Island["alert"]>[0], isAlert: boolean) => {
    if (State.mode === "expanded") {
      if (isAlert) island.setView(view);
    } else if (isAlert) {
      island.alert(view);
    } else if (State.mode === "hidden") {
      island.reveal();
    }
  };

  /** Ensure the agent pill exists (no-op for Claude Code). */
  const ensurePill = () => {
    if (isExternalAgent) {
      const bot = State.settings.grokBots.find((b) => botPillId(b) === agentId);
      const known = catalogAgent(validAgent!);
      const label = bot?.name ?? known?.name ?? (payload.coucou_bot?.trim().slice(0, 40) || validAgent!);
      State.upsertExternalAgent(agentId, label, bot?.color ?? known?.color ?? agentColor(validAgent!));
      const t = State.tasks.find((x) => x.id === agentId);
      if (t && cwd) t.sessionCwd = cwd;
    } else {
      upsert(projectName, cwd);
    }
  };

  const task = () => State.tasks.find((x) => x.id === agentId);

  // AskUserQuestion from Claude Code: an interactive card, answered from here.
  if (payload.coucou_kind === "ask_user_question") {
    const requestId = payload.request_id ?? "";
    const questions = parseQuestions(payload.tool_input ?? {});
    if (!requestId || questions.length === 0 || State.pendingApproval || State.pendingQuestion) {
      if (requestId) void Bridge.approvalDecline(requestId);
      return;
    }
    ensurePill();
    if (pendingTimeout != null) window.clearTimeout(pendingTimeout);
    // Sized before the alert so the island opens straight to the right height.
    setQuestionHeight(118 + questions[0].options.length * 30 + 44);
    State.pendingQuestion = { requestId, agentId, questions };
    void Bridge.approvalAck(requestId);
    State.updateTask(agentId, "question");
    State.isPinned = true;
    Sound.play("approval");
    State.setFocus(agentId);
    island.alert("question");
    pendingTimeout = window.setTimeout(() => {
      pendingTimeout = null;
      if (State.pendingQuestion?.requestId === requestId) releaseCard(island, agentId);
    }, 125_000);
    State.notify();
    return;
  }

  switch (name) {
    case "SessionStart":
      ensurePill();
      surface("overview", false);
      Sound.play("work");
      break;

    case "UserPromptSubmit": {
      ensurePill();
      State.updateTask(agentId, "thinking");
      // The field is `prompt`; reading `message` meant this step was always blank.
      const asked = payload.prompt ?? payload.message;
      if (asked) State.appendStep(agentId, asked.slice(0, 60));
      surface("overview", false);
      break;
    }

    case "PreToolUse": {
      ensurePill();
      State.updateTask(agentId, "working");
      const tool = payload.tool_name ?? "Tool";
      if (tool === "AskUserQuestion") break; // the --ask hook owns this one
      State.appendStep(agentId, stepLabel(tool, payload.tool_input ?? {}));
      surface("overview", false);
      break;
    }

    case "PostToolUse": {
      ensurePill();
      State.updateTask(agentId, "working");
      const tool = payload.tool_name ?? "";
      const diff = buildFileDiff(tool, payload.tool_input ?? {});
      const t = task();
      if (diff && t) {
        t.lastDiff = diff;
        State.appendStep(agentId, `Modifica · ${diff.file} +${diff.added} −${diff.removed}`);
      }
      break;
    }

    case "PostToolUseFailure":
      State.updateTask(agentId, "working");
      State.appendStep(agentId, "⚠ falló");
      break;

    case "AgentResponse": {
      ensurePill();
      const t = task();
      if (t && payload.message) t.lastMessage = payload.message;
      break;
    }

    case "Notification": {
      const message = payload.message ?? "";
      const lower = message.toLowerCase();
      if (lower.includes("rate limit") || lower.includes("limite d")) {
        State.updateTask(agentId, "ratelimit");
        Sound.play("rate");
      } else if (message.endsWith("?")) {
        State.updateTask(agentId, "question");
        State.appendStep(agentId, message);
      }
      break;
    }

    case "Stop": {
      ensurePill();
      State.updateTask(agentId, "finished");
      const final = payload.last_assistant_message ?? payload.message ?? task()?.lastMessage ?? "";
      if (final) State.appendStep(agentId, final.replace(/\s+/g, " ").slice(0, 80));
      Sound.play("finish");
      if (focused) surface("finished", true);
      else State.setPillBadge(agentId, "finished");
      window.setTimeout(() => {
        const t = task();
        if (!t || t.state !== "finished") return;
        if (isExternalAgent) {
          State.endSession(agentId);
        } else {
          State.updateTask(agentId, "idle");
          State.setPillBadge(agentId, null);
        }
      }, 5200);
      break;
    }

    case "StopFailure":
      ensurePill();
      State.updateTask(agentId, "error");
      Sound.play("error");
      if (focused) surface("error", true);
      else State.setPillBadge(agentId, "error");
      break;

    case "Interrupt":
      State.updateTask(agentId, "idle");
      break;

    case "SessionEnd":
      if (isExternalAgent) {
        State.endSession(agentId);
      } else {
        State.updateTask(agentId, "idle");
        clearSession();
      }
      break;

    case "SubagentStart":
      State.appendStep(agentId, "+ subagente");
      break;

    case "SubagentStop":
      State.appendStep(agentId, "• subagente listo");
      break;

    // A Grok Bot reporting through `coucou-hook --bot`.
    case "BotUpdate": {
      if (!agentId.startsWith(BOT_PREFIX)) break;
      ensurePill();
      const message = (payload.message ?? "").trim();
      const t = task();
      switch (payload.bot_status) {
        case "done":
          State.updateTask(agentId, "finished");
          if (t && message) t.lastMessage = message;
          if (message) State.appendStep(agentId, message.replace(/\s+/g, " ").slice(0, 80));
          Sound.play("finish");
          if (focused) surface("finished", true);
          else {
            State.setPillBadge(agentId, "finished");
            island.reveal();
          }
          window.setTimeout(() => {
            if (task()?.state === "finished") State.endSession(agentId);
          }, 8000);
          break;
        case "needs":
          State.updateTask(agentId, "question");
          if (t && message) t.lastMessage = message;
          if (message) State.appendStep(agentId, message.slice(0, 80));
          Sound.play("approval");
          State.setPillBadge(agentId, "approval");
          State.setFocus(agentId);
          surface("overview", true);
          break;
        case "error":
          State.updateTask(agentId, "error");
          if (message) State.appendStep(agentId, `⚠ ${message.slice(0, 78)}`);
          Sound.play("error");
          if (focused) surface("error", true);
          else State.setPillBadge(agentId, "error");
          break;
        default:
          State.updateTask(agentId, "working");
          if (message) State.appendStep(agentId, message.slice(0, 60));
          surface("overview", false);
          break;
      }
      break;
    }

    case "PermissionRequest": {
      const requestId = payload.request_id ?? "";
      // Only Claude Code, Cursor, Codex and Grok Bots get a card. Anything else
      // is handed straight back so the agent asks in its own UI.
      if (!approves(agentId)) {
        if (requestId) void Bridge.approvalDecline(requestId);
        break;
      }
      // One card, one request. A second one must never quietly replace the first
      // — that would leave a human staring at request B while request A waits for
      // a decision nobody can give. Hand it straight back to the terminal.
      if (State.pendingQuestion || (State.pendingApproval && State.pendingApproval.requestId !== requestId)) {
        if (requestId) void Bridge.approvalDecline(requestId);
        break;
      }
      ensurePill();
      if (pendingTimeout != null) window.clearTimeout(pendingTimeout);
      setApprovalDetail(false);
      const tool = payload.tool_name ?? "Tool";
      const input = payload.tool_input ?? {};
      const suggestions = Array.isArray(payload.permission_suggestions) ? payload.permission_suggestions : [];
      State.pendingApproval = {
        requestId,
        sessionId: payload.session_id ?? "",
        tool,
        command: approvalTarget(tool, input),
        agentId,
        allowAlways: agentId === CLAUDE_ID && suggestions.length > 0,
      };
      // The relay's short ack window closes in 800 ms; everything below this
      // line is synchronous, so the card really is up by the time it lands.
      if (requestId) void Bridge.approvalAck(requestId);
      State.updateTask(agentId, "approval");
      State.isPinned = true;
      Sound.play("approval");
      if (focused) {
        island.alert("approval");
      } else {
        // Another agent holds the view, so the card would yank it away. The badge
        // is the signal instead — but it has to be on screen for that to mean
        // anything, hence the reveal. We just told the relay a human can act.
        State.setPillBadge(agentId, "approval");
        island.reveal();
      }
      // Coucou answers within 108 s or not at all; after that the terminal has
      // taken over and the card would be lying.
      pendingTimeout = window.setTimeout(() => {
        pendingTimeout = null;
        if (State.pendingApproval?.requestId !== requestId) return;
        releaseCard(island, agentId);
      }, 110_000);
      break;
    }

    default:
      break;
  }
  State.notify();
}
