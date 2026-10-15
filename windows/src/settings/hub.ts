// ARIA's window: a sidebar with Flow (dictation, meeting notes, statistics,
// dictionary, snippets, styles) and ARIA (her assistant, voice, agents, pills,
// integrations, phone, general settings, shortcuts), and the page on the
// right under its coloured band. The layout of Coucou's window (hub/shell.ts),
// drawing the sections Settings already has.

import { h, clear } from "../views/dom";
import { t } from "../i18n/i18n";
import { iconAria } from "../aria/outfits";
import { resolveOutfit, parseOutfit } from "../aria/wardrobe";

export type PageId =
  | "dictation" | "notes" | "insights" | "dictionary" | "snippets" | "styles"
  | "assistant" | "voice" | "agents" | "pills" | "integrations" | "phone" | "general" | "shortcuts";

interface PageDef {
  id: PageId;
  icon: string;
  color: string;
  title: () => string;
  subtitle: () => string;
}

/** Stroke icons on a 24×24 grid (Coucou's hub/ui.ts). */
const ICONS: Record<string, string> = {
  mic: "M12 3a3 3 0 0 0-3 3v6a3 3 0 0 0 6 0V6a3 3 0 0 0-3-3zM5.5 11.5a6.5 6.5 0 0 0 13 0M12 18v3",
  record: "M12 3.5a8.5 8.5 0 1 0 0 17 8.5 8.5 0 0 0 0-17zM12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6z",
  chart: "M5 20v-7M10 20V6M15 20v-9M20 20V9",
  book: "M5 4.5h10a3 3 0 0 1 3 3V20H8a3 3 0 0 1-3-3V4.5zM5 17a3 3 0 0 1 3-3h10",
  bolt: "M13 3 5 13h6l-1 8 8-10h-6l1-8z",
  text: "M3.5 18 8 6l4.5 12M5 14h6M17.5 11a2.8 2.8 0 1 0 0 5.6 2.8 2.8 0 0 0 0-5.6zM20.3 10.8V18",
  gear: "M12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6zM12 2.8v2.4M12 18.8v2.4M2.8 12h2.4M18.8 12h2.4M5.5 5.5l1.7 1.7M16.8 16.8l1.7 1.7M5.5 18.5l1.7-1.7M16.8 7.2l1.7-1.7",
  grid: "M4.5 4.5h6v6h-6zM13.5 4.5h6v6h-6zM4.5 13.5h6v6h-6zM13.5 13.5h6v6h-6z",
  terminal: "M3.5 5.5h17v13h-17zM7 10l3 2.5L7 15M12.5 15H17",
  chat: "M4 5.5h16v10H10l-4 3.5v-3.5H4z",
  puzzle: "M9 4h4v2a1.8 1.8 0 1 0 3.6 0V4H20v5.5h-2a1.8 1.8 0 1 0 0 3.6h2V20h-5.5v-2a1.8 1.8 0 1 0-3.6 0v2H4v-6.4h2a1.8 1.8 0 1 0 0-3.6H4V4h5z",
  phone: "M8 2.8h8a1.6 1.6 0 0 1 1.6 1.6v15.2a1.6 1.6 0 0 1-1.6 1.6H8a1.6 1.6 0 0 1-1.6-1.6V4.4A1.6 1.6 0 0 1 8 2.8zM11 18h2",
  wave: "M4 10v4M8 7v10M12 4v16M16 7v10M20 10v4",
  keyboard: "M3 6.5h18v11H3zM6.5 10h1M10 10h1M13.5 10h1M17 10h.5M7 14h10",
};

function icon(name: string): SVGSVGElement {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("aria-hidden", "true");
  const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
  path.setAttribute("d", ICONS[name] ?? "");
  svg.append(path);
  return svg;
}

export const FLOW_PAGES: PageDef[] = [
  { id: "dictation", icon: "mic", color: "#fb923c", title: () => t("Dictation"), subtitle: () => t("Hold a key, speak, and your words are written where you type.") },
  { id: "notes", icon: "record", color: "#f43f5e", title: () => t("Meeting notes"), subtitle: () => t("Record a call: who said what, a summary and the tasks.") },
  { id: "insights", icon: "chart", color: "#38bdf8", title: () => t("Statistics"), subtitle: () => t("What you dictated, how fast, and where.") },
  { id: "dictionary", icon: "book", color: "#a78bfa", title: () => t("Dictionary"), subtitle: () => t("Names and words written your way.") },
  { id: "snippets", icon: "bolt", color: "#facc15", title: () => t("Snippets"), subtitle: () => t("Say a few words, get a whole text.") },
  { id: "styles", icon: "text", color: "#34d399", title: () => t("Styles"), subtitle: () => t("How the clean-up writes in each app.") },
];

export const ARIA_PAGES: PageDef[] = [
  { id: "assistant", icon: "chat", color: "#2dd4bf", title: () => t("Assistant"), subtitle: () => t("Her AI, her Bots, her tools, her skills and her protected core.") },
  { id: "voice", icon: "wave", color: "#e879f9", title: () => t("Voice"), subtitle: () => t("“Hey ARIA”, her voice and speech recognition.") },
  { id: "agents", icon: "terminal", color: "#60a5fa", title: () => t("Agents"), subtitle: () => t("Claude Code, Cursor, Codex and the other agents ARIA follows.") },
  { id: "pills", icon: "grid", color: "#f472b6", title: () => t("Active pills"), subtitle: () => t("The pills shown in the island, their order and their colours.") },
  { id: "integrations", icon: "puzzle", color: "#c084fc", title: () => t("Integrations"), subtitle: () => t("Stripe, GitHub, Vercel, n8n, Notion and the rest.") },
  { id: "phone", icon: "phone", color: "#4ade80", title: () => t("Android phone"), subtitle: () => t("Your agents and their approvals on your Android phone, over your Wi-Fi.") },
  { id: "general", icon: "gear", color: "#94a3b8", title: () => t("General"), subtitle: () => t("ARIA, her sounds and how the island behaves.") },
  { id: "shortcuts", icon: "keyboard", color: "#fbbf24", title: () => t("Shortcuts"), subtitle: () => t("The keys that open the island and its actions.") },
];

const ALL = [...FLOW_PAGES, ...ARIA_PAGES];

export function isPage(id: string): id is PageId {
  return ALL.some((p) => p.id === id);
}

function band(def: PageDef): HTMLElement {
  const el = h("div", { class: "hub-band" },
    h("div", { class: "hub-band-icon" }, icon(def.icon)),
    h("div", { class: "hub-band-copy" }, h("h1", { text: def.title() }), h("p", { text: def.subtitle() })),
  );
  el.style.setProperty("--band", def.color);
  return el;
}

export interface Hub {
  /** Draws the pages' sections again (after a render), on the page shown. */
  setPages(pages: Partial<Record<PageId, HTMLElement[]>>): void;
  show(page: PageId): void;
  current(): PageId;
}

const LAST_PAGE = "aria.settings.page";

export function createHub(root: HTMLElement, version: string, outfit: string): Hub {
  document.body.classList.add("hub-window");
  let current: PageId = (() => {
    const saved = localStorage.getItem(LAST_PAGE) ?? "";
    return isPage(saved) ? saved : "assistant";
  })();
  let pages: Partial<Record<PageId, HTMLElement[]>> = {};
  const items = new Map<PageId, HTMLElement>();
  const scroll = h("div", { class: "hub-scroll" });
  const tops = new Map<PageId, number>();

  const side = h("nav", { class: "hub-side" });
  const logo = h("canvas", { width: "56", height: "56" }) as HTMLCanvasElement;
  const ctx = logo.getContext("2d");
  if (ctx) {
    ctx.scale(2, 2);
    const worn = resolveOutfit(parseOutfit(outfit), new Date());
    iconAria(ctx, 28, worn);
  }
  side.append(h("div", { class: "hub-brand" }, logo, h("span", { text: "ARIA" })));
  const group = (title: string, defs: PageDef[]) => {
    side.append(h("div", { class: "hub-group", text: title }));
    for (const p of defs) {
      const b = h("button", { class: "hub-item", onclick: () => show(p.id) });
      b.append(icon(p.icon), h("span", { text: p.title() }));
      items.set(p.id, b);
      side.append(b);
    }
  };
  group("Flow", FLOW_PAGES);
  group("ARIA", ARIA_PAGES);
  side.append(
    h("div", { class: "spacer" }),
    h("div", { class: "hub-version", text: `ARIA ${version}` }),
    h("div", { class: "hub-version", text: t("Based on Coucou by Louis Raille") }),
  );

  clear(root);
  root.append(side, h("main", { class: "hub-panel" }, scroll));

  function draw(keepScroll: boolean) {
    const def = ALL.find((p) => p.id === current) ?? ALL[0];
    for (const [id, el] of items) el.classList.toggle("on", id === current);
    const top = scroll.scrollTop;
    clear(scroll);
    const page = h("div", { class: "hub-page" }, band(def));
    for (const s of pages[def.id] ?? []) page.append(s);
    scroll.append(page);
    scroll.scrollTop = keepScroll ? top : (tops.get(def.id) ?? 0);
  }

  function show(page: PageId) {
    if (page !== current) tops.set(current, scroll.scrollTop);
    const same = page === current;
    current = page;
    localStorage.setItem(LAST_PAGE, page);
    draw(same);
  }

  return {
    setPages(next) {
      pages = next;
      draw(true);
    },
    show,
    current: () => current,
  };
}
