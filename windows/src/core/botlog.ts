// Grok Bot approvals: each decision the island takes on a Bot's request goes to
// coucou.log as one `bot-approval {json}` line, and the island / Settings read
// the last ones back (bot_approvals, read-only). Nothing else is stored.
//
// The line also carries the request's tool_input, made safe for a log first:
// file contents are never written (path + length only), long text fields are
// omitted, secret-looking fields are hidden, and the whole thing is capped at
// ~2 KB (`truncated: true`). Old lines without tool_input still parse.

import { BotChat } from "./botchat";
import { Bridge } from "./bridge";

/**
 * allow / always / deny: the owner's click. timeout: no click in time. busy:
 * another card was already up. paused: the island was paused. auto: a read-only
 * tool that runs without asking (written by the relay, not the island).
 */
export type BotDecision = "allow" | "always" | "deny" | "timeout" | "busy" | "paused" | "auto";

export interface BotApproval {
  /** The Bot's slug: its pill is `agent_bot-<bot>`. */
  bot: string;
  name: string;
  tool: string;
  decision: BotDecision;
  /** What the request authorised: the command, the path, the arguments. */
  target: string;
  /** "YYYY-MM-DD HH:MM:SS", local time, taken from the log line. */
  at?: string;
  /**
   * The sanitised tool_input: an object, or a string when it had to be cut
   * (then `truncated`). Undefined on lines written before it existed.
   */
  toolInput?: unknown;
  /** tool_input was longer than the cap and was cut. */
  truncated?: boolean;
  /** Some content was left out (file contents, long text fields). */
  omitted?: boolean;
}

/** What callers hand over: the raw tool_input, sanitised here. */
export type BotApprovalRecord = Omit<BotApproval, "at" | "toolInput" | "truncated" | "omitted"> & {
  input?: Record<string, unknown> | null;
};

export const DECISION_LABELS: Record<BotDecision, { text: string; color: string }> = {
  allow: { text: "Permitido", color: "#22c55e" },
  always: { text: "Permitido", color: "#22c55e" },
  deny: { text: "Denegado", color: "#f4505e" },
  timeout: { text: "Sin respuesta", color: "#f5a524" },
  busy: { text: "Ocupado", color: "#9398a1" },
  paused: { text: "En pausa", color: "#9398a1" },
  auto: { text: "Libre", color: "#60a5fa" },
};

const clean = (s: string, max: number) => s.replace(/\s+/g, " ").trim().slice(0, max);

// ── tool_input sanitising ─────────────────────────────────────────────────────

/** Serialized tool_input cap, in characters. */
export const INPUT_MAX = 2048;
/** A text field longer than this is replaced by its length on any tool. */
const LONG_TEXT = 200;
/** Fields that carry bulk text (a file's body, an edit, a payload). */
const CONTENT_KEYS = new Set(["content", "contents", "new_string", "old_string", "text", "data"]);
/** Tools that write or edit files: their content fields are never logged. */
const WRITE_TOOL = /(write|edit|create|append|save|replace|patch|put)/i;
/** Field names that look like credentials. */
const SECRET_KEY = /(password|passwd|passphrase|token|secret|api[_-]?key|authorization|cookie|credential|private[_-]?key)/i;
/** Values that look like credentials whatever their field is called. */
const SECRET_VALUE = /^(bearer\s+\S|basic\s+\S|sk-[a-z0-9]|sk-ant-|xai-|ghp_|gho_|github_pat_|glpat-|xox[abp]-|AIza)/i;

const HIDDEN = "<oculto>";

/** Free-text redaction: headers, key=value pairs and well-known token prefixes. */
const TEXT_SECRETS: [RegExp, string | ((m: string, ...g: string[]) => string)][] = [
  // "Authorization: Bearer abc", "authorization=xyz" → keep the name, hide the value.
  [/(authorization["']?\s*[:=]\s*["']?)(?:(?:bearer|basic|token)\s+)?[^\s"',;&]+/gi, `$1${HIDDEN}`],
  // "Bearer abc…", "Basic dXNlcjpw…" anywhere else.
  [/\b(bearer|basic)\s+[A-Za-z0-9._~+/=-]{6,}/gi, `$1 ${HIDDEN}`],
  // password=…, token: …, "api_key": "…", client_secret=… (quoted or not).
  [/\b([\w-]*(?:password|passwd|pwd|token|secret|api[_-]?key)["']?\s*[:=]\s*)("[^"]*"|'[^']*'|[^\s&"',;]+)/gi,
    // Quoted values keep their quotes, so JSON-ish text still reads as JSON.
    (_m, key, v) => `${key}${/^["']/.test(v) ? `${v[0]}${HIDDEN}${v[0]}` : HIDDEN}`],
  // Provider keys by prefix.
  [/\b(?:sk-ant-|sk-|xai-|ghp_|gho_|github_pat_|glpat-|xox[abpr]-|AIza)[A-Za-z0-9_-]{8,}/g, HIDDEN],
];

/** Hides secret-looking pieces of a text and leaves the rest as it was. */
export function redactText(text: string): string {
  let out = text;
  for (const [re, by] of TEXT_SECRETS) out = typeof by === "string" ? out.replace(re, by) : out.replace(re, by);
  return out;
}

function sanitize(value: unknown, key: string, writeTool: boolean, depth: number, flags: { omitted: boolean }): unknown {
  if (key && SECRET_KEY.test(key)) return HIDDEN;
  if (typeof value === "string") {
    if (SECRET_VALUE.test(value.trim())) return HIDDEN;
    if (CONTENT_KEYS.has(key.toLowerCase()) && (writeTool || value.length > LONG_TEXT)) {
      flags.omitted = true;
      return `<omitido, ${value.length} caracteres>`;
    }
    // A command or URL can still carry a token inline.
    return redactText(value);
  }
  if (value == null || typeof value !== "object") return value;
  if (depth >= 6) return "…";
  if (Array.isArray(value)) {
    const items = value.slice(0, 50).map((v) => sanitize(v, key, writeTool, depth + 1, flags));
    if (value.length > 50) items.push(`… (+${value.length - 50})`);
    return items;
  }
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
    out[k] = sanitize(v, k, writeTool, depth + 1, flags);
  }
  return out;
}

/** The tool_input as it may go into coucou.log. */
export function safeToolInput(
  tool: string,
  input: Record<string, unknown> | null | undefined,
): { toolInput?: unknown; truncated?: boolean; omitted?: boolean } {
  if (!input || typeof input !== "object" || Object.keys(input).length === 0) return {};
  const flags = { omitted: false };
  const safe = sanitize(input, "", WRITE_TOOL.test(tool), 0, flags);
  const json = JSON.stringify(safe) ?? "";
  const out: { toolInput?: unknown; truncated?: boolean; omitted?: boolean } = {};
  if (json.length > INPUT_MAX) {
    out.toolInput = json.slice(0, INPUT_MAX);
    out.truncated = true;
  } else {
    out.toolInput = safe;
  }
  if (flags.omitted) out.omitted = true;
  return out;
}

// ── Log lines ─────────────────────────────────────────────────────────────────

export function recordBotApproval(e: BotApprovalRecord) {
  const tool = clean(e.tool, 60);
  const safe = safeToolInput(e.tool, e.input);
  // Redacted before the cut, so a secret split at 200 characters is still caught.
  const target = clean(redactText(e.target), 200);
  const line = JSON.stringify({
    bot: clean(e.bot, 24),
    name: clean(e.name, 40),
    tool,
    decision: e.decision,
    target,
    ...(safe.toolInput !== undefined ? { tool_input: safe.toolInput } : {}),
    ...(safe.truncated ? { truncated: true } : {}),
    ...(safe.omitted ? { omitted: true } : {}),
  });
  void Bridge.log(`bot-approval ${line}`);
  // The same, already sanitised, in that Bot's conversation.
  BotChat.add(clean(e.bot, 24), { kind: "perm", tool, decision: e.decision, target, ...safe });
}

const MARK = "bot-approval ";

export function parseBotApproval(line: string): BotApproval | null {
  const at = line.indexOf(`${MARK}{`);
  if (at < 0) return null;
  try {
    const o = JSON.parse(line.slice(at + MARK.length)) as Record<string, unknown>;
    const str = (k: string) => (typeof o[k] === "string" ? (o[k] as string) : "");
    const decision = str("decision") as BotDecision;
    if (!str("bot") || !(decision in DECISION_LABELS)) return null;
    const entry: BotApproval = {
      bot: str("bot"),
      name: str("name") || str("bot"),
      tool: str("tool") || "Tool",
      decision,
      target: str("target"),
      at: /^\d{4}-\d\d-\d\d \d\d:\d\d:\d\d/.exec(line)?.[0],
    };
    // Newer lines only: older ones simply have no tool_input.
    if (o.tool_input !== undefined && o.tool_input !== null) entry.toolInput = o.tool_input;
    if (o.truncated === true) entry.truncated = true;
    if (o.omitted === true) entry.omitted = true;
    return entry;
  } catch {
    return null;
  }
}

/** The last ~20 approvals, newest first. */
export async function loadBotApprovals(): Promise<BotApproval[]> {
  const lines = (await Bridge.botApprovals()) ?? [];
  return lines.map(parseBotApproval).filter((e): e is BotApproval => e !== null).reverse();
}
