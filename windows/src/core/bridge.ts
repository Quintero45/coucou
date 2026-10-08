// Thin wrapper over the Tauri commands/events. Every call is a no-op when the
// page is opened in a plain browser, so the island can be iterated on with
// `npm run dev` alone.

import { invoke } from "@tauri-apps/api/core";
import { emitTo, listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { GrokBot, Settings } from "./state";

export type { GrokBot };

export interface GrokBotStatus extends GrokBot {
  hasKey: boolean;
}
import type { RecapHistory, RecapPrefs } from "../recap/summary";

export const IS_TAURI =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T | null> {
  if (!IS_TAURI) return null;
  try {
    return await invoke<T>(cmd, args);
  } catch (err) {
    console.error(`[coucou] ${cmd} failed`, err);
    return null;
  }
}

export interface BootInfo {
  settings: Settings;
  /** Logical screen rect of the monitor the island lives on. */
  screen: { x: number; y: number; width: number; height: number; scale: number };
  version: string;
  hookPath: string;
  /** False where the OS has no global cursor (Wayland): see Island.followPageCursor. */
  cursorPoll: boolean;
}

export const Bridge = {
  boot: () => call<BootInfo>("boot"),
  /** The system's languages as the webview sees them, for Rust's own texts (i18n.rs). */
  setSystemLanguages: (languages: string[]) => call<void>("set_system_languages", { languages }),

  saveSettings: (settings: Settings) => call<void>("save_settings", { settings }),

  /** Shrink the window down to the invisible wake strip (hidden) or back to full. */
  setCollapsed: (collapsed: boolean) => call<void>("set_collapsed", { collapsed }),

  /**
   * Pushes the island shape in window coordinates. Rust flips click-through from
   * its own cursor poll, so the flag is never a frame behind a click.
   */
  setIslandRect: (x: number, y: number, width: number, height: number) =>
    call<void>("set_island_rect", { x, y, width, height }),

  /** Give the window keyboard focus (chat field) and take it away again. */
  focusWindow: (focused: boolean) => call<void>("focus_window", { focused }),

  reposition: () => call<void>("reposition"),

  /** Displays the island can be pinned to: `key` is what `settings.screen` stores. */
  listMonitors: () => call<{ key: string; label: string }[]>("list_monitors"),

  openUrl: (url: string) => call<void>("open_url", { url }),

  /** "Open terminal" → opens the folder in VS Code when `code` is on PATH. */
  openInVSCode: (path: string | null) => call<boolean>("open_in_vscode", { path }),

  /**
   * "Open terminal": the window the session runs in when Rust found it
   * (Windows), else the folder in VS Code.
   */
  openSession: (sessionId: string | null, path: string | null) =>
    call<boolean>("open_session", { sessionId, path }),

  /** The Claude desktop app, for the Claude Desktop pill (Windows only). */
  openClaudeDesktop: () => call<boolean>("open_claude_desktop"),

  /** The diff card's ↗: an existing file, in VS Code; never launched by its type. */
  openFileInVSCode: (path: string) => call<boolean>("open_file_in_vscode", { path }),
  /** Where each piece of an edit sits in its file (editctx.rs): its line and a few lines around it. */
  editContext: (path: string, lookups: { text: string; from: number; to: number }[]) =>
    call<({ line: number; before: string[]; after: string[] } | null)[]>("edit_context", { path, lookups }),

  quit: () => call<void>("quit_app"),

  openSettingsWindow: () => call<void>("open_settings_window"),

  /** Writes to %LOCALAPPDATA%\Coucou\coucou.log, next to the Rust lines. */
  log: (message: string) => call<void>("log_line", { message }),

  // ── Claude Code hooks ─────────────────────────────────────────────────────
  hooksStatus: () => call<HookStatus>("hooks_status"),
  /** Pill ID → whether that agent's hooks reach Coucou (read-only, Mac #183). */
  agentHooksStatus: () => call<Record<string, boolean>>("agent_hooks_status"),
  /** Diff to show before anything is written. `install: false` previews removal. */
  hooksPreview: (install: boolean) => callOrThrow<HookPreview>("hooks_preview", { install }),
  /**
   * Writes settings.json — only ever after an explicit click, and only when
   * the file still matches the preview the user looked at.
   */
  hooksApply: (install: boolean, fingerprint: string) =>
    callOrThrow<string>("hooks_apply", { install, fingerprint }),

  // ── Other agents (Gemini CLI, Codex, Cursor…) ─────────────────────────────
  agentHooksList: () => call<AgentHookStatus[]>("agent_hooks_list"),
  /** Diff to show before anything is written. `install: false` previews removal. */
  agentHooksPreview: (agent: string, install: boolean, options: AgentHookOptions = {}) =>
    callOrThrow<AgentHookPlan>("agent_hooks_preview", { agent, install, options }),
  /**
   * Writes the agent's config — only ever after an explicit click, and only when
   * it still matches the preview the user looked at. Returns the backups taken.
   */
  agentHooksApply: (agent: string, install: boolean, fingerprint: string, options: AgentHookOptions = {}) =>
    callOrThrow<string>("agent_hooks_apply", { agent, install, fingerprint, options }),
  // ── Plan usage: the status line relay, installed apart from the hooks ──────
  /** Diff of the status line change. `install: false` previews taking the relay out. */
  statusLinePreview: (install: boolean) => callOrThrow<HookPreview>("status_line_preview", { install }),
  /** Same rules as hooksApply: an explicit click, and only for the diff that was shown. */
  statusLineApply: (install: boolean, fingerprint: string) =>
    callOrThrow<string>("status_line_apply", { install, fingerprint }),
  /**
   * Asks the Codex CLI (`codex app-server`) for its plan limits, as Codex's
   * /status does. The raw `account/rateLimits/read` result, or null when Codex
   * is missing, not signed in or slow (15 s).
   */
  codexPlanUsage: () => call<unknown>("codex_plan_usage"),

  approvalDecision: (requestId: string, decision: "allow" | "deny" | "always") =>
    call<void>("approval_decision", { requestId, decision }),
  /** "The card is up" — until this lands the relay only waits a moment. */
  approvalAck: (requestId: string) => call<void>("approval_ack", { requestId }),
  /** Answers a question Claude Code asked: question text → chosen label. */
  approvalAnswer: (requestId: string, answers: Record<string, string | string[]>) =>
    call<void>("approval_answer", { requestId, answers }),

  /** "Nobody can act on this" — Claude Code asks in the terminal right away. */
  approvalDecline: (requestId: string) => call<void>("approval_decline", { requestId }),

  // ── Chat, files, secrets ──────────────────────────────────────────────────
  /** One chat turn. The API key and any file bytes never leave Rust. */
  chatSend: (query: string, context: ChatContext | null) =>
    callOrThrow<{ text: string }>("chat_send", { query, context }),
  chatReset: () => call<void>("chat_reset"),
  /** Stops the assistant after the current step. */
  chatStop: () => call<void>("chat_stop"),
  /** Models the provider offers (needs its key, or the local server running). */
  modelsList: (provider: string) => callOrThrow<string[]>("models_list", { provider }),

  // ── Grok Bots ─────────────────────────────────────────────────────────────
  grokbotList: () => call<GrokBotStatus[]>("grokbot_list"),
  /** An empty key keeps the stored one; `previous` is the old id after a rename. */
  grokbotSave: (bot: { previous?: string; name: string; color: string; url: string; key: string }) =>
    callOrThrow<GrokBot>("grokbot_save", bot),
  grokbotRemove: (id: string) => callOrThrow<void>("grokbot_remove", { id }),
  /** Starts the Bot's routine with this task. Spends the owner's Grok Bot usage. */
  grokbotSend: (bot: string, message: string, attachments?: AttachmentIn[]) =>
    callOrThrow<string>("grokbot_send", { bot, message, attachments: attachments ?? null }),
  /** Read-only: the last `bot-approval` lines of coucou.log, oldest first. */
  botApprovals: () => call<string[]>("bot_approvals"),
  grokbotInstructions: (id: string) => callOrThrow<string>("grokbot_instructions", { id }),
  /** The Cursor engine (sdk-bridge): installed, and which version. */
  cursorStatus: () => call<{ installed: boolean; version: string | null }>("cursor_status"),
  /** Downloads and verifies the latest bridge from GitHub. Returns the tag. */
  cursorInstall: () => callOrThrow<string>("cursor_install"),

  // ── Connections (MCP), skills, core ───────────────────────────────────────
  mcpList: () => call<McpStatus[]>("mcp_list"),
  mcpConfig: () => call<{ mcpServers: Record<string, McpServerConfig> }>("mcp_config"),
  /** Secret values (`env:NAME` / `header:NAME`) go to the Credential Manager. */
  mcpSave: (name: string, config: McpServerConfig, secrets: Record<string, string> = {}) =>
    callOrThrow<void>("mcp_save", { name, config, secrets }),
  mcpRemove: (name: string) => callOrThrow<void>("mcp_remove", { name }),
  mcpSetEnabled: (name: string, enabled: boolean) => callOrThrow<void>("mcp_set_enabled", { name, enabled }),
  mcpImportPreview: () => call<McpImportCandidate[]>("mcp_import_preview"),
  mcpImportApply: (names: string[]) => callOrThrow<number>("mcp_import_apply", { names }),
  mcpReconnect: () => call<void>("mcp_reconnect"),
  skillsList: () => call<SkillInfo[]>("skills_list"),
  skillSetEnabled: (name: string, enabled: boolean) => callOrThrow<void>("skill_set_enabled", { name, enabled }),
  skillRemove: (name: string) => callOrThrow<void>("skill_remove", { name }),
  coreStatus: () => call<{ protected: string[]; tampered: string[] }>("core_status"),
  openDataFolder: (which: "skills" | "memory" | "log" | "config") => call<void>("open_data_folder", { which }),

  /** Copies a dropped file into the inbox. */
  ingestFile: (path: string) => callOrThrow<DroppedFile>("ingest_file", { path }),
  /** Copies several files into the inbox; the ids can be sent as Grok Bot attachments. */
  ingestFiles: (paths: string[]) => callOrThrow<IngestedFile[]>("ingest_files", { paths }),
  /** Only ever tells you whether a key exists — never its value. */
  secretPresent: (key: string) => call<boolean>("secret_present", { key }),
  secretSet: (key: string, value: string) => callOrThrow<void>("secret_set", { key, value }),
  secretClear: (key: string) => callOrThrow<void>("secret_clear", { key }),

  // ── Integrations ──────────────────────────────────────────────────────────
  refreshIntegration: (id: string) => call<void>("refresh_integration", { id }),
  /** The GitHub card is on screen: refetch that part if it is stale. */
  githubRefresh: (section: "pulse" | "activity") => call<void>("github_refresh", { section }),
  /** Opens the configured n8n instance in the browser. */
  openN8n: () => call<void>("open_n8n"),

  /** Tray → Pause. Stops the integration pollers, not just the island. */
  setPaused: (paused: boolean) => call<void>("set_paused", { paused }),

  /** Opens a file a Bot shared with `bot-attach` this session (anything that
   * could run code is shown in its folder instead). Rejects other paths. */
  openAttachment: (path: string) => callOrThrow<void>("open_attachment", { path }),
  /** Foreground window, clipboard text and a screenshot of that window; the
   * screenshot is already in the inbox, send it as `{ id }` via grokbotSend. */
  captureContext: () => callOrThrow<CapturedContext>("capture_context"),
  /** Shares the screen with a Grok Bot: a frame every `intervalS` seconds
   * (default 5, 2–60) when it changed, for at most 30 minutes. Starting for
   * another Bot replaces the current share. Progress arrives as `screen-share`. */
  startScreenShare: (bot: string, intervalS?: number) =>
    callOrThrow<ScreenShareEvent>("start_screen_share", { bot, intervalS: intervalS ?? null }),
  /** Ends the share (no-op when none); true when one was running. */
  stopScreenShare: () => callOrThrow<boolean>("stop_screen_share"),

  // ── Global shortcuts ──────────────────────────────────────────────────────
  /** How each global shortcut went when Rust last registered them. */
  shortcutsStatus: () => call<ShortcutsReport>("shortcuts_status"),
  /** Lets go of every global shortcut while Settings records a new one. */
  shortcutsSuspend: (suspended: boolean) => call<void>("shortcuts_suspend", { suspended }),

  // ── Weekly recap ──────────────────────────────────────────────────────────
  /** Turns and decisions from `since` (Unix seconds) on, plus the recap prefs. */
  recapHistory: (since: number) => call<RecapHistory>("recap_history", { since: Math.floor(since) }),
  recapPrefs: () => call<RecapPrefs>("recap_prefs"),
  recapSetEnabled: (enabled: boolean) => call<void>("recap_set_enabled", { enabled }),
  recapSetHideProjects: (hide: boolean) => call<void>("recap_set_hide_projects", { hide }),
  /** `week` is the Monday (YYYY-MM-DD) the recap opened on its own for. */
  recapMarkShown: (week: string) => call<void>("recap_mark_shown", { week }),
  recapClear: () => call<void>("recap_clear"),
  /** Writes the PNG (a data URL) into Pictures or Downloads; returns its path. */
  recapSavePng: (data: string, week: string) => callOrThrow<string>("recap_save_png", { data, week }),
  /** Opens the folder of the image saved last. */
  recapRevealSaved: () => call<void>("recap_reveal_saved"),

  // ── Mochi on the desktop (src-tauri/src/desktop.rs) ───────────────────────
  desktopInfo: () => call<DesktopInfo>("desktop_mochi_info"),
  /** Dragged out of the island: (x, y) is the pointer in island-window coordinates. */
  desktopPickUp: (x: number, y: number) => call<boolean>("desktop_mochi_pick_up", { x, y }),
  /** Linux: the pointer moved during that drag (Windows carries him from Rust). */
  desktopCarry: (x: number, y: number) => call<void>("desktop_mochi_carry", { x, y }),
  desktopCarryEnd: (x: number, y: number) => call<void>("desktop_mochi_carry_end", { x, y }),
  /** A drag started on the desktop Mochi; resolves to his top-left corner. */
  desktopDragBegin: () => call<[number, number] | null>("desktop_mochi_drag_begin"),
  /** X11: top-left corner, physical pixels. */
  desktopDragMove: (x: number, y: number) => call<void>("desktop_mochi_drag_move", { x, y }),
  desktopDragEnd: (x: number, y: number) => call<void>("desktop_mochi_drag_end", { x, y }),
  /** From the island to his spot. False: no spot on any connected display. */
  desktopFlyOut: () => call<boolean>("desktop_mochi_fly_out"),
  /** To the island, then hidden. `forget`: he lives in the island again. */
  desktopFlyHome: (forget: boolean) => call<boolean>("desktop_mochi_fly_home", { forget }),
  /** Asleep, the cursor poll stops. */
  desktopSetAsleep: (asleep: boolean) => call<void>("desktop_mochi_set_asleep", { asleep }),
};

/** `bot-step` event: a progress line on a Bot's pill. `ts` is epoch ms. */
export interface BotStepEvent {
  agent: string;
  bot: string;
  text: string;
  ts: number;
}

/** `bot-attach` event: a file card on a Bot's pill (open it with openAttachment). */
export interface BotAttachEvent {
  agent: string;
  bot: string;
  path: string;
  name: string;
  mime: string;
  size: number;
  caption?: string;
}

/** `island-close` event: the owner clicked outside the expanded island. */
export interface IslandCloseEvent {
  reason: "outside-click";
}

/** `screen-share` event (and startScreenShare's result). `since`: epoch ms of
 * the start; `reason` only when it ended. */
export interface ScreenShareEvent {
  active: boolean;
  bot: string;
  since?: number;
  reason?: "user" | "timeout" | "error" | "replaced" | "quit";
}

/** What `captureContext` returns; missing fields could not be read. */
export interface CapturedContext {
  window_title: string;
  process_name: string;
  selected_text?: string;
  clipboard_text?: string;
  screenshot?: IngestedFile;
}

export type ShortcutStatus =
  | "active" | "off" | "inUse" | "duplicate" | "invalid"
  | "typesCharacter" | "unsupported" | "notPorted";

export interface ShortcutsReport {
  actions: { id: string; status: ShortcutStatus; typed?: string }[];
  /** "wayland" or "no-display" when no global shortcut can be registered. */
  blocked: string | null;
  /** `<executable> --shortcut`: append an action id for a desktop shortcut. */
  command: string;
}

/** How the desktop Mochi's window works here (platform::DesktopMode). */
export type DesktopMode = "poll" | "window" | "layer" | "off";

export interface DesktopInfo {
  mode: DesktopMode;
  /** He was on the desktop when the app last quit. */
  onDesktop: boolean;
}

/** An event for one window only (island ⇄ desktop Mochi). Never throws. */
export async function emitToWindow(label: string, event: string, payload?: unknown) {
  if (!IS_TAURI) return;
  try {
    await emitTo(label, event, payload);
  } catch (err) {
    console.error(`[coucou] emit ${event} failed`, err);
  }
}

export interface IntegrationUpdate {
  id: string;
  data: Record<string, unknown>;
  error: string | null;
  event: { success: boolean; label: string; detail: string | null } | null;
}

export type ChatContext =
  | { kind: "file"; name: string; path: string }
  | { kind: "window"; appName: string; title: string; url?: string };

/** A file copied into the inbox by `ingestFiles`. `id` is the inbox file name. */
export interface IngestedFile {
  id: string;
  name: string;
  mime: string;
  size: number;
}

/** A Grok Bot attachment: an ingested inbox file, or pasted text / base64 bytes. */
export type AttachmentIn =
  | { id: string }
  | { name: string; mime: string; text: string }
  | { name: string; mime: string; base64: string };

export interface DroppedFile {
  name: string;
  path: string;
  size: number;
}

export interface McpServerConfig {
  command?: string;
  args?: string[];
  env?: Record<string, string>;
  cwd?: string;
  url?: string;
  headers?: Record<string, string>;
  disabled?: boolean;
  askAll?: boolean;
}

export interface McpStatus {
  name: string;
  transport: "stdio" | "http";
  state: "connecting" | "connected" | "disabled" | "error";
  error: string | null;
  tools: string[];
}

export interface McpImportCandidate {
  name: string;
  source: string;
  summary: string;
  exists: boolean;
}

export interface SkillInfo {
  name: string;
  description: string;
  kind: "script" | "mcp";
  tools: { name: string; description: string; readOnly: boolean }[];
  enabled: boolean;
}

/** What the user picked next to an agent's Install button (agents.rs). */
export interface AgentHookOptions {
  /** Cursor: route shell and MCP approvals through the island. */
  approvals?: boolean;
}

export interface HookStatus {
  installed: boolean;
  /** Coucou's status line relay (plan usage) is the status line in settings.json. */
  planRelayInstalled: boolean;
  settingsPath: string;
  hookPath: string;
  hookReady: boolean;
}

/** One agent other than Claude Code, as agents.rs reports it. */
export interface AgentHookStatus {
  /** The `--agent` name; its pill is `agent_<id>`. */
  id: string;
  name: string;
  installed: boolean;
  /** The file (or files, one per line) Coucou writes. */
  path: string;
  hookReady: boolean;
  /** The island can allow or deny this agent's permission requests. */
  approvals: boolean;
  /** Cursor only: its shell / MCP approval gates are installed; null for the others. */
  gates: boolean | null;
  /** Installed by an older Coucou: reinstalling brings what's missing. */
  outdated: boolean;
  /** What to do once it is written. */
  note: string;
}

export interface AgentHookPlan {
  diff: string;
  /** Where each existing file is copied first, one per line; "" when none. */
  backup: string;
  path: string;
  fingerprint: string;
}

export interface HookPreview {
  diff: string;
  backup: string;
  settingsPath: string;
  /** Hand back to hooksApply so only the reviewed diff is ever written. */
  fingerprint: string;
}

/** Same as `call`, but surfaces the error so the UI can show what went wrong. */
async function callOrThrow<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!IS_TAURI) throw new Error("not running inside ARIA");
  return invoke<T>(cmd, args);
}

export type BridgeEvent =
  | { name: "cursor"; payload: { x: number; y: number } }
  | { name: "tray"; payload: string }
  | { name: "hook"; payload: Record<string, unknown> }
  | { name: "screen-changed"; payload: null };

export interface DragDropPayload {
  type: "enter" | "over" | "drop" | "leave";
  paths?: string[];
  /** Shift held during the drag (Windows: from the page's own drag events). */
  shift?: boolean;
  /** Physical px in the window, as Tauri's own drag events give it. */
  position?: { x: number; y: number };
}

interface WebView2Bridge {
  postMessageWithAdditionalObjects(message: unknown, objects: ArrayLike<unknown>): void;
}

/**
 * Files dragged onto the island. Only reaches us when the window takes the mouse.
 *
 * On Windows WebView2 takes the drop itself, as in Edge — Tauri's drop handling
 * never sees drags from the classic Explorer folder view (see
 * src-tauri/src/webview_drop.rs). Enter/over/leave come straight from the page;
 * the drop hands the File objects to Rust, which answers with their real paths
 * as a `file-drag` event. Elsewhere (Linux) Tauri's own drag events still do it.
 */
export async function onDragDrop(handler: (e: DragDropPayload) => void) {
  if (!IS_TAURI) return () => {};
  const webview = (window as unknown as { chrome?: { webview?: WebView2Bridge } }).chrome?.webview;
  if (!webview) {
    return getCurrentWebview().onDragDropEvent((event) => {
      handler(event.payload as DragDropPayload);
    });
  }
  const hasFiles = (e: DragEvent) => e.dataTransfer?.types.includes("Files") ?? false;
  // dragenter/dragleave fire for every element crossed; only the outermost pair counts.
  let depth = 0;
  // The paths come back from Rust after the drop: Shift and the spot are remembered until then.
  let dropShift = false;
  let dropAt: { x: number; y: number } | undefined;
  const at = (e: DragEvent) => {
    const dpr = window.devicePixelRatio || 1;
    return { x: e.clientX * dpr, y: e.clientY * dpr };
  };

  const onEnter = (e: DragEvent) => {
    if (!hasFiles(e)) return;
    e.preventDefault();
    if (depth++ === 0) handler({ type: "enter", shift: e.shiftKey, position: at(e) });
  };
  const onOver = (e: DragEvent) => {
    if (!hasFiles(e)) return;
    // Without this WebView2 refuses the drop — or navigates to the file.
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "copy";
    handler({ type: "over", shift: e.shiftKey, position: at(e) });
  };
  const onLeave = (e: DragEvent) => {
    if (!hasFiles(e) || depth === 0) return;
    if (--depth === 0) handler({ type: "leave" });
  };
  const onDrop = (e: DragEvent) => {
    e.preventDefault();
    depth = 0;
    dropShift = e.shiftKey;
    dropAt = at(e);
    const files = e.dataTransfer?.files;
    if (!files || files.length === 0) {
      handler({ type: "drop", paths: [], shift: dropShift, position: dropAt });
      return;
    }
    webview.postMessageWithAdditionalObjects("coucou-file-drop", files);
  };

  window.addEventListener("dragenter", onEnter);
  window.addEventListener("dragover", onOver);
  window.addEventListener("dragleave", onLeave);
  window.addEventListener("drop", onDrop);
  const unlisten = await listen<DragDropPayload>("file-drag", (e) =>
    handler(e.payload.type === "drop" ? { ...e.payload, shift: dropShift, position: dropAt } : e.payload),
  );
  return () => {
    window.removeEventListener("dragenter", onEnter);
    window.removeEventListener("dragover", onOver);
    window.removeEventListener("dragleave", onLeave);
    window.removeEventListener("drop", onDrop);
    unlisten();
  };
}

export async function onEvent<T>(name: string, handler: (payload: T) => void) {
  if (!IS_TAURI) return () => {};
  return listen<T>(name, (e) => handler(e.payload));
}
