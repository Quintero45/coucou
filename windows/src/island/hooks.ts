// Hook events from Claude Code and every other agent → island state.
// Port of HookServer.processEvent / processPermissionRequest from the macOS app.
// Difference from macOS: no terminal filter. On Windows the hook fires from any
// terminal (Windows Terminal, VS Code, PowerShell…) and all of them are handled.
// The relay has already mapped every agent's events and fields onto Claude
// Code's (hook/src/normalize.rs, hook/src/cursor.rs), so one handler serves
// them all; Grok Bots report through `coucou-hook --bot` (hook/src/bot.rs).

import { Bridge, onEvent } from "../core/bridge";
import { BotChat, CURSOR_CHAT } from "../core/botchat";
import { CURSOR_WRITE, parseChoices, playSound } from "../core/botcmds";
import { recordBotApproval, type BotDecision } from "../core/botlog";
import { buildFileDiff, fileName, makeDiffStep, toOneLine, type FileDiff } from "../core/diff";
import { setApprovalDetail } from "../core/layout";
import { Sound } from "../core/sound";
import { BOT_PREFIX, CURSOR_AGENT_ID, State, botPillId, isHiddenAgent, type AskedQuestion } from "../core/state";
import { pillDefinition } from "../core/pills";
import { botDetail } from "../views/views";
import { botReplyBusy } from "../views/integrations";
import { APPROVAL_AGENTS, agentColor, agentName, validateAgent } from "./agents";
import type { Island } from "./island";
import { parseClaudePlan, restorePlanUsage } from "../core/plan";
import { setClaudePlanUsage, storedClaudePlanUsage } from "../views/usage";
import { N_, t } from "../i18n/i18n";

const CLAUDE_ID = "integration_claude";
const CURSOR_ID = CURSOR_AGENT_ID;

/**
 * Whether a tagged agent's permission requests get a card. Besides the agents
 * the relay answers for (APPROVAL_AGENTS), Cursor (hook/src/cursor.rs) and the
 * Grok Bots (hook/src/bot.rs) wait for the island's decision too.
 */
function approves(validAgent: string): boolean {
  return APPROVAL_AGENTS.has(validAgent) || validAgent === "cursor" || validAgent.startsWith("bot-");
}

/** Clears the approval card if no decision was made before the hook gave up. */
let pendingTimeout: number | null = null;

/** Takes the approval or question card down and gives the island back. */
function dropPendingCard(island: Island): void {
  if (!State.pendingApproval) return;
  State.endApproval();
  island.dropPin();
  if (State.view === "approval" || State.view === "question") {
    island.setView(State.defaultView());
  }
  State.notify();
}

/** The return to idle that Stop arms, per pill, so the next turn can cancel it. */
const stopTimers = new Map<string, number>();

function cancelStopTimer(id: string): boolean {
  const timer = stopTimers.get(id);
  if (timer == null) return false;
  window.clearTimeout(timer);
  stopTimers.delete(id);
  return true;
}

/** Events after which a pending permission request of the same session is moot. */
const TURN_OVER = new Set(["Stop", "StopFailure", "UserPromptSubmit", "SessionEnd", "Interrupt"]);

interface HookPayload {
  hook_event_name?: string;
  request_id?: string;
  session_id?: string;
  cwd?: string;
  message?: string;
  /** UserPromptSubmit carries `prompt`; `message` belongs to Notification/Stop. */
  prompt?: string;
  /** Stop: the turn's final answer (Markdown) — Claude Code's, Hermes's, Codex's. */
  last_assistant_message?: string;
  tool_name?: string;
  tool_input?: Record<string, unknown>;
  /** Set by the relay when an edit was too big to forward whole (> 256 KB). */
  coucou_diff_truncated?: boolean;
  /** Optional agent tag: lowercase, digits and hyphens, ≤ 24 chars. */
  coucou_agent?: string;
  /** Claude Code's suggested rules for "Always". */
  permission_suggestions?: unknown[];
  /** `coucou-hook --bot`: the Bot's display name and what it reports. */
  coucou_bot?: string;
  bot_status?: "working" | "done" | "needs" | "error";
  /** Hermes: where the session runs (telegram, discord…; "cli" in a terminal). */
  platform?: string;
  /** "cursor" when Claude Code runs in Cursor's terminal (set by the relay). */
  term_editor?: string;
  /** StatusLine (the plan usage relay): Claude Code's 5-hour and weekly limits. */
  rate_limits?: unknown;
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

/**
 * localizedStep() — same labels as the macOS app, in English and shown in the
 * interface language (src/i18n) when the step is recorded.
 */
const TOOL_LABELS: Record<string, string> = {
  Bash: N_("Runs"),
  Read: N_("Reads"),
  Write: N_("Writes"),
  Edit: N_("Edits"),
  Glob: N_("Searches"),
  Grep: N_("Searches"),
  WebSearch: N_("Searches the web"),
  WebFetch: N_("Fetches"),
  TodoWrite: N_("Tasks"),
  Task: N_("Agent"),
  LS: N_("Lists"),
  MultiEdit: N_("Edits"),
  NotebookEdit: N_("Notebook"),
  PowerShell: N_("Runs"),
  // Antigravity's tools (#298).
  run_command: N_("Runs"),
  view_file: N_("Reads"),
  write_to_file: N_("Writes"),
  replace_file_content: N_("Edits"),
  read_url_content: N_("Fetches"),
  search_web: N_("Searches the web"),
  // Cursor's.
  Shell: N_("Runs"),
  StrReplace: N_("Edits"),
  Delete: N_("Deletes"),
  SemanticSearch: N_("Searches"),
  ReadLints: N_("Lints"),
  // Codex's.
  apply_patch: N_("Edits"),
  update_plan: N_("Plan"),
  spawn_agent: N_("Agent"),
};

function stepLabel(tool: string, input: Record<string, unknown>): string {
  const label = tool.startsWith("MCP: ") ? `MCP · ${tool.slice(5)}` : TOOL_LABELS[tool] ? t(TOOL_LABELS[tool]) : tool;
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

/** `coucou-hook --bot … --status ask` arrives as this tool, its question in `command`. */
const BOT_QUESTION_TOOL = "Pregunta";

/** Longest string kept per argument, and for the whole block, on a Bot's card. */
const ARG_MAX = 400;
const ARGS_MAX = 2400;

/** A Bot's tool arguments, pretty-printed and cut to something a card can show. */
function botArgs(input: Record<string, unknown>): string | null {
  if (Object.keys(input).length === 0) return null;
  const text = JSON.stringify(
    input,
    (_k, v: unknown) =>
      typeof v === "string" && v.length > ARG_MAX ? `${v.slice(0, ARG_MAX)}… (+${v.length - ARG_MAX})` : v,
    2,
  );
  if (!text) return null;
  return text.length > ARGS_MAX ? `${text.slice(0, ARGS_MAX)}\n…` : text;
}

/** The Bot's display name: the one in Settings, else what the relay sent. */
function botName(agentId: string, fallback?: string): string {
  return (
    State.settings.grokBots.find((b) => botPillId(b) === agentId)?.name ??
    State.tasks.find((t) => t.id === agentId)?.name ??
    (fallback?.trim().slice(0, 40) || agentId.slice(BOT_PREFIX.length))
  );
}

/** A Bot's request that never reached a card still belongs in its history. */
function logBotDecline(payload: HookPayload, decision: BotDecision) {
  const agent = validateAgent(payload.coucou_agent);
  if (payload.hook_event_name !== "PermissionRequest" || !agent?.startsWith("bot-")) return;
  const agentId = `agent_${agent}`;
  const tool = payload.tool_name ?? "Tool";
  recordBotApproval({
    bot: agent.slice(4),
    name: botName(agentId, payload.coucou_bot),
    tool,
    decision,
    target: approvalTarget(tool, payload.tool_input ?? {}),
    input: payload.tool_input ?? null,
  });
}

/** A long text's first non-empty line, for a card's one-line target. */
function firstLine(text: string, max = 90): string {
  const line = text.split(/\r?\n/).map((l) => l.trim()).find(Boolean) ?? "";
  return line.length > max ? `${line.slice(0, max - 1)}…` : line;
}

/**
 * The questions of an AskUserQuestion call, if the island can show all of them
 * as options to pick from. Anything it cannot is left to the terminal.
 */
function askedQuestions(tool: string, input: Record<string, unknown>): AskedQuestion[] | null {
  if (tool !== "AskUserQuestion" || !Array.isArray(input.questions)) return null;
  const out: AskedQuestion[] = [];
  for (const raw of input.questions as Record<string, unknown>[]) {
    const question = typeof raw?.question === "string" ? raw.question : "";
    const options = (Array.isArray(raw?.options) ? (raw.options as Record<string, unknown>[]) : [])
      .filter((o) => typeof o?.label === "string" && o.label)
      .map((o) => ({
        label: o.label as string,
        description: typeof o.description === "string" ? o.description : "",
      }));
    // A question cut short by the relay would be answered under the wrong text.
    if (!question || question.endsWith("…") || options.length < 2) return null;
    out.push({ question, options, multiSelect: raw.multiSelect === true });
  }
  return out.length > 0 ? out : null;
}

/** The Claude Code session's pill, named after its project for the session. */
function upsert(id: string, projectName: string, cwd: string, sessionId: string) {
  const t = State.upsertWorkspacePill(id, projectName, cwd);
  if (t && sessionId) t.sessionId = sessionId;
}

/** The session is over: the pill goes back as it was, or away if it was only there for it. */
function clearSession(id: string) {
  const t = State.tasks.find((x) => x.id === id);
  if (!t) return;
  if (!State.isKept(id)) {
    State.removeTask(id);
    return;
  }
  t.state = "idle";
  t.steps = [];
  t.stepIndex = 0;
  delete t.stepSeq;
  t.name = pillDefinition(id)?.name ?? t.name;
  t.pillBadge = null;
  t.sessionId = null;
  t.finalLine = null;
  t.lastMessage = null;
}

/** The final message stays on the card until the next turn starts. */
function clearFinalLine(id: string) {
  const t = State.tasks.find((x) => x.id === id);
  if (t) t.finalLine = null;
}

/**
 * PostToolUse of Edit / MultiEdit / Write → a diff stored for the pill and a
 * ticker step with its +N −M counts. Nothing is kept for a pill that does not
 * exist, so a stray event cannot grow memory. An edit the relay had to cut
 * would give wrong counts: the PreToolUse step ("Edits · file") stands alone.
 * Returns the diff, for the conversations (Cursor, Grok Bots).
 */
function recordDiff(agentId: string, payload: HookPayload): FileDiff | null {
  if (payload.coucou_diff_truncated) return null;
  if (!State.tasks.some((t) => t.id === agentId)) return null;
  const diff = buildFileDiff(payload.tool_name ?? "", payload.tool_input ?? {});
  if (!diff) return null;
  const id = State.appendSessionDiff(agentId, diff);
  State.appendStep(agentId, makeDiffStep(fileName(diff.path), diff.added, diff.removed, id));
  return diff;
}

export function registerHookHandlers(island: Island) {
  // The last plan numbers seen survive a restart, as on the Mac.
  State.planUsage ??= restorePlanUsage(storedClaudePlanUsage());
  void onEvent<HookPayload>("hook", (payload) => handleHook(island, payload));
  // Cursor was opened (Rust's appwatch): it fires no hook of its own until the
  // first agent chat, so this stands in for its SessionStart.
  void onEvent<{ agent?: string }>("agent-app-opened", (p) => {
    if (p?.agent) handleHook(island, { hook_event_name: "SessionStart", coucou_agent: p.agent });
  });
}

function handleHook(island: Island, payload: HookPayload) {
  // Account-wide numbers from the status line relay, not part of any session:
  // keep the latest, nothing else (no reveal, no sound), paused or not.
  if (payload.hook_event_name === "StatusLine") {
    const usage = parseClaudePlan(payload.rate_limits);
    if (usage) setClaudePlanUsage(usage);
    return;
  }

  if (State.paused) {
    // Silence here used to cost Claude Code nearly two minutes: the relay waited
    // for a decision from an island that had already decided not to look. Say so,
    // and the terminal takes the question immediately.
    if (payload.request_id) void Bridge.approvalDecline(payload.request_id);
    logBotDecline(payload, "paused");
    return;
  }

  const name = payload.hook_event_name ?? "";
  const cwd = payload.cwd ?? "";
  const raw = lastPathComponent(cwd);
  const projectName = aliasProjectName(raw || t("Session"));

  // Route to the right pill. Valid coucou_agent → "agent_<name>" pill, with the
  // catalog's name and colour when it is a known agent (Cursor, Codex…), or the
  // owner's Grok Bot for "bot-<id>". "claude" is reserved; absent or invalid →
  // Claude Code's own pill: Cursor's when it runs in Cursor's terminal (Mac
  // #120), VS Code's otherwise.
  const validAgent = validateAgent(payload.coucou_agent);
  const workspaceId = payload.term_editor === "cursor" ? CURSOR_ID : CLAUDE_ID;
  const agentId = validAgent ? `agent_${validAgent}` : workspaceId;
  const isExternalAgent = validAgent !== null;
  const sessionId = payload.session_id ?? "";

  // Coding agents are hidden unless the owner turned them back on: hand any
  // question straight back so the agent asks in its own UI.
  if (isHiddenAgent(agentId, State.settings)) {
    if (payload.request_id) void Bridge.approvalDecline(payload.request_id);
    return;
  }

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
      const sent = isBotPill ? payload.coucou_bot?.trim().slice(0, 40) : "";
      const label = bot?.name ?? (sent || agentName(validAgent!));
      State.upsertExternalAgent(agentId, label, bot?.color ?? agentColor(validAgent!));
      const t = State.tasks.find((x) => x.id === agentId);
      if (t && cwd) t.sessionCwd = cwd;
      if (t && sessionId) t.sessionId = sessionId;
    } else {
      upsert(agentId, projectName, cwd, sessionId);
    }
  };

  const task = () => State.tasks.find((x) => x.id === agentId);

  /**
   * Called where a handler is about to replace `finished` with a newer state:
   * the timer Stop armed would otherwise put the pill back to idle over it. The
   * badge that timer was going to clear goes now.
   */
  const supersedeStop = () => {
    if (cancelStopTimer(agentId)) State.setPillBadge(agentId, null);
  };

  // The Cursor agent's conversation in the island (its pill opens it like a Bot's).
  const isCursor = validAgent === "cursor";
  const cursorSaid = (text: string, status: "done" | "error" = "done") => {
    const clean = text.trim().slice(0, 4000);
    const last = [...BotChat.list(CURSOR_CHAT)].reverse().find((e) => e.kind === "bot");
    if (clean && !(last?.kind === "bot" && last.text === clean)) BotChat.add(CURSOR_CHAT, { kind: "bot", text: clean, status });
  };

  // A Grok Bot whose conversation is open on screen: its news goes there
  // instead of switching the island to another card.
  const isBotPill = agentId.startsWith(BOT_PREFIX);
  const botSlug = agentId.slice(BOT_PREFIX.length);
  const inChat = isBotPill && botDetail.id === agentId && State.view === "overview" && State.mode === "expanded";
  const cursorInChat = isCursor && botDetail.id === agentId && State.view === "overview" && State.mode === "expanded";

  // The turn that asked for a permission is over — answered in the terminal,
  // interrupted, or a new prompt — so the card would be lying. It goes, and the
  // relay is released without a decision. Same rule as the Mac.
  const pending = State.pendingApproval;
  if (
    pending &&
    TURN_OVER.has(name) &&
    pending.pillId === agentId &&
    pending.sessionId === (payload.session_id ?? "")
  ) {
    if (pendingTimeout != null) window.clearTimeout(pendingTimeout);
    pendingTimeout = null;
    if (pending.requestId) void Bridge.approvalDecline(pending.requestId);
    dropPendingCard(island);
  }

  // Read after the card above is dropped: its pill may have handed the front
  // back to the pill you were on.
  const focused = State.focusId === agentId;

  /** Only the focused agent's finish, error or question opens its card. A
   *  coding agent that starts working takes the focus, unless the one holding
   *  it is busy itself, so Cursor or Codex open their card like Claude Code. */
  const claimFocus = () => {
    if (focused || !isExternalAgent || isBotPill) return;
    const current = State.tasks.find((t) => t.id === State.focusId);
    const busy = !!current && ["working", "thinking", "searching", "approval", "question"].includes(current.state);
    if (!busy) State.setFocus(agentId);
  };

  switch (name) {
    case "SessionStart":
      ensurePill();
      claimFocus();
      clearFinalLine(agentId);
      // Hermes through its gateway says where the session comes from.
      if (validAgent === "hermes" && payload.platform && payload.platform !== "cli") {
        State.appendStep(agentId, payload.platform.charAt(0).toUpperCase() + payload.platform.slice(1));
      }
      surface("overview", false);
      Sound.play("work");
      break;

    case "UserPromptSubmit": {
      ensurePill();
      claimFocus();
      supersedeStop();
      clearFinalLine(agentId);
      State.updateTask(agentId, "thinking");
      // The field is `prompt`; reading `message` meant this step was always blank.
      const asked = payload.prompt ?? payload.message;
      if (asked) State.appendStep(agentId, asked.slice(0, 60));
      if (asked && isCursor) {
        // An order sent from the island comes back here when Cursor takes it.
        // Only an island order not matched yet: a prompt typed in Cursor (even
        // the same text again) must still show up as its own message.
        const mine = [...BotChat.list(CURSOR_CHAT)].reverse().find((e) => e.kind === "me");
        const fromIsland = mine?.kind === "me" && mine.origin === "island" && mine.status !== "error" &&
          Date.now() - mine.at < 15 * 60_000 && asked.trim().startsWith(mine.text.trim());
        if (fromIsland) BotChat.update(CURSOR_CHAT, mine.id, { status: "sent", note: "", origin: "seen" });
        else BotChat.add(CURSOR_CHAT, { kind: "me", text: asked.slice(0, 4000), status: "sent" });
      }
      surface("overview", false);
      break;
    }

    case "PreToolUse": {
      ensurePill();
      claimFocus();
      supersedeStop();
      clearFinalLine(agentId);
      State.updateTask(agentId, "working");
      const tool = payload.tool_name ?? "Tool";
      if (tool === "AskUserQuestion") break; // the --ask hook owns this one
      const step = stepLabel(tool, payload.tool_input ?? {});
      State.appendStep(agentId, step);
      if (isBotPill) BotChat.add(botSlug, { kind: "step", text: step });
      if (isCursor) BotChat.addStep(CURSOR_CHAT, step);
      surface("overview", false);
      break;
    }

    case "PostToolUse": {
      // Cursor's afterFileEdit comes with no PreToolUse before it.
      if (isCursor) ensurePill();
      supersedeStop();
      // The question was answered in the terminal: the card would be lying.
      if (
        payload.tool_name === "AskUserQuestion" &&
        State.pendingApproval?.questions &&
        State.pendingApproval.sessionId === (payload.session_id ?? "")
      ) {
        if (pendingTimeout != null) window.clearTimeout(pendingTimeout);
        pendingTimeout = null;
        dropPendingCard(island);
      }
      State.updateTask(agentId, "working");
      const edited = recordDiff(agentId, payload);
      if (edited && isCursor) BotChat.addEdit(CURSOR_CHAT, edited);
      else if (edited && isBotPill) BotChat.addEdit(botSlug, edited);
      break;
    }

    case "PostToolUseFailure":
      supersedeStop();
      State.updateTask(agentId, "working");
      State.appendStep(agentId, t("⚠ failed"));
      break;

    case "AgentResponse": {
      ensurePill();
      const t = task();
      if (t && payload.message) t.lastMessage = payload.message;
      if (isCursor && payload.message) cursorSaid(payload.message);
      break;
    }

    case "Notification": {
      const message = payload.message ?? "";
      const lower = message.toLowerCase();
      if (lower.includes("rate limit") || lower.includes("limite d")) {
        supersedeStop();
        State.updateTask(agentId, "ratelimit");
        Sound.play("rate");
      } else if (message.endsWith("?")) {
        supersedeStop();
        State.updateTask(agentId, "question");
        State.appendStep(agentId, message);
      }
      break;
    }

    case "Stop": {
      ensurePill();
      State.updateTask(agentId, "finished");
      // Claude Code puts the turn's answer in the Stop payload itself, so there is
      // no transcript to read (the relay does not even forward its path). Other
      // agents report their last words the same way (Hermes, Codex) or as
      // `message`; Cursor said it earlier, in afterAgentResponse.
      const final = payload.last_assistant_message ?? payload.message ?? task()?.lastMessage ?? "";
      const finalText = toOneLine(final);
      if (finalText) {
        State.appendStep(agentId, finalText);
        const t = task();
        if (t) t.finalLine = finalText;
      }
      if (isCursor && final) cursorSaid(final);
      playSound("listo", State.settings);
      // A card waiting for an answer is never covered by another alert; a
      // conversation on screen already shows it.
      if (cursorInChat) {
        // Already on screen, in its conversation.
      } else if (focused && !State.pendingApproval) surface("finished", true);
      else State.setPillBadge(agentId, "finished");
      cancelStopTimer(agentId);
      stopTimers.set(
        agentId,
        window.setTimeout(() => {
          stopTimers.delete(agentId);
          if (task()?.state !== "finished") return;
          if (isExternalAgent && botDetail.id !== agentId) {
            State.removeTask(agentId);
          } else {
            State.updateTask(agentId, "idle");
            State.setPillBadge(agentId, null);
          }
        }, 5200),
      );
      break;
    }

    case "Interrupt":
      // Codex: the user stopped the turn. Back to idle, nothing to celebrate.
      supersedeStop();
      State.updateTask(agentId, "idle");
      State.setPillBadge(agentId, null);
      break;

    case "StopFailure":
      ensurePill();
      supersedeStop();
      State.updateTask(agentId, "error");
      if (isCursor) cursorSaid(payload.message || t("The session failed."), "error");
      playSound("error", State.settings);
      if (cursorInChat) break;
      if (focused && !State.pendingApproval) surface("error", true);
      else State.setPillBadge(agentId, "error");
      break;

    case "SessionEnd":
      // Nothing left for the timer to do, and it must not outlive the session: a
      // pill recreated within 5.2 s would be removed by it.
      cancelStopTimer(agentId);
      State.clearSessionDiffs(agentId);
      if (cursorInChat) {
        State.updateTask(agentId, "idle");
      } else if (isExternalAgent) {
        State.removeTask(agentId);
      } else {
        State.updateTask(agentId, "idle");
        clearSession(agentId);
      }
      break;

    case "SubagentStart":
      State.appendStep(agentId, t("+ subagent"));
      break;

    case "SubagentStop":
      State.appendStep(agentId, t("• subagent done"));
      break;

    // A Grok Bot reporting through `coucou-hook --bot`.
    case "BotUpdate": {
      if (!isBotPill) break;
      ensurePill();
      const message = (payload.message ?? "").trim();
      const t = task();
      // The conversation: what it says when it finishes, needs you or fails is
      // a message; "working" updates are steps.
      if (message) {
        const status = payload.bot_status ?? "working";
        if (status === "working") BotChat.add(botSlug, { kind: "step", text: message });
        else {
          // A question with choices but no relay request: answered as a message.
          const choices = status === "needs" ? parseChoices(payload as unknown as Record<string, unknown>) : null;
          BotChat.add(botSlug, { kind: "bot", text: message, status, ...(choices ? { options: choices.options, allowCustom: choices.allowCustom } : {}) });
        }
      }
      switch (payload.bot_status) {
        case "done":
          State.updateTask(agentId, "finished");
          if (t && message) t.lastMessage = message;
          if (message) State.appendStep(agentId, toOneLine(message, 80));
          playSound("listo", State.settings);
          if (inChat) {
            // Already on screen, in the conversation.
          } else if (focused && !State.pendingApproval) surface("finished", true);
          else {
            State.setPillBadge(agentId, "finished");
            island.reveal();
          }
          {
            // Back to idle after 8 s, unless the owner is answering it right there.
            const settle = () => {
              if (task()?.state !== "finished") return;
              if (botReplyBusy(agentId)) window.setTimeout(settle, 2000);
              else State.removeTask(agentId);
            };
            window.setTimeout(settle, 8000);
          }
          break;
        case "needs":
          State.updateTask(agentId, "question");
          if (t && message) t.lastMessage = message;
          if (message) State.appendStep(agentId, message.slice(0, 80));
          playSound("pregunta", State.settings);
          State.setPillBadge(agentId, "approval");
          if (!State.pendingApproval) {
            State.setFocus(agentId);
            surface("overview", true);
          }
          break;
        case "error":
          State.updateTask(agentId, "error");
          if (t && message) t.lastMessage = message;
          if (message) State.appendStep(agentId, `⚠ ${message.slice(0, 78)}`);
          playSound("error", State.settings);
          if (inChat) break;
          if (focused && !State.pendingApproval) surface("error", true);
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
      // Claude Code, the agents the relay answers for (Codex, Copilot CLI, Muse
      // Code — same as the Mac), Cursor and the Grok Bots get a card. Anyone
      // else's request is declined at once, so the agent asks in its own UI.
      if (isExternalAgent && !approves(validAgent!)) {
        if (requestId) void Bridge.approvalDecline(requestId);
        break;
      }
      // One card, one request. A second one must never quietly replace the first
      // — that would leave a human staring at request B while request A waits for
      // a decision nobody can give. Hand it straight back to the terminal.
      if (State.pendingApproval && State.pendingApproval.requestId !== requestId) {
        if (requestId) void Bridge.approvalDecline(requestId);
        logBotDecline(payload, "busy");
        break;
      }
      ensurePill();
      supersedeStop();
      if (pendingTimeout != null) window.clearTimeout(pendingTimeout);
      const tool = payload.tool_name ?? "Tool";
      const input = payload.tool_input ?? {};
      // An agent asking questions (Claude Code's AskUserQuestion, Cursor's
      // AskQuestion with several questions) is not a permission to grant: the
      // island shows the options and sends back the ones that were picked.
      const questions = !isExternalAgent || isCursor ? askedQuestions(tool, input) : null;
      const view = questions ? "question" : "approval";
      const suggestions = Array.isArray(payload.permission_suggestions) ? payload.permission_suggestions : [];
      // A Grok Bot: the same card, generic on tool_name / tool_input, whether
      // it came from `--status ask` or from a tool call. A question reads as
      // prose; a tool shows its arguments in full under the target line.
      const asked = typeof input.command === "string" ? input.command : (payload.message ?? "");
      // "Pregunta": a Bot's --status ask, or Cursor's single AskQuestion
      // (hook/src/cursor.rs); the question is in tool_input.command.
      const isQuestion = tool === BOT_QUESTION_TOOL;
      // "Escribir en Cursor" (cursorlink.rs): the whole order under review,
      // wrapped and scrollable; the target line is its first line.
      const isCursorWrite = tool === CURSOR_WRITE.tool;
      const order = isCursorWrite && typeof input[CURSOR_WRITE.field] === "string" ? String(input[CURSOR_WRITE.field]) : "";
      let detail: string | null = null;
      if (isQuestion) detail = asked.length > 70 ? asked.slice(0, ARGS_MAX) : null;
      else if (isCursorWrite) detail = order.trim() ? order : null;
      else if (isBotPill) detail = botArgs(input);
      // Choices (botcmds.ts QUESTION_OPTIONS): one button each, on the taller
      // card — only on a question card, never on a tool approval.
      const choices = isQuestion ? parseChoices(payload as unknown as Record<string, unknown>) : null;
      setApprovalDetail(!!detail || !!choices);
      // The card always comes up, even over another pill or an island that is
      // already open: its pill comes to the front, and the one you were on
      // comes back once you answer (Mac #117, #120).
      State.beginApproval({
        requestId,
        sessionId,
        pillId: agentId,
        tool,
        command: isQuestion
          ? asked.replace(/\s+/g, " ").trim()
          : isCursorWrite && order.trim()
            ? firstLine(order)
            : approvalTarget(tool, input),
        ...(questions ? { questions } : {}),
        ...(agentId === CLAUDE_ID && suggestions.length > 0 ? { allowAlways: true } : {}),
        ...(detail ? { detail } : {}),
        ...(isBotPill || isQuestion || isCursorWrite ? { detailWrap: true } : {}),
        ...(isBotPill ? { toolInput: input } : {}),
        ...(choices ? { options: choices.options, allowCustom: choices.allowCustom } : {}),
      });
      // The relay's short ack window closes in 800 ms; everything below this
      // line is synchronous, so the card really is up by the time it lands.
      if (requestId) void Bridge.approvalAck(requestId);
      State.updateTask(agentId, view);
      playSound("pregunta", State.settings);
      // A Bot's conversation on screen answers it inline (Permitir / Denegar).
      if (!inChat) island.alert(view);
      // Coucou answers within 108 s or not at all; after that the terminal has
      // taken over and the card would be lying.
      pendingTimeout = window.setTimeout(() => {
        pendingTimeout = null;
        const req = State.pendingApproval;
        if (req?.requestId !== requestId) return;
        if (isBotPill) {
          recordBotApproval({
            bot: botSlug, name: botName(agentId), tool: req.tool,
            decision: "timeout", target: req.command, input: req.toolInput ?? null,
          });
        }
        dropPendingCard(island);
        // A Cursor order nobody approved never left: Cursor isn't working on it.
        if (req.tool === CURSOR_WRITE.tool) State.updateTask(agentId, "idle");
      }, 110_000);
      break;
    }

    default:
      break;
  }
  State.notify();
}
