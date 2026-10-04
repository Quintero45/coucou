// Settings for the assistant: provider, model and keys; the owner's Grok Bots;
// MCP connections; skills Mochi installed; the protected core. Every write here
// is a click in this window — keys go to the Credential Manager, never to disk.

import {
  Bridge, onEvent,
  type GrokBotStatus, type McpImportCandidate, type McpServerConfig, type McpStatus, type SkillInfo,
} from "../core/bridge";
import { AI_PROVIDERS, aiProvider, type Settings } from "../core/state";
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

const STORED = "••••••••••••  (guardada)";

/** Password field + Save / Remove for one Credential Manager key. */
function keyRow(label: string, key: string, placeholder: string, present: boolean, feedback: HTMLElement): HTMLElement {
  const status = dot(present ? "#22c55e" : "#f4505e");
  const field = h("input", {
    type: "password",
    placeholder: present ? STORED : placeholder,
    style: "flex:1 1 auto;min-width:0",
    autocomplete: "off",
    spellcheck: "false",
  }) as HTMLInputElement;
  const saveBtn = h("button", { class: "primary", text: "Guardar" });
  const removeBtn = h("button", { class: "danger", text: "Quitar" });
  removeBtn.style.display = present ? "" : "none";
  const set = (on: boolean) => {
    status.style.background = on ? "#22c55e" : "#f4505e";
    field.placeholder = on ? STORED : placeholder;
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
      feedback.append(notice("ok", "Guardada en el Administrador de credenciales de Windows. Nunca se escribe en disco."));
    } catch (err) {
      feedback.append(notice("err", `No se pudo guardar: ${errText(err)}`));
    }
  });
  removeBtn.addEventListener("click", async () => {
    clear(feedback);
    try {
      await Bridge.secretClear(key);
      set(false);
      feedback.append(notice("ok", "Clave quitada."));
    } catch (err) {
      feedback.append(notice("err", `No se pudo quitar: ${errText(err)}`));
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

/** The Cursor engine: the SDK bridge Coucou drives, downloaded on a click. */
async function cursorEngineRows(feedback: HTMLElement): Promise<HTMLElement[]> {
  const st = await Bridge.cursorStatus();
  const state = h("span", {
    class: "hint",
    text: st?.installed ? `Instalado${st.version ? ` (${st.version})` : ""}.` : "Sin instalar.",
  });
  const install = h("button", { class: st?.installed ? "" : "primary", text: st?.installed ? "Actualizar motor" : "Instalar motor" });
  install.addEventListener("click", async () => {
    clear(feedback);
    install.disabled = true;
    state.textContent = "Descargando de GitHub (cursor/sdk-bridge)…";
    try {
      const v = await Bridge.cursorInstall();
      state.textContent = `Instalado (${v}).`;
      feedback.append(notice("ok", "Motor de Cursor listo. Verificado con la suma SHA-256 publicada por Cursor."));
    } catch (err) {
      state.textContent = "Sin instalar.";
      feedback.append(notice("err", errText(err)));
    } finally {
      install.disabled = false;
    }
  });
  return [
    h("div", { class: "row" }, h("label", { text: "Motor" }), dot(st?.installed ? "#22c55e" : "#f4505e"), state, install),
    h("div", {
      class: "hint",
      text: "Mochi piensa con Grok a través de tu cuenta de Cursor, la misma de tus Bots de Grok. " +
        "1) En cursor.com/dashboard/integrations crea una «User API Key». 2) Pégala en «Clave de Cursor» y pulsa Guardar. " +
        "3) Pulsa «Instalar motor». 4) Escríbele a Mochi. " +
        "Las herramientas de Cursor quedan apagadas: Mochi solo actúa con las suyas, y cada acción te pide permiso en la isla.",
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
    head.append(dot(present ? p.color : "#f4505e"), h("span", { text: "Asistente" }));
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
        text: "Mochi responde con el motor que elijas aquí y puede actuar en esta PC con sus herramientas: todo lo que cambia algo te pide permiso primero, en la isla.",
      }),
      h("div", { class: "row" }, h("label", { text: "Motor de IA" }), provider),
    );

    if (p.key) {
      body.append(keyRow(p.id === "cursor" ? "Clave de Cursor" : "Clave de API", p.key, KEY_PLACEHOLDERS[p.id] ?? "…", present, feedback));
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
        h("div", { class: "row" }, h("label", { text: "URL del servidor" }), url),
        h("div", { class: "hint", text: `${p.name} corre en esta PC: sin clave, nada sale de tu equipo. Ábrelo antes de chatear.` }),
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
    const fetchBtn = h("button", { text: "Ver modelos" });
    fetchBtn.addEventListener("click", async () => {
      clear(feedback);
      fetchBtn.disabled = true;
      try {
        const models = await Bridge.modelsList(p.id);
        clear(datalist);
        for (const m of models) datalist.append(h("option", { value: m }));
        feedback.append(notice("ok", `${models.length} modelos disponibles: escribe en el campo para elegir uno.`));
      } catch (err) {
        feedback.append(notice("err", errText(err)));
      } finally {
        fetchBtn.disabled = false;
      }
    });
    body.append(h("div", { class: "row" }, h("label", { text: "Modelo" }), model, datalist, fetchBtn));
    if (p.id === "cursor") {
      body.append(h("div", { class: "hint", text: "«grok» elige solo el Grok más nuevo de tu cuenta." }));
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
        h("label", { text: "Herramientas" }),
        tools,
        h("span", { class: "hint", text: "Archivos, PowerShell, apps, web, tus integraciones, tus Bots de Grok y las conexiones MCP. Leer es libre; cada acción espera tu clic." }),
      ),
    );

    const braveFeedback = h("div", {});
    body.append(
      keyRow("Brave Search", "brave-api-key", "BSA…", bravePresent, braveFeedback),
      h("div", { class: "hint", text: "Opcional: búsqueda web para los motores que no la traen (los modelos de Anthropic buscan solos)." }),
      braveFeedback,
      h("div", { class: "row" },
        h("label", { text: "Memoria" }),
        h("button", { text: "Abrir carpeta de memoria", onclick: () => void Bridge.openDataFolder("memory") }),
        h("span", { class: "hint", text: "notes.md es lo que Mochi recuerda; history.jsonl, el registro de los chats terminados." }),
      ),
      feedback,
    );
  }

  await draw();
  return section;
}

// ── Grok Bots ─────────────────────────────────────────────────────────────────

export async function grokBotsSection(): Promise<HTMLElement> {
  const list = h("div", { style: "display:flex;flex-direction:column;gap:8px" });
  const form = h("div", { style: "display:flex;flex-direction:column;gap:8px" });
  const feedback = h("div", {});

  async function drawList() {
    const bots: GrokBotStatus[] = (await Bridge.grokbotList()) ?? [];
    clear(list);
    if (bots.length === 0) {
      list.append(h("div", { class: "hint", text: "Todavía no conectaste ningún Bot." }));
    }
    for (const b of bots) {
      const test = h("button", { text: "Probar" });
      test.addEventListener("click", async () => {
        clear(feedback);
        test.disabled = true;
        try {
          const msg = await Bridge.grokbotSend(
            b.id,
            `Prueba de conexión desde Coucou. Avísame en la isla con --status done "Conectado y listo".`,
          );
          feedback.append(notice("ok", msg));
        } catch (err) {
          feedback.append(notice("err", errText(err)));
        } finally {
          test.disabled = false;
        }
      });
      const copy = h("button", { text: "Copiar instrucciones" });
      copy.addEventListener("click", async () => {
        clear(feedback);
        try {
          const text = await Bridge.grokbotInstructions(b.id);
          await navigator.clipboard.writeText(text);
          feedback.append(notice("ok", `Copiadas. Pégalas en Grok Bot → ${b.name} → Bot settings → Description.`));
        } catch (err) {
          feedback.append(notice("err", errText(err)));
        }
      });
      list.append(h("div", { class: "row" },
        dot(b.color),
        h("span", { style: "font-size:12.5px;min-width:96px", text: b.name }),
        h("span", { class: "hint", style: "flex:1 1 auto;min-width:0", text: b.hasKey ? "webhook listo" : "falta la clave" }),
        test,
        copy,
        h("button", { text: "Editar", onclick: () => drawForm(b) }),
        h("button", {
          class: "danger",
          text: "Quitar",
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
    const name = h("input", { type: "text", value: edit?.name ?? "", placeholder: "Igual que en Grok Bot, p. ej. Investigador", style: "flex:1 1 auto;min-width:0" }) as HTMLInputElement;
    const color = h("input", { type: "color", value: edit?.color ?? "#38BDF8", style: "width:44px;padding:0" }) as HTMLInputElement;
    const url = h("input", { type: "text", value: edit?.url ?? "", placeholder: "POST to: https://…", style: "flex:1 1 auto;min-width:0", spellcheck: "false" }) as HTMLInputElement;
    const key = h("input", {
      type: "password",
      placeholder: edit?.hasKey ? STORED : "key de la rutina",
      style: "flex:1 1 auto;min-width:0",
      autocomplete: "off",
    }) as HTMLInputElement;
    const saveBtn = h("button", { class: "primary", text: edit ? "Guardar cambios" : "Conectar Bot" });
    saveBtn.addEventListener("click", async () => {
      clear(feedback);
      saveBtn.disabled = true;
      try {
        const bot = await Bridge.grokbotSave({
          previous: edit?.id, name: name.value, color: color.value, url: url.value, key: key.value,
        });
        feedback.append(notice("ok", `${bot.name} conectado. Ahora pulsa «Copiar instrucciones» y pégalas en la descripción del Bot.`));
        drawForm();
        void drawList();
      } catch (err) {
        feedback.append(notice("err", errText(err)));
      } finally {
        saveBtn.disabled = false;
      }
    });
    form.append(
      h("div", { class: "row" }, h("label", { text: "Nombre" }), name, color),
      h("div", { class: "row" }, h("label", { text: "Webhook" }), url),
      h("div", { class: "row" }, h("label", { text: "Clave" }), key),
      h("div", { class: "row" }, saveBtn, edit ? h("button", { text: "Cancelar", onclick: () => drawForm() }) : h("span")),
    );
  }

  const steps = h("ol", { class: "hint", style: "margin:0;padding-left:18px;display:flex;flex-direction:column;gap:4px" },
    h("li", { text: "En Grok Bot, escríbele a tu Bot: «Crea una rutina llamada Tareas de Coucou que se active por webhook y haga lo que diga el campo message del cuerpo»." }),
    h("li", { text: "Abre la rutina (nombre del Bot → Tasks → la rutina) y copia «POST to» y «key»." }),
    h("li", { text: "Pégalos abajo con el mismo nombre del Bot y pulsa «Conectar Bot»." }),
    h("li", { text: "Pulsa «Copiar instrucciones» y pégalas en Bot settings → Description del Bot: así sabe avisarte en la isla." }),
    h("li", { text: "En Grok Bot → Settings → Computer, deja «Execution on this computer» en preguntar o permitir siempre." }),
  );

  await drawList();
  drawForm();
  void onEvent("settings-changed", () => void drawList());
  return h(
    "section",
    {},
    h("h2", {}, h("span", { text: "Mis Bots de Grok" })),
    h("div", {
      class: "hint",
      text: "Tus compañeros de Grok Bot (Cursor), con su computadora en la nube, memoria y plugins. Coucou les manda tareas por el webhook de una rutina, " +
        "y ellos te avisan en la isla ejecutando un comando en esta PC. En el chat de la isla, escribe «@Nombre tarea» para mandarle algo directo a un Bot.",
    }),
    steps,
    list,
    feedback,
    form,
  );
}

// ── Connections (MCP) ─────────────────────────────────────────────────────────

interface Recommended {
  name: string;
  title: string;
  why: string;
  config: McpServerConfig;
  /** Values the user must type, stored as secrets. */
  secrets?: { field: string; label: string; placeholder: string }[];
}

const RECOMMENDED: Recommended[] = [
  { name: "playwright", title: "Navegador Playwright", why: "Maneja un navegador de verdad: abre páginas, hace clic, rellena formularios, toma capturas.",
    config: { command: "npx", args: ["-y", "@playwright/mcp@latest"] } },
  { name: "windows", title: "Escritorio de Windows", why: "Controla apps y el escritorio de Windows: ventanas, clics, teclado, portapapeles. Necesita uv (Python).",
    config: { command: "uvx", args: ["windows-mcp"] } },
  { name: "filesystem", title: "Archivos", why: "Operaciones de archivos avanzadas dentro de tu carpeta de usuario.",
    config: { command: "npx", args: ["-y", "@modelcontextprotocol/server-filesystem", "${userHome}"] } },
  { name: "github", title: "GitHub", why: "Issues, pull requests, búsqueda de código y más, con el servidor oficial de GitHub.",
    config: { url: "https://api.githubcopilot.com/mcp/" },
    secrets: [{ field: "header:Authorization", label: "Authorization", placeholder: "Bearer ghp_…" }] },
  { name: "memory", title: "Grafo de conocimiento", why: "Una memoria estructurada a largo plazo que Mochi puede consultar.",
    config: { command: "npx", args: ["-y", "@modelcontextprotocol/server-memory"] } },
  { name: "fetch", title: "Fetch", why: "Convierte cualquier página web en Markdown limpio. Necesita uv (Python).",
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

const STATE_LABELS: Record<McpStatus["state"], string> = {
  connected: "conectado", connecting: "conectando", disabled: "apagado", error: "error",
};

export async function connectionsSection(): Promise<HTMLElement> {
  const list = h("div", { style: "display:flex;flex-direction:column;gap:8px" });
  const form = h("div", { style: "display:flex;flex-direction:column;gap:8px" });
  const feedback = h("div", {});
  const section = h(
    "section",
    {},
    h("h2", {}, h("span", { text: "Conexiones (MCP)" })),
    h("div", {
      class: "hint",
      text: "Conecta a Mochi con cualquier cosa que hable el Model Context Protocol. Las herramientas de cada servidor pasan a ser de Mochi; las que cambian algo te preguntan primero. Las claves que escribas aquí van al Administrador de credenciales: el archivo de configuración solo guarda un marcador.",
    }),
    list,
    feedback,
    form,
  );

  async function drawList() {
    const statuses = (await Bridge.mcpList()) ?? [];
    clear(list);
    if (statuses.length === 0) {
      list.append(h("div", { class: "hint", text: "Sin conexiones todavía. Añade uno de los servidores recomendados de abajo, o importa los tuyos de Cursor." }));
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
        st.state === "connected" ? `${st.tools.length} herramientas` : st.state === "error" ? (st.error ?? "error") : STATE_LABELS[st.state];
      const row = h("div", { class: "row" },
        isSkill ? h("span", { style: "width:34px" }) : sw,
        dot(stateColor(st.state)),
        h("span", { style: "font-size:12.5px;min-width:96px", text: st.name }),
        h("span", { class: "hint", style: "flex:1 1 auto;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap", text: `${st.transport} · ${info}`, title: st.tools.join(", ") || info }),
      );
      if (!isSkill) {
        row.append(h("button", {
          class: "danger",
          text: "Quitar",
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
    const name = h("input", { type: "text", value: prefill?.name ?? "", placeholder: "nombre (letras, números, - _)", style: "width:160px" }) as HTMLInputElement;
    const kind = h("select", {}) as HTMLSelectElement;
    kind.append(h("option", { value: "stdio", text: "Comando" }), h("option", { value: "http", text: "URL" }));
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
      placeholder: "Una por línea: NOMBRE=valor (las claves y tokens van al Administrador de credenciales)",
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

    const cmdRow = h("div", { class: "row" }, h("label", { text: "Comando" }), command);
    const urlRow = h("div", { class: "row" }, h("label", { text: "URL" }), url);
    const envRow = h("div", { class: "row" }, h("label", { text: "Entorno" }), env);
    const syncKind = () => {
      const http = kind.value === "http";
      cmdRow.style.display = http ? "none" : "";
      envRow.style.display = http ? "none" : "";
      urlRow.style.display = http ? "" : "none";
    };
    kind.addEventListener("change", syncKind);
    syncKind();

    const saveBtn = h("button", { class: "primary", text: "Guardar conexión" });
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
        feedback.append(notice("ok", `Guardado. Conectando con ${name.value.trim()}…`));
        drawForm();
      } catch (err) {
        feedback.append(notice("err", errText(err)));
      } finally {
        saveBtn.disabled = false;
      }
    });

    const recRow = h("div", { style: "display:flex;flex-wrap:wrap;gap:6px" });
    for (const rec of RECOMMENDED) {
      recRow.append(h("button", { text: rec.title, title: rec.why, onclick: () => drawForm(rec) }));
    }
    const importBtn = h("button", { text: "Importar de Cursor / Claude Code…", onclick: () => void showImport() });

    form.append(
      h("div", { class: "hint", text: "Recomendados: un clic rellena el formulario, luego Guardar." }),
      recRow,
      prefill ? h("div", { class: "hint", text: prefill.why }) : h("span"),
      h("div", { class: "row" }, h("label", { text: "Nombre" }), name, kind),
      cmdRow,
      urlRow,
      envRow,
      ...secretInputs.map((s) => h("div", { class: "row" }, h("label", { text: s.label }), s.input)),
      h("div", { class: "row" },
        h("label", { text: "Preguntar todo" }),
        askAll,
        h("span", { class: "hint", text: "Pregunta también antes de las herramientas que dicen que solo leen." }),
      ),
      h("div", { class: "row" }, saveBtn, importBtn, h("button", { text: "Reconectar todo", onclick: () => void Bridge.mcpReconnect() })),
    );
  }

  async function showImport() {
    clear(feedback);
    const candidates: McpImportCandidate[] = (await Bridge.mcpImportPreview()) ?? [];
    if (candidates.length === 0) {
      feedback.append(notice("warn", "No hay servidores MCP en ~/.cursor/mcp.json ni en ~/.claude.json."));
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
        h("span", { class: "hint", style: "flex:1 1 auto;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap", text: `${c.source} · ${c.exists ? "ya añadido" : c.summary}` }),
      ));
    }
    const go = h("button", { class: "primary", text: "Importar seleccionados" });
    go.addEventListener("click", async () => {
      try {
        const n = await Bridge.mcpImportApply([...picks]);
        clear(feedback);
        feedback.append(notice("ok", `${n} servidor(es) importados. Sus claves pasaron al Administrador de credenciales.`));
      } catch (err) {
        feedback.append(notice("err", errText(err)));
      }
    });
    feedback.append(
      h("div", { class: "hint", text: "Elige los servidores a copiar. No se cambia nada en los archivos de Cursor ni de Claude Code." }),
      box,
      h("div", { class: "row" }, go, h("button", { text: "Cancelar", onclick: () => clear(feedback) })),
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
      list.append(h("div", { class: "hint", text: "Sin habilidades todavía. Pídele a Mochi que se cree una («créate una herramienta que…»): lees su código en la isla antes de que se instale." }));
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
      const tools = sk.kind === "mcp" ? "servidor MCP" : sk.tools.map((t) => t.name).join(", ");
      list.append(h("div", { class: "row" },
        sw,
        h("span", { style: "font-size:12.5px;min-width:110px", text: sk.name }),
        h("span", { class: "hint", style: "flex:1 1 auto;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap", text: `${sk.description} · ${tools}`, title: sk.description }),
        h("button", {
          class: "danger",
          text: "Quitar",
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
    h("h2", {}, h("span", { text: "Habilidades" })),
    h("div", { class: "hint", text: "Herramientas pequeñas que Mochi escribió para sí mismo, cada una instalada después de que aprobaste su código." }),
    list,
    h("div", { class: "row" },
      h("button", { text: "Abrir carpeta de habilidades", onclick: () => void Bridge.openDataFolder("skills") }),
      h("button", { text: "Actualizar", onclick: () => void draw() }),
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
    h("h2", {}, dot(ok ? "#22c55e" : "#f5a524"), h("span", { text: "Núcleo protegido" })),
    h("div", {
      class: "hint",
      text: "Mochi puede mejorarse a sí mismo (habilidades y cambios en su propio código en un worktree de git que apruebas como diff), pero nunca puede cambiar esto. Solo tú lo editas, a mano.",
    }),
    h("div", { class: "path", style: "white-space:pre-wrap", text: status.protected.join("\n") }),
    ok
      ? notice("ok", "Verificado al arrancar: idéntico a lo que se usó para compilar esta versión.")
      : notice("warn", `Cambió desde esta compilación: ${status.tampered.join(", ")}. La autoevolución queda bloqueada hasta que recompiles.`),
    h("div", { class: "row" },
      h("button", { text: "Abrir carpeta del registro", onclick: () => void Bridge.openDataFolder("log") }),
      h("span", { class: "hint", text: "coucou.log lista cada herramienta que usó Mochi y si la permitiste." }),
    ),
  );
}
