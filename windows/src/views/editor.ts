// The editor view: an agent's file edit across the whole overview, as a small
// code window — file tab, real line numbers, the lines around the change, the
// removed lines struck through and the new ones typed in. On its left, under
// Mochi, the agent and its latest steps.

import { h, svg, clear } from "./dom";
import { ICONS } from "./icons";
import { highlightLine, languageOf } from "./highlight";
import { reducedMotion } from "./integrations";
import { fileName, parseDiffStep, type DiffHunk, type FileDiff } from "../core/diff";
import { Bridge } from "../core/bridge";
import type { AgentTask } from "../core/state";
import { t } from "../i18n/i18n";

type EditCtx = { line: number; before: string[]; after: string[] } | null;
interface Row { kind: "ctx" | "add" | "del" | "gap"; num: number | null; text: string }

export interface EditorHooks {
  back(): void;
  open(path: string): void;
}

/** A new edit types itself in for this long after it arrived. */
const FRESH_MS = 4000;
/** The whole typing never takes longer than this. */
const TYPE_BUDGET_MS = 1400;
const BADGES: Record<string, [string, string]> = {
  ts: ["#3178C6", "#fff"], tsx: ["#3178C6", "#fff"], mts: ["#3178C6", "#fff"],
  js: ["#F7DF1E", "#111"], jsx: ["#F7DF1E", "#111"], mjs: ["#F7DF1E", "#111"], cjs: ["#F7DF1E", "#111"],
  rs: ["#DEA584", "#111"], py: ["#3572A5", "#fff"], go: ["#00ADD8", "#111"], css: ["#663399", "#fff"],
  json: ["#CBCB41", "#111"], md: ["#4B6BFB", "#fff"], html: ["#E34C26", "#fff"], java: ["#B07219", "#fff"],
  cs: ["#178600", "#fff"], swift: ["#F05138", "#fff"], kt: ["#A97BFF", "#fff"], sh: ["#89E051", "#111"],
};

const isBusy = (task: AgentTask) => task.state === "working" || task.state === "thinking" || task.state === "searching";

/**
 * What to look for in the file to find a hunk: its whole edit as the agent
 * wrote it (more text, so more often in one place only), else the hunk's own
 * new side. `from`/`to` are the hunk's lines inside that text.
 */
export function lookupOf(diff: FileDiff, hunk: DiffHunk): { text: string; from: number; to: number } {
  const kept = hunk.lines.filter((l) => l.kind !== "removed");
  if (kept.length === 0) return { text: "", from: 1, to: 1 };
  const whole = hunk.edit != null ? diff.edits?.[hunk.edit] : undefined;
  if (whole != null) return { text: whole, from: kept[0].newLine, to: kept[kept.length - 1].newLine };
  return { text: kept.map((l) => l.text).join("\n"), from: 1, to: kept.length };
}

/** The rows to draw: each hunk with its real line numbers and the file's lines around it, when found. */
export function editorRows(diff: FileDiff, ctx: EditCtx[] | undefined): Row[] {
  const rows: Row[] = [];
  diff.hunks.forEach((hunk, i) => {
    if (i > 0) rows.push({ kind: "gap", num: null, text: "" });
    const c = ctx?.[i] ?? null;
    const firstNew = hunk.lines.find((l) => l.kind !== "removed")?.newLine ?? hunk.newStart;
    const base = diff.isNewFile ? 0 : c ? c.line - firstNew : null;
    if (c) c.before.forEach((text, k) => rows.push({ kind: "ctx", num: c.line - c.before.length + k, text }));
    let next = base != null ? base + firstNew : null;
    for (const l of hunk.lines) {
      if (l.kind === "removed") {
        rows.push({ kind: "del", num: next, text: l.text });
      } else {
        const num = base != null ? base + l.newLine : null;
        rows.push({ kind: l.kind === "added" ? "add" : "ctx", num, text: l.text });
        next = num != null ? num + 1 : null;
      }
    }
    if (c && next != null) c.after.forEach((text, k) => rows.push({ kind: "ctx", num: next! + k, text }));
  });
  return rows;
}

/** A step of the side list: an edit by its file, anything else as written. */
function stepText(step: string): string {
  const d = parseDiffStep(step);
  return d ? `${t("Edits")} · ${d.filename}` : step;
}

export function createEditor(hooks: EditorHooks) {
  const nameEl = h("b", { class: "ld-name" });
  const subEl = h("span", { class: "ld-sub" });
  const stepsEl = h("div", { class: "ld-steps" });
  const side = h("div", { class: "ld-side" }, nameEl, subEl, stepsEl);

  const badge = h("span", { class: "ld-badge" });
  const fileEl = h("b", { class: "ld-file" });
  const dirty = h("i", { class: "ld-dirty" });
  const counts = h("span", { class: "ld-counts" });
  const pathEl = h("span", { class: "ld-path" });
  const back = h("button", { class: "ld-back", title: t("Back"), onclick: () => hooks.back() },
    svg(ICONS.chevronLeft, 9, { stroke: 2.4 }));
  const openBtn = h("button", { class: "icon-btn", title: t("Open in VS Code") }, svg(ICONS.arrowUpRight, 8));
  const tab = h("div", { class: "ld-tab" }, badge, fileEl, dirty);
  const bar = h("div", { class: "ld-bar" }, back, tab, counts, h("span", { class: "grow" }), pathEl, openBtn);
  const code = h("div", { class: "ld-code" });
  const editor = h("div", { class: "ld-editor" }, bar, code);
  const el = h("div", { class: "live-editor" }, side, editor);

  /** Line lookups per diff id (editctx.rs); a few are enough. */
  const contexts = new Map<number, EditCtx[]>();
  let path = "";
  let codeKey = "";
  let sideKey = "";

  openBtn.addEventListener("click", () => hooks.open(path));

  function fetchContext(diff: FileDiff, redraw: () => void) {
    if (contexts.has(diff.id) || diff.isNewFile || diff.tooLarge) return;
    contexts.set(diff.id, []);
    const lookups = diff.hunks.map((hunk) => lookupOf(diff, hunk));
    if (!lookups.some((l) => l.text.trim())) return;
    void Bridge.editContext(diff.path, lookups).then((found) => {
      if (!found?.some(Boolean)) return;
      contexts.set(diff.id, found);
      while (contexts.size > 12) contexts.delete(contexts.keys().next().value!);
      redraw();
    });
  }

  function drawCode(diff: FileDiff, typing: boolean) {
    clear(code);
    if (diff.tooLarge) {
      code.append(h("div", { class: "ld-note", text: t("Diff too large") }));
      return;
    }
    const rows = editorRows(diff, contexts.get(diff.id));
    if (rows.length === 0) {
      code.append(h("div", { class: "ld-note", text: t("No changes") }));
      return;
    }
    const lang = languageOf(diff.path);
    const added = rows.filter((r) => r.kind === "add");
    const chars = added.reduce((n, r) => n + Math.max(1, r.text.length), 0);
    const perChar = Math.min(22, TYPE_BUDGET_MS / Math.max(1, chars));
    let delay = 0;
    let lastAdd: HTMLElement | null = null;
    const frag = document.createDocumentFragment();
    for (const r of rows) {
      if (r.kind === "gap") {
        frag.append(h("div", { class: "ld-row gap" }, h("span", { class: "ld-num" }), h("span", { class: "ld-txt", text: "⋯" })));
        continue;
      }
      const txt = h("span", { class: "ld-txt" }, ...highlightLine(r.text, lang));
      if (typing && r.kind === "add") {
        const n = Math.max(1, r.text.length);
        txt.classList.add("ld-typing");
        txt.style.setProperty("--n", String(n));
        txt.style.setProperty("--d", `${Math.round(n * perChar)}ms`);
        txt.style.setProperty("--delay", `${Math.round(delay)}ms`);
        delay += n * perChar;
      }
      const row = h("div", { class: `ld-row ${r.kind}` },
        h("span", { class: "ld-num", text: r.num != null ? String(r.num) : "" }),
        h("span", { class: "ld-sym", text: r.kind === "add" ? "+" : r.kind === "del" ? "−" : "" }),
        txt);
      if (r.kind === "add") lastAdd = row;
      frag.append(row);
    }
    if (typing && lastAdd) {
      const caret = h("i", { class: "ld-caret" });
      caret.style.setProperty("--delay", `${Math.round(delay)}ms`);
      lastAdd.append(caret);
    }
    code.append(frag);
    // The change in sight, with a couple of lines above it.
    requestAnimationFrame(() => {
      const first = code.querySelector<HTMLElement>(".ld-row.add, .ld-row.del");
      if (first) code.scrollTop = Math.max(0, first.offsetTop - first.offsetHeight * 3);
    });
  }

  const api = {
    el,
    sync(task: AgentTask, diff: FileDiff, subtitle: string) {
      const busy = isBusy(task);
      const sk = [task.id, task.name, task.color, subtitle, task.state, task.steps.slice(-4).join("|")].join("~");
      if (sk !== sideKey) {
        sideKey = sk;
        nameEl.textContent = task.name;
        nameEl.style.color = task.color;
        subEl.textContent = subtitle;
        clear(stepsEl);
        const steps = task.steps.slice(-4);
        steps.forEach((step, i) => {
          const current = i === steps.length - 1;
          const state = current && busy ? "run" : current && task.state === "error" ? "fail" : "done";
          stepsEl.append(h("div", { class: `ld-step ${state}`, title: stepText(step) },
            h("i", { class: "ld-step-ico" }), h("span", { text: stepText(step) })));
        });
      }

      path = diff.path;
      const name = fileName(diff.path);
      const lang = languageOf(diff.path);
      const [bg, fg] = BADGES[lang] ?? ["#5b616e", "#fff"];
      badge.textContent = lang.slice(0, 4).toUpperCase() || "TXT";
      badge.style.background = bg;
      badge.style.color = fg;
      fileEl.textContent = name;
      dirty.style.display = busy ? "" : "none";
      clear(counts);
      if (diff.added > 0) counts.append(h("span", { class: "plus", text: `+${diff.added}` }));
      if (diff.removed > 0) counts.append(h("span", { class: "minus", text: `−${diff.removed}` }));
      pathEl.textContent = diff.path.replace(/\\/g, "/").split("/").slice(-3).join("/");
      pathEl.title = diff.path;

      const ck = `${diff.id}:${contexts.get(diff.id)?.length ?? -1}`;
      if (ck !== codeKey) {
        const firstDraw = !codeKey.startsWith(`${diff.id}:`);
        codeKey = ck;
        const typing = firstDraw && !reducedMotion() && Date.now() - (diff.at ?? 0) < FRESH_MS;
        drawCode(diff, typing);
        fetchContext(diff, () => {
          if (path !== diff.path || !codeKey.startsWith(`${diff.id}:`)) return;
          codeKey = "";
          api.sync(task, diff, subtitle);
        });
      }
    },
  };
  return api;
}
