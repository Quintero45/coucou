// Live diff for the edits agents make — the Windows side of DiffEngine.swift.
// Claude Code sends Edit / MultiEdit / Write tool inputs; Cursor's afterFileEdit
// arrives (via the relay) as an Edit with an `edits` array.

import type { DiffLine, FileDiff } from "./state";

/** Beyond this many lines a plain LCS gets slow; the diff is then approximate. */
const MAX_LCS = 600;

function lcsDiff(before: string[], after: string[]): DiffLine[] {
  const n = before.length;
  const m = after.length;
  if (n * m > MAX_LCS * MAX_LCS) {
    return [
      ...before.map((text) => ({ kind: "del" as const, text })),
      ...after.map((text) => ({ kind: "add" as const, text })),
    ];
  }
  const dp: Uint32Array[] = Array.from({ length: n + 1 }, () => new Uint32Array(m + 1));
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      dp[i][j] = before[i] === after[j] ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1]);
    }
  }
  const out: DiffLine[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (before[i] === after[j]) {
      out.push({ kind: "ctx", text: before[i] });
      i++;
      j++;
    } else if (dp[i + 1][j] >= dp[i][j + 1]) {
      out.push({ kind: "del", text: before[i++] });
    } else {
      out.push({ kind: "add", text: after[j++] });
    }
  }
  while (i < n) out.push({ kind: "del", text: before[i++] });
  while (j < m) out.push({ kind: "add", text: after[j++] });
  return out;
}

function splitLines(s: string): string[] {
  if (!s) return [];
  return s.replace(/\r\n/g, "\n").split("\n");
}

function str(v: unknown): string {
  return typeof v === "string" ? v : "";
}

function fileName(p: string): string {
  const cleaned = p.replace(/[\\/]+$/, "");
  const idx = Math.max(cleaned.lastIndexOf("\\"), cleaned.lastIndexOf("/"));
  return idx >= 0 ? cleaned.slice(idx + 1) : cleaned;
}

/** A diff for the edit tools we know, or null for anything else. */
export function buildFileDiff(tool: string, input: Record<string, unknown>): FileDiff | null {
  const path = str(input.file_path) || str(input.path);
  if (!path) return null;

  let lines: DiffLine[] = [];
  const t = tool.toLowerCase();
  if (t === "write" || t === "notebookedit") {
    lines = splitLines(str(input.content)).map((text) => ({ kind: "add" as const, text }));
  } else if (Array.isArray(input.edits)) {
    for (const e of input.edits as Record<string, unknown>[]) {
      if (lines.length) lines.push({ kind: "ctx", text: "…" });
      lines.push(...lcsDiff(splitLines(str(e.old_string)), splitLines(str(e.new_string))));
    }
  } else if ("old_string" in input || "new_string" in input) {
    lines = lcsDiff(splitLines(str(input.old_string)), splitLines(str(input.new_string)));
  } else {
    return null;
  }

  const added = lines.filter((l) => l.kind === "add").length;
  const removed = lines.filter((l) => l.kind === "del").length;
  if (!added && !removed) return null;
  return { file: fileName(path), added, removed, lines: compact(lines) };
}

/** Keep three lines of context around changes, like the settings diff. */
function compact(lines: DiffLine[]): DiffLine[] {
  const keep = new Array(lines.length).fill(false);
  lines.forEach((l, i) => {
    if (l.kind === "ctx") return;
    for (let k = Math.max(0, i - 3); k < Math.min(lines.length, i + 4); k++) keep[k] = true;
  });
  const out: DiffLine[] = [];
  let gap = false;
  lines.forEach((l, i) => {
    if (keep[i]) {
      out.push(l);
      gap = false;
    } else if (!gap) {
      out.push({ kind: "ctx", text: "…" });
      gap = true;
    }
  });
  return out.slice(0, 400);
}
