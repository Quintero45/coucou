// Settings for the assistant: provider, model and keys; the owner's Grok Bots;
// MCP connections; skills Mochi installed; the protected core. Every write here
// is a click in this window — keys go to the Credential Manager, never to disk.

import {
  Bridge, onEvent,
  type GrokBotStatus, type McpImportCandidate, type McpServerConfig, type McpStatus, type SkillInfo,
} from "../core/bridge";
import { DECISION_LABELS, loadBotApprovals, type BotApproval } from "../core/botlog";
import { AI_PROVIDERS, aiProvider, defaultBotColor, type Settings } from "../core/state";
import { N_, labels, t } from "../i18n/i18n";
import { h, clear } from "../views/dom";

export interface SettingsCtx {
  get(): Settings;
  save(): Promise<void>;
}

function dot(color: string): HTMLElement {
  return h("i", { class: "dot", style: `background:${color}` });
}

function notice(kind: "ok" | "err" | "warn", text: string): HTMLElement {
  return h("div", { class: `notice ${kind}`, text });
}

const errText = (err: unknown) => String(err).replace(/^Error:\s*/, "");

const stored = () => `••••••••••••  ${t("(stored)")}`;

/** Password field + Save / Remove for one Credential Manager key. */
function keyRow(label: string, key: string, placeholder: string, present: boolean, feedback: HTMLElement): HTMLElement {
  const status = dot(present ? "#22c55e" : "#f4505e");
  const field = h("input", {
    type: "password",
    placeholder: present ? stored() : placeholder,
    style: "flex:1 1 auto;min-width:0",
    autocomplete: "off",
    spellcheck: "false",
  }) as HTMLInputElement;
  const saveBtn = h("button", { class: "primary", text: t("Save") });
  const removeBtn = h("button", { class: "danger", text: t("Remove") });
  removeBtn.style.display = present ? "" : "none";
  const set = (on: boolean) => {
    status.style.background = on ? "#22c55e" : "#f4505e";
    field.placeholder = on ? stored() : placeholder;
    removeBtn.style.display = on ? "" : "none";
  };
  saveBtn.addEventListener("click", async () => {
    const value = field.value.trim();
    if (!value) return;
    clear(feedback);
    try {
      await Bridge.secretSet(key, value);
      field.value = "";
      set(true);
      feedback.append(notice("ok", t("Saved in the Windows Credential Manager. Never written to disk.")));
    } catch (err) {
      feedback.append(notice("err", t("Could not save: {error}", { error: errText(err) })));
    }
  });
  removeBtn.addEventListener("click", async () => {
    clear(feedback);
    try {
      await Bridge.secretClear(key);
      set(false);
      feedback.append(notice("ok", t("Key removed.")));
    } catch (err) {
      feedback.append(notice("err", t("Could not remove: {error}", { error: errText(err) })));
    }
  });
  return h("div", { class: "row" }, h("label", { text: label }), field, saveBtn, removeBtn, status);
}

// ── Assistant ─────────────────────────────────────────────────────────────────

const PRESET_MODELS: Record<string, string[]> = {
  cursor: ["grok"],
  anthropic: ["claude-opus-5", "claude-sonnet-5", "claude-haiku-4-5"],
  xai: ["grok-4", "grok-4-fast", "grok-code-fast-1"],
  openai: ["gpt-5", "gpt-5-mini", "o4-mini"],
  google: ["gemini-2.5-pro", "gemini-2.5-flash"],
  ollama: ["llama3.2", "qwen2.5-coder", "mistral"],
  lmstudio: [],
};

const KEY_PLACEHOLDERS: Record<string, string> = {
  cursor: "cursor_…", anthropic: "sk-ant-…", xai: "xai-…", openai: "sk-…", google: "AIza…",
};

const installedText = (version: string | null | undefined) =>
  version ? t("Installed ({version}).", { version }) : t("Installed.");

/** The Cursor engine: the SDK bridge Coucou drives, downloaded on a click. */
async function cursorEngineRows(feedback: HTMLElement): Promise<HTMLElement[]> {
  const st = await Bridge.cursorStatus();
  const state = h("span", {
    class: "hint",
    text: st?.installed ? installedText(st.version) : t("Not installed."),
  });
  const install = h("button", { class: st?.installed ? "" : "primary", text: st?.installed ? t("Update engine") : t("Install engine") });
  install.addEventListener("click", async () => {
    clear(feedback);
    install.disabled = true;
    state.textContent = t("Downloading from GitHub (cursor/sdk-bridge)…");
    try {
      const v = await Bridge.cursorInstall();
      state.textContent = installedText(v);
      feedback.append(notice("ok", t("Cursor engine ready. Verified against the SHA-256 checksum Cursor publishes.")));
    } catch (err) {
      state.textContent = t("Not installed.");
      feedback.append(notice("err", errText(err)));
    } finally {
      install.disabled = false;
    }
  });
  return [
    h("div", { class: "row" }, h("label", { text: t("Engine") }), dot(st?.installed ? "#22c55e" : "#f4505e"), state, install),
    h("div", {
      class: "hint",
      text: t("ARIA thinks with Grok through your Cursor account, the same one as your Grok Bots. 1) At cursor.com/dashboard/integrations, create a “User API Key”. 2) Paste it in “Cursor key” and click Save. 3) Click “Install engine”. 4) Write to ARIA. Cursor's own tools stay off: ARIA only acts with its own, and every action asks for your permission in the island."),
    }),
  ];
}

export async function assistantSection(ctx: SettingsCtx): Promise<HTMLElement> {
  const body = h("div", { style: "display:flex;flex-direction:column;gap:12px" });
  const head = h("h2", {});
  const section = h("section", {}, head, body);

  async function draw() {
    const s = ctx.get();
    const p = aiProvider(s.provider);
    const present = p.key ? ((await Bridge.secretPresent(p.key)) ?? false) : true;
    const bravePresent = (await Bridge.secretPresent("brave-api-key")) ?? false;
    clear(head);
    head.append(dot(present ? p.color : "#f4505e"), h("span", { text: t("Assistant") }));
    clear(body);
    const feedback = h("div", {});

    const provider = h("select", {}) as HTMLSelectElement;
    for (const ap of AI_PROVIDERS) provider.append(h("option", { value: ap.id, text: ap.name }));
    provider.value = p.id;
    provider.addEventListener("change", async () => {
      ctx.get().provider = provider.value;
      await ctx.save();
      void draw();
    });

    body.append(
      h("div", {
        class: "hint",
        text: t("ARIA answers with the engine you pick here and can act on this PC with its tools: anything that changes something asks for your permission first, in the island."),
      }),
      h("div", { class: "row" }, h("label", { text: t("AI engine") }), provider),
    );

    if (p.key) {
      body.append(keyRow(p.id === "cursor" ? t("Cursor key") : t("API key"), p.key, KEY_PLACEHOLDERS[p.id] ?? "…", present, feedback));
    } else {
      const url = h("input", {
        type: "text",
        value: s.providerUrls[p.id] ?? "",
        placeholder: p.defaultUrl ?? "",
        style: "flex:1 1 auto;min-width:0",
        spellcheck: "false",
      }) as HTMLInputElement;
      url.addEventListener("change", () => {
        ctx.get().providerUrls = { ...ctx.get().providerUrls, [p.id]: url.value.trim() };
        void ctx.save();
      });
      body.append(
        h("div", { class: "row" }, h("label", { text: t("Server URL") }), url),
        h("div", { class: "hint", text: t("{name} runs on this PC: no key, nothing leaves your computer. Open it before chatting.", { name: p.name }) }),
      );
    }
    if (p.id === "cursor") body.append(...(await cursorEngineRows(feedback)));

    // Model: free text with suggestions, and the provider's real list on demand.
    const current = p.id === "anthropic" ? s.model : (s.providerModels[p.id] ?? "");
    const listId = `models-${p.id}`;
    const datalist = h("datalist", { id: listId });
    for (const m of PRESET_MODELS[p.id] ?? []) datalist.append(h("option", { value: m }));
    const model = h("input", {
      type: "text",
      list: listId,
      value: current,
      placeholder: p.defaultModel,
      style: "flex:1 1 auto;min-width:0",
      spellcheck: "false",
    }) as HTMLInputElement;
    model.addEventListener("change", () => {
      const v = model.value.trim();
      if (p.id === "anthropic") ctx.get().model = v || p.defaultModel;
      else ctx.get().providerModels = { ...ctx.get().providerModels, [p.id]: v };
      void ctx.save();
    });
    const fetchBtn = h("button", { text: t("See models") });
    fetchBtn.addEventListener("click", async () => {
      clear(feedback);
      fetchBtn.disabled = true;
      try {
        const models = await Bridge.modelsList(p.id);
        clear(datalist);
        for (const m of models) datalist.append(h("option", { value: m }));
        feedback.append(notice("ok", t("Models available: {count}. Type in the field to pick one.", { count: models.length })));
      } catch (err) {
        feedback.append(notice("err", errText(err)));
      } finally {
        fetchBtn.disabled = false;
      }
    });
    body.append(h("div", { class: "row" }, h("label", { text: t("Model") }), model, datalist, fetchBtn));
    if (p.id === "cursor") {
      body.append(h("div", { class: "hint", text: t("“grok” picks the newest Grok on your account by itself.") }));
    }

    const tools = h("button", { class: s.assistantTools ? "switch on" : "switch" });
    tools.addEventListener("click", () => {
      const next = !tools.classList.contains("on");
      tools.classList.toggle("on", next);
      ctx.get().assistantTools = next;
      void ctx.save();
    });
    body.append(
      h("div", { class: "row" },
        h("label", { text: t("Tools") }),
        tools,
        h("span", { class: "hint", text: t("Files, PowerShell, apps, the web, your integrations, your Grok Bots and MCP connections. Reading is free; every action waits for your click.") }),
      ),
    );

    const braveFeedback = h("div", {});
    body.append(
      keyRow("Brave Search", "brave-api-key", "BSA…", bravePresent, braveFeedback),
      h("div", { class: "hint", text: t("Optional: web search for the engines that don't have it (Anthropic's models search on their own).") }),
      braveFeedback,
      h("div", { class: "row" },
        h("label", { text: t("Memory") }),
        h("button", { text: t("Open memory folder"), onclick: () => void Bridge.openDataFolder("memory") }),
        h("span", { class: "hint", text: t("notes.md is what ARIA remembers; history.jsonl, the log of finished chats.") }),
      ),
      feedback,
    );
  }

  await draw();
  return section;
}

// ── Grok Bots ─────────────────────────────────────────────────────────────────

/** "14:05" today, "03/10 14:05" before. `at` is the log's local "YYYY-MM-DD HH:MM:SS". */
function shortTime(at: string | undefined): string {
  if (!at) return "";
  const [date, time] = at.split(" ");
  const now = new Date();
  const today = `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}-${String(now.getDate()).padStart(2, "0")}`;
  const hm = (time ?? "").slice(0, 5);
  if (date === today) return hm;
  const [, m, d] = date.split("-");
  return `${d}/${m} ${hm}`;
}

/** Last ~20 permission requests from the Bots: who, which tool, what was decided, when. Read-only. */
function botHistory(colorOf: (bot: string) => string): { el: HTMLElement; refresh(): Promise<void> } {
  const rows = h("div", { class: "bot-history" });
  const refreshBtn = h("button", { text: t("Refresh") });
  const el = h("div", { style: "display:flex;flex-direction:column;gap:8px" },
    h("div", { class: "row" }, h("label", { text: t("Recent permissions") }), h("span", { class: "spacer" }), refreshBtn),
    rows,
  );
  async function refresh() {
    const entries: BotApproval[] = await loadBotApprovals();
    clear(rows);
    if (entries.length === 0) {
      rows.append(h("div", { class: "hint", text: t("No Bot has asked for permission yet.") }));
      return;
    }
    for (const e of entries) {
      const verdict = DECISION_LABELS[e.decision];
      rows.append(h("div", { class: "entry", title: e.target },
        dot(colorOf(e.bot)),
        h("span", { class: "who", text: e.name }),
        h("span", { class: "tool", text: e.tool }),
        h("span", { class: "target", text: e.target }),
        h("span", { class: "verdict", style: `color:${verdict.color};background:${verdict.color}24`, text: t(verdict.text) }),
        h("time", { text: shortTime(e.at) }),
      ));
    }
  }
  refreshBtn.addEventListener("click", () => void refresh());
  return { el, refresh };
}

export async function grokBotsSection(): Promise<HTMLElement> {
  const list = h("div", { style: "display:flex;flex-direction:column;gap:8px" });
  const form = h("div", { style: "display:flex;flex-direction:column;gap:8px" });
  const feedback = h("div", {});
  const colors = new Map<string, string>();
  const history = botHistory((bot) => colors.get(bot) ?? "#6b7079");

  async function drawList() {
    const bots: GrokBotStatus[] = (await Bridge.grokbotList()) ?? [];
    colors.clear();
    for (const b of bots) colors.set(b.id, b.color);
    clear(list);
    if (bots.length === 0) {
      list.append(h("div", { class: "hint", text: t("You haven't connected any Bot yet.") }));
    }
    for (const b of bots) {
      const test = h("button", { text: t("Test") });
      test.addEventListener("click", async () => {
        clear(feedback);
        test.disabled = true;
        try {
          // Read by the Bot, not shown here: it stays in the language its instructions use.
          const msg = await Bridge.grokbotSend(
            b.id,
            `Prueba de conexión desde ARIA. Avísame en la isla con --status done "Conectado y listo".`,
          );
          feedback.append(notice("ok", msg));
        } catch (err) {
          feedback.append(notice("err", errText(err)));
        } finally {
          test.disabled = false;
        }
      });
      const copy = h("button", { text: t("Copy instructions") });
      copy.addEventListener("click", async () => {
        clear(feedback);
        try {
          const text = await Bridge.grokbotInstructions(b.id);
          await navigator.clipboard.writeText(text);
          feedback.append(notice("ok", t("Copied. Paste them in Grok Bot → {name} → Bot settings → Description.", { name: b.name })));
        } catch (err) {
          feedback.append(notice("err", errText(err)));
        }
      });
      list.append(h("div", { class: "row" },
        dot(b.color),
        h("span", { style: "font-size:12.5px;min-width:96px", text: b.name }),
        h("span", { class: "hint", style: "flex:1 1 auto;min-width:0", text: b.hasKey ? t("webhook ready") : t("key missing") }),
        test,
        copy,
        h("button", { text: t("Edit"), onclick: () => drawForm(b) }),
        h("button", {
          class: "danger",
          text: t("Remove"),
          onclick: async () => {
            try {
              await Bridge.grokbotRemove(b.id);
              void drawList();
            } catch (err) {
              feedback.append(notice("err", errText(err)));
            }
          },
        }),
      ));
    }
  }

  function drawForm(edit?: GrokBotStatus) {
    clear(form);
    const name = h("input", { type: "text", value: edit?.name ?? "", placeholder: t("Same as in Grok Bot, e.g. Researcher"), style: "flex:1 1 auto;min-width:0" }) as HTMLInputElement;
    const color = h("input", { type: "color", value: edit?.color ?? "#38BDF8", style: "width:44px;padding:0" }) as HTMLInputElement;
    // A new Aegon, Aerys or Daemond starts with the family colour, until the picker is touched.
    let colorTouched = !!edit;
    color.addEventListener("input", () => {
      colorTouched = true;
    });
    name.addEventListener("input", () => {
      const def = colorTouched ? null : defaultBotColor(name.value);
      if (def) color.value = def.toLowerCase();
    });
    const url = h("input", { type: "text", value: edit?.url ?? "", placeholder: "POST to: https://…", style: "flex:1 1 auto;min-width:0", spellcheck: "false" }) as HTMLInputElement;
    const key = h("input", {
      type: "password",
      placeholder: edit?.hasKey ? stored() : t("the routine's key"),
      style: "flex:1 1 auto;min-width:0",
      autocomplete: "off",
    }) as HTMLInputElement;
    const saveBtn = h("button", { class: "primary", text: edit ? t("Save changes") : t("Connect Bot") });
    saveBtn.addEventListener("click", async () => {
      clear(feedback);
      saveBtn.disabled = true;
      try {
        const bot = await Bridge.grokbotSave({
          previous: edit?.id, name: name.value, color: color.value, url: url.value, key: key.value,
        });
        feedback.append(notice("ok", t("{name} connected. Now click “Copy instructions” and paste them in the Bot's description.", { name: bot.name })));
        drawForm();
        void drawList();
      } catch (err) {
        feedback.append(notice("err", errText(err)));
      } finally {
        saveBtn.disabled = false;
      }
    });
    form.append(
      h("div", { class: "row" }, h("label", { text: t("Name") }), name, color),
      h("div", { class: "row" }, h("label", { text: "Webhook" }), url),
      h("div", { class: "row" }, h("label", { text: t("Key") }), key),
      h("div", { class: "row" }, saveBtn, edit ? h("button", { text: t("Cancel"), onclick: () => drawForm() }) : h("span")),
    );
  }

  const steps = h("ol", { class: "hint", style: "margin:0;padding-left:18px;display:flex;flex-direction:column;gap:4px" },
    h("li", { text: t("In Grok Bot, tell your Bot: “Create a routine called ARIA Tasks, triggered by webhook, that does what the body's message field says.”") }),
    h("li", { text: t("Open the routine (Bot name → Tasks → the routine) and copy “POST to” and “key”.") }),
    h("li", { text: t("Paste them below with the same Bot name and click “Connect Bot”.") }),
    h("li", { text: t("Click “Copy instructions” and paste them in the Bot's Bot settings → Description: that's how it knows to alert you in the island.") }),
    h("li", { text: t("In Grok Bot → Settings → Computer, set “Execution on this computer” to ask or always allow.") }),
  );

  await drawList();
  drawForm();
  await history.refresh();
  void onEvent("settings-changed", () => void drawList().then(history.refresh));
  // The window lives hidden between visits: catch up each time it comes back.
  window.addEventListener("focus", () => void history.refresh());
  return h(
    "section",
    {},
    h("h2", {}, h("span", { text: t("My Grok Bots") })),
    h("div", {
      class: "hint",
      text: t("Your Grok Bot teammates (Cursor), with their cloud computer, memory and plugins. ARIA sends them tasks through a routine's webhook, and they alert you in the island by running a command on this PC. In the island chat, type “@Name task” to send something straight to a Bot."),
    }),
    steps,
    list,
    feedback,
    form,
    history.el,
  );
}

// ── Connections (MCP) ─────────────────────────────────────────────────────────

interface Recommended {
  name: string;
  /** English key (N_), or a name that stays as it is. */
  title: string;
  why: string;
  config: McpServerConfig;
  /** Values the user must type, stored as secrets. */
  secrets?: { field: string; label: string; placeholder: string }[];
}

const RECOMMENDED: Recommended[] = [
  { name: "playwright", title: N_("Playwright browser"), why: N_("Drives a real browser: opens pages, clicks, fills in forms, takes screenshots."),
    config: { command: "npx", args: ["-y", "@playwright/mcp@latest"] } },
  { name: "windows", title: N_("Windows desktop"), why: N_("Controls apps and the Windows desktop: windows, clicks, keyboard, clipboard. Needs uv (Python)."),
    config: { command: "uvx", args: ["windows-mcp"] } },
  { name: "filesystem", title: N_("Files"), why: N_("Advanced file operations inside your user folder."),
    config: { command: "npx", args: ["-y", "@modelcontextprotocol/server-filesystem", "${userHome}"] } },
  { name: "github", title: "GitHub", why: N_("Issues, pull requests, code search and more, with GitHub's official server."),
    config: { url: "https://api.githubcopilot.com/mcp/" },
    secrets: [{ field: "header:Authorization", label: "Authorization", placeholder: "Bearer ghp_…" }] },
  { name: "memory", title: N_("Knowledge graph"), why: N_("A structured long-term memory ARIA can look up."),
    config: { command: "npx", args: ["-y", "@modelcontextprotocol/server-memory"] } },
  { name: "fetch", title: "Fetch", why: N_("Turns any web page into clean Markdown. Needs uv (Python)."),
    config: { command: "uvx", args: ["mcp-server-fetch"] } },
];

function splitArgs(line: string): string[] {
  const out: string[] = [];
  for (const m of line.matchAll(/"([^"]*)"|(\S+)/g)) out.push(m[1] ?? m[2]);
  return out;
}

function stateColor(s: McpStatus["state"]): string {
  return { connected: "#22c55e", connecting: "#f5a524", disabled: "#6b7280", error: "#f4505e" }[s];
}

const STATE_LABELS = labels<McpStatus["state"]>({
  connected: N_("Connected"), connecting: N_("Connecting"), disabled: N_("Off"), error: N_("Error"),
});

export async function connectionsSection(): Promise<HTMLElement> {
  const list = h("div", { style: "display:flex;flex-direction:column;gap:8px" });
  const form = h("div", { style: "display:flex;flex-direction:column;gap:8px" });
  const feedback = h("div", {});
  const section = h(
    "section",
    {},
    h("h2", {}, h("span", { text: t("Connections (MCP)") })),
    h("div", {
      class: "hint",
      text: t("Connect ARIA to anything that speaks the Model Context Protocol. Each server's tools become ARIA's; the ones that change something ask you first. Keys you type here go to the Credential Manager: the config file only keeps a placeholder."),
    }),
    list,
    feedback,
    form,
  );

  async function drawList() {
    const statuses = (await Bridge.mcpList()) ?? [];
    clear(list);
    if (statuses.length === 0) {
      list.append(h("div", { class: "hint", text: t("No connections yet. Add one of the recommended servers below, or import yours from Cursor.") }));
    }
    for (const st of statuses) {
      const isSkill = st.name.startsWith("skill-");
      const sw = h("button", { class: st.state === "disabled" ? "switch" : "switch on" });
      sw.addEventListener("click", async () => {
        try {
          await Bridge.mcpSetEnabled(st.name, sw.classList.contains("on") ? false : true);
        } catch (err) {
          feedback.append(notice("err", errText(err)));
        }
      });
      const info =
        st.state === "connected"
          ? t("Tools: {count}", { count: st.tools.length })
          : st.state === "error" ? (st.error ?? STATE_LABELS.error) : STATE_LABELS[st.state];
      const row = h("div", { class: "row" },
        isSkill ? h("span", { style: "width:34px" }) : sw,
        dot(stateColor(st.state)),
        h("span", { style: "font-size:12.5px;min-width:96px", text: st.name }),
        h("span", { class: "hint", style: "flex:1 1 auto;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap", text: `${st.transport} · ${info}`, title: st.tools.join(", ") || info }),
      );
      if (!isSkill) {
        row.append(h("button", {
          class: "danger",
          text: t("Remove"),
          onclick: async () => {
            try {
              await Bridge.mcpRemove(st.name);
            } catch (err) {
              feedback.append(notice("err", errText(err)));
            }
          },
        }));
      }
      list.append(row);
    }
  }

  function drawForm(prefill?: Recommended) {
    clear(form);
    const name = h("input", { type: "text", value: prefill?.name ?? "", placeholder: t("name (letters, digits, - _)"), style: "width:160px" }) as HTMLInputElement;
    const kind = h("select", {}) as HTMLSelectElement;
    kind.append(h("option", { value: "stdio", text: t("Command") }), h("option", { value: "http", text: "URL" }));
    kind.value = prefill?.config.url ? "http" : "stdio";
    const command = h("input", {
      type: "text",
      value: prefill?.config.command ? [prefill.config.command, ...(prefill.config.args ?? [])].map((a) => (a.includes(" ") ? `"${a}"` : a)).join(" ") : "",
      placeholder: "npx -y @scope/server-name …",
      style: "flex:1 1 auto;min-width:0",
      spellcheck: "false",
    }) as HTMLInputElement;
    const url = h("input", { type: "text", value: prefill?.config.url ?? "", placeholder: "https://…/mcp", style: "flex:1 1 auto;min-width:0", spellcheck: "false" }) as HTMLInputElement;
    const env = h("textarea", {
      rows: "2",
      placeholder: t("One per line: NAME=value (keys and tokens go to the Credential Manager)"),
      style: "flex:1 1 auto;min-width:0;font:inherit;font-size:12px",
      spellcheck: "false",
    }) as HTMLTextAreaElement;
    const secretInputs = (prefill?.secrets ?? []).map((sec) => ({
      field: sec.field,
      input: h("input", { type: "password", placeholder: sec.placeholder, style: "flex:1 1 auto;min-width:0", autocomplete: "off" }) as HTMLInputElement,
      label: sec.label,
    }));
    const askAll = h("button", { class: "switch" });
    askAll.addEventListener("click", () => askAll.classList.toggle("on"));

    const cmdRow = h("div", { class: "row" }, h("label", { text: t("Command") }), command);
    const urlRow = h("div", { class: "row" }, h("label", { text: "URL" }), url);
    const envRow = h("div", { class: "row" }, h("label", { text: t("Environment") }), env);
    const syncKind = () => {
      const http = kind.value === "http";
      cmdRow.style.display = http ? "none" : "";
      envRow.style.display = http ? "none" : "";
      urlRow.style.display = http ? "" : "none";
    };
    kind.addEventListener("change", syncKind);
    syncKind();

    const saveBtn = h("button", { class: "primary", text: t("Save connection") });
    saveBtn.addEventListener("click", async () => {
      clear(feedback);
      const config: McpServerConfig = {};
      const secrets: Record<string, string> = {};
      if (kind.value === "http") {
        config.url = url.value.trim();
      } else {
        const parts = splitArgs(command.value.trim());
        config.command = parts[0];
        config.args = parts.slice(1);
        const vars: Record<string, string> = {};
        for (const line of env.value.split("\n")) {
          const i = line.indexOf("=");
          if (i > 0) vars[line.slice(0, i).trim()] = line.slice(i + 1).trim();
        }
        if (Object.keys(vars).length) config.env = vars;
      }
      for (const s of secretInputs) {
        const v = s.input.value.trim();
        if (!v) continue;
        secrets[s.field] = v;
        const [where, field] = s.field.split(":");
        if (where === "header") config.headers = { ...(config.headers ?? {}), [field]: "" };
        else config.env = { ...(config.env ?? {}), [field]: "" };
      }
      if (askAll.classList.contains("on")) config.askAll = true;
      saveBtn.disabled = true;
      try {
        await Bridge.mcpSave(name.value.trim(), config, secrets);
        feedback.append(notice("ok", t("Saved. Connecting to {name}…", { name: name.value.trim() })));
        drawForm();
      } catch (err) {
        feedback.append(notice("err", errText(err)));
      } finally {
        saveBtn.disabled = false;
      }
    });

    const recRow = h("div", { style: "display:flex;flex-wrap:wrap;gap:6px" });
    for (const rec of RECOMMENDED) {
      recRow.append(h("button", { text: t(rec.title), title: t(rec.why), onclick: () => drawForm(rec) }));
    }
    const importBtn = h("button", { text: t("Import from Cursor / Claude Code…"), onclick: () => void showImport() });

    form.append(
      h("div", { class: "hint", text: t("Recommended: one click fills in the form, then Save.") }),
      recRow,
      prefill ? h("div", { class: "hint", text: t(prefill.why) }) : h("span"),
      h("div", { class: "row" }, h("label", { text: t("Name") }), name, kind),
      cmdRow,
      urlRow,
      envRow,
      ...secretInputs.map((s) => h("div", { class: "row" }, h("label", { text: s.label }), s.input)),
      h("div", { class: "row" },
        h("label", { text: t("Ask for everything") }),
        askAll,
        h("span", { class: "hint", text: t("Also asks before tools that say they only read.") }),
      ),
      h("div", { class: "row" }, saveBtn, importBtn, h("button", { text: t("Reconnect all"), onclick: () => void Bridge.mcpReconnect() })),
    );
  }

  async function showImport() {
    clear(feedback);
    const candidates: McpImportCandidate[] = (await Bridge.mcpImportPreview()) ?? [];
    if (candidates.length === 0) {
      feedback.append(notice("warn", t("No MCP servers in ~/.cursor/mcp.json or ~/.claude.json.")));
      return;
    }
    const picks = new Set<string>();
    const box = h("div", { style: "display:flex;flex-direction:column;gap:6px" });
    for (const c of candidates) {
      const sw = h("button", { class: "switch" });
      if (c.exists) sw.setAttribute("disabled", "");
      sw.addEventListener("click", () => {
        const on = !sw.classList.contains("on");
        sw.classList.toggle("on", on);
        if (on) picks.add(c.name);
        else picks.delete(c.name);
      });
      box.append(h("div", { class: "row" },
        sw,
        h("span", { style: "font-size:12.5px;min-width:96px", text: c.name }),
        h("span", { class: "hint", style: "flex:1 1 auto;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap", text: `${c.source} · ${c.exists ? t("already added") : c.summary}` }),
      ));
    }
    const go = h("button", { class: "primary", text: t("Import selected") });
    go.addEventListener("click", async () => {
      try {
        const n = await Bridge.mcpImportApply([...picks]);
        clear(feedback);
        feedback.append(notice("ok", t("Servers imported: {count}. Their keys moved to the Credential Manager.", { count: n })));
      } catch (err) {
        feedback.append(notice("err", errText(err)));
      }
    });
    feedback.append(
      h("div", { class: "hint", text: t("Pick the servers to copy. Nothing changes in Cursor's or Claude Code's files.") }),
      box,
      h("div", { class: "row" }, go, h("button", { text: t("Cancel"), onclick: () => clear(feedback) })),
    );
  }

  await drawList();
  drawForm();
  void onEvent<McpStatus[]>("mcp-changed", () => void drawList());
  return section;
}

// ── Skills ────────────────────────────────────────────────────────────────────

export async function skillsSection(): Promise<HTMLElement> {
  const list = h("div", { style: "display:flex;flex-direction:column;gap:8px" });
  const feedback = h("div", {});

  async function draw() {
    const skills: SkillInfo[] = (await Bridge.skillsList()) ?? [];
    clear(list);
    if (skills.length === 0) {
      list.append(h("div", { class: "hint", text: t("No skills yet. Ask ARIA to make itself one (“make yourself a tool that…”): you read its code in the island before it is installed.") }));
    }
    for (const sk of skills) {
      const sw = h("button", { class: sk.enabled ? "switch on" : "switch" });
      sw.addEventListener("click", async () => {
        const next = !sw.classList.contains("on");
        try {
          await Bridge.skillSetEnabled(sk.name, next);
          sw.classList.toggle("on", next);
        } catch (err) {
          feedback.append(notice("err", errText(err)));
        }
      });
      const tools = sk.kind === "mcp" ? t("MCP server") : sk.tools.map((x) => x.name).join(", ");
      list.append(h("div", { class: "row" },
        sw,
        h("span", { style: "font-size:12.5px;min-width:110px", text: sk.name }),
        h("span", { class: "hint", style: "flex:1 1 auto;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap", text: `${sk.description} · ${tools}`, title: sk.description }),
        h("button", {
          class: "danger",
          text: t("Remove"),
          onclick: async () => {
            try {
              await Bridge.skillRemove(sk.name);
              void draw();
            } catch (err) {
              feedback.append(notice("err", errText(err)));
            }
          },
        }),
      ));
    }
  }

  await draw();
  return h(
    "section",
    {},
    h("h2", {}, h("span", { text: t("Skills") })),
    h("div", { class: "hint", text: t("Small tools ARIA wrote for itself, each one installed after you approved its code.") }),
    list,
    h("div", { class: "row" },
      h("button", { text: t("Open skills folder"), onclick: () => void Bridge.openDataFolder("skills") }),
      h("button", { text: t("Refresh"), onclick: () => void draw() }),
    ),
    feedback,
  );
}

// ── Protected core ────────────────────────────────────────────────────────────

export async function coreSection(): Promise<HTMLElement> {
  const status = (await Bridge.coreStatus()) ?? { protected: [], tampered: [] };
  const ok = status.tampered.length === 0;
  return h(
    "section",
    {},
    h("h2", {}, dot(ok ? "#22c55e" : "#f5a524"), h("span", { text: t("Protected core") })),
    h("div", {
      class: "hint",
      text: t("ARIA can improve itself (skills, and changes to its own code in a git worktree you approve as a diff), but it can never change this. Only you edit it, by hand."),
    }),
    h("div", { class: "path", style: "white-space:pre-wrap", text: status.protected.join("\n") }),
    ok
      ? notice("ok", t("Verified at startup: identical to what this version was built with."))
      : notice("warn", t("Changed since this build: {files}. Self-evolution is blocked until you rebuild.", { files: status.tampered.join(", ") })),
    h("div", { class: "row" },
      h("button", { text: t("Open log folder"), onclick: () => void Bridge.openDataFolder("log") }),
      h("span", { class: "hint", text: t("coucou.log lists every tool ARIA used and whether you allowed it.") }),
    ),
  );
}
