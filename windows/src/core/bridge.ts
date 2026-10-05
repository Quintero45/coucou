// Thin wrapper over the Tauri commands/events. Every call is a no-op when the
// page is opened in a plain browser, so the island can be iterated on with
// `npm run dev` alone.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { GrokBot, Settings } from "./state";

export type { GrokBot };

export interface GrokBotStatus extends GrokBot {
  hasKey: boolean;
}

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

  openUrl: (url: string) => call<void>("open_url", { url }),

  /** "Open terminal" → opens the folder in VS Code when `code` is on PATH. */
  openInVSCode: (path: string | null) => call<boolean>("open_in_vscode", { path }),

  quit: () => call<void>("quit_app"),

  openSettingsWindow: () => call<void>("open_settings_window"),

  /** Writes to %LOCALAPPDATA%\Coucou\coucou.log, next to the Rust lines. */
  log: (message: string) => call<void>("log_line", { message }),

  // ── Agent hooks (Claude Code, Cursor, Codex, Gemini CLI) ──────────────────
  hooksStatus: (target: HookTarget = "claude") => call<HookStatus>("hooks_status", { target }),
  /** Diff to show before anything is written. `install: false` previews removal. */
  hooksPreview: (install: boolean, target: HookTarget = "claude", options: HookOptions = {}) =>
    callOrThrow<HookPreview>("hooks_preview", { target, install, options }),
  /**
   * Writes the agent's config — only ever after an explicit click, and only
   * when the file still matches the preview the user looked at.
   */
  hooksApply: (install: boolean, fingerprint: string, target: HookTarget = "claude", options: HookOptions = {}) =>
    callOrThrow<string>("hooks_apply", { target, install, fingerprint, options }),

  approvalDecision: (requestId: string, decision: "allow" | "deny" | "always") =>
    call<void>("approval_decision", { requestId, decision }),
  /** AskUserQuestion answers: `{ question text: chosen label(s) }`. */
  questionAnswer: (requestId: string, answers: Record<string, string>) =>
    call<void>("question_answer", { requestId, answers }),
  /** "The card is up" — until this lands the relay only waits a moment. */
  approvalAck: (requestId: string) => call<void>("approval_ack", { requestId }),
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

export type HookTarget = "claude" | "cursor" | "codex" | "gemini";

export interface HookOptions {
  /** Cursor: route shell and MCP approvals through the island. */
  approvals?: boolean;
}

export interface HookStatus {
  installed: boolean;
  settingsPath: string;
  hookPath: string;
  hookReady: boolean;
  approvals: boolean;
  /** Installed entries match what this build would write. */
  upToDate: boolean;
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
  if (!IS_TAURI) throw new Error("not running inside Coucou");
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
  /** Shift held (OLE key state), when the drop came through the drag overlay. */
  shift?: boolean;
}

/** Files dragged onto the island. Only reaches us when the window takes the mouse. */
export async function onDragDrop(handler: (e: DragDropPayload) => void) {
  if (!IS_TAURI) return () => {};
  // The drag overlay (platform/windows.rs) sends `drag-keys` {shift} right
  // before each enter/over/drop; Tauri's wrapper below rebuilds the payload and
  // would lose an extra field, so it is put back here.
  let shift: boolean | undefined;
  const unKeys = await listen<{ shift: boolean }>("drag-keys", (e) => {
    shift = e.payload.shift;
  });
  const unDrop = await getCurrentWebview().onDragDropEvent((event) => {
    const p = event.payload as DragDropPayload;
    handler(shift === undefined || p.type === "leave" ? p : { ...p, shift });
    if (p.type === "drop" || p.type === "leave") shift = undefined;
  });
  return () => {
    unKeys();
    unDrop();
  };
}

export async function onEvent<T>(name: string, handler: (payload: T) => void) {
  if (!IS_TAURI) return () => {};
  return listen<T>(name, (e) => handler(e.payload));
}
