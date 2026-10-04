// Light Markdown for the assistant's replies, built as DOM nodes — never
// innerHTML, so nothing a model writes can become markup. Paragraphs, line
// breaks, headings, lists, fenced code, `code`, **bold**, *italic* and links
// (opened through Rust, http/https only).

import { Bridge } from "../core/bridge";
import { h } from "./dom";

const INLINE =
  /(`[^`\n]+`)|(\*\*[^*\n]+\*\*)|(\*[^*\n]+\*)|\[([^\]\n]+)\]\((https?:\/\/[^)\s]+)\)|(https?:\/\/[^\s)<]+[^\s).,;:!?<])/g;

function link(label: string, url: string): HTMLElement {
  return h("a", {
    class: "md-link",
    href: "#",
    text: label,
    title: url,
    onclick: (e: Event) => {
      e.preventDefault();
      void Bridge.openUrl(url);
    },
  });
}

function inline(text: string, into: HTMLElement) {
  let last = 0;
  for (const m of text.matchAll(INLINE)) {
    const at = m.index ?? 0;
    if (at > last) into.append(text.slice(last, at));
    if (m[1]) into.append(h("code", { class: "md-code", text: m[1].slice(1, -1) }));
    else if (m[2]) into.append(h("strong", { text: m[2].slice(2, -2) }));
    else if (m[3]) into.append(h("em", { text: m[3].slice(1, -1) }));
    else if (m[4] && m[5]) into.append(link(m[4], m[5]));
    else if (m[6]) into.append(link(m[6], m[6]));
    last = at + m[0].length;
  }
  if (last < text.length) into.append(text.slice(last));
}

function lines(texts: string[], into: HTMLElement) {
  texts.forEach((t, i) => {
    if (i > 0) into.append(h("br"));
    inline(t, into);
  });
}

export function renderMarkdown(text: string): HTMLElement {
  const root = h("div", { class: "md" });
  const src = text.replace(/\r/g, "").split("\n");
  let para: string[] = [];
  let list: { el: HTMLElement; ordered: boolean } | null = null;

  const flushPara = () => {
    if (para.length) {
      const p = h("p");
      lines(para, p);
      root.append(p);
      para = [];
    }
  };
  const flushList = () => {
    if (list) root.append(list.el);
    list = null;
  };

  for (let i = 0; i < src.length; i++) {
    const line = src[i];
    const fence = line.match(/^\s*```(\S*)/);
    if (fence) {
      flushPara();
      flushList();
      const code: string[] = [];
      i++;
      while (i < src.length && !/^\s*```/.test(src[i])) code.push(src[i++]);
      root.append(h("pre", { class: "md-pre" }, h("code", { text: code.join("\n") })));
      continue;
    }
    const heading = line.match(/^#{1,6}\s+(.*)$/);
    if (heading) {
      flushPara();
      flushList();
      const el = h("div", { class: "md-h" });
      inline(heading[1], el);
      root.append(el);
      continue;
    }
    const bullet = line.match(/^\s*(?:[-*•])\s+(.*)$/);
    const numbered = line.match(/^\s*\d+[.)]\s+(.*)$/);
    if (bullet || numbered) {
      flushPara();
      const ordered = !!numbered;
      if (!list || list.ordered !== ordered) {
        flushList();
        list = { el: h(ordered ? "ol" : "ul", { class: "md-list" }), ordered };
      }
      const li = h("li");
      inline((bullet ?? numbered)![1], li);
      list.el.append(li);
      continue;
    }
    if (!line.trim()) {
      flushPara();
      flushList();
      continue;
    }
    flushList();
    para.push(line);
  }
  flushPara();
  flushList();
  return root;
}
