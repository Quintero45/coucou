// Settings window — the place where anything that writes to disk is confirmed:
// the assistant and its connections, Grok Bots, agent hooks, pills, integrations.

import "./settings.css";
import { assistantSection, connectionsSection, coreSection, grokBotsSection, skillsSection } from "./assistant";
import { voiceSection } from "./voice";
import { saveSettingsMerged } from "../core/savesettings";
import { Bridge, onEvent, type HookStatus, type HookTarget } from "../core/bridge";
import { AGENT_PILLS, DEFAULT_SETTINGS, isHiddenAgent, type Settings } from "../core/state";
import { h, clear } from "../views/dom";

let settings: Settings = { ...DEFAULT_SETTINGS };
let version = "";

const root = document.getElementById("settings-root")!;

async function save() {
  await saveSettingsMerged(settings);
}

// ── Reusable bits ─────────────────────────────────────────────────────────────

function toggle(on: boolean, onChange: (v: boolean) => void): HTMLElement {
  const el = h("button", { class: on ? "switch on" : "switch", "aria-pressed": on });
  el.addEventListener("click", () => {
    const next = !el.classList.contains("on");
    el.classList.toggle("on", next);
    onChange(next);
  });
  return el;
}

function statusDot(ok: boolean): HTMLElement {
  return h("i", { class: "dot", style: `background:${ok ? "#22c55e" : "#f4505e"}` });
}

function renderDiff(text: string): HTMLElement {
  const box = h("div", { class: "diff" });
  for (const line of text.split("\n")) {
    const cls = line.startsWith("+") ? "add" : line.startsWith("-") ? "del" : "ctx";
    box.append(h("div", { class: cls, text: line }));
  }
  return box;
}

// ── Agent hooks sections (Claude Code, Cursor, Codex, Gemini CLI) ────────────

interface AgentDef {
  target: HookTarget;
  title: string;
  fileLabel: string;
  installedHint: string;
  idleHint: string;
  /** Cursor: the "approve shell & MCP from the island" option. */
  approvals?: boolean;
}

const AGENTS: AgentDef[] = [
  {
    target: "claude",
    title: "Claude Code",
    fileLabel: "settings.json",
    installedHint: "Coucou está conectado a tus sesiones de Claude Code. Herramientas, preguntas y permisos aparecen en la isla y puedes responderlos ahí.",
    idleHint: "Instala los hooks para ver tus sesiones de Claude Code en la isla y aprobar permisos sin dejar lo que estás haciendo.",
  },
  {
    target: "cursor",
    title: "Cursor IDE",
    fileLabel: "hooks.json",
    installedHint: "Coucou sigue a tu agente de Cursor: prompts, herramientas, ediciones con diff en vivo y la respuesta final aparecen en su pill, y sus preguntas se responden desde la isla.",
    idleHint: "Instala los hooks para seguir las sesiones del agente de Cursor en la isla, responder sus preguntas desde ahí y, si quieres, aprobar sus comandos de shell y MCP.",
    approvals: true,
  },
  {
    target: "codex",
    title: "Codex",
    fileLabel: "hooks.json",
    installedHint: "Coucou sigue tus sesiones de Codex y muestra sus permisos con Permitir / Rechazar.",
    idleHint: "Instala los hooks para seguir las sesiones de Codex en la isla y aprobar sus permisos desde ahí.",
  },
  {
    target: "gemini",
    title: "Gemini CLI",
    fileLabel: "settings.json",
    installedHint: "Coucou sigue tus sesiones de Gemini CLI en su propia pill.",
    idleHint: "Instala los hooks para seguir las sesiones de Gemini CLI en la isla.",
  },
];

function hooksSection(def: AgentDef, status: HookStatus): HTMLElement {
  const body = h("div", { style: "display:flex;flex-direction:column;gap:12px" });
  const section = h(
    "section",
    {},
    h("h2", {}, statusDot(status.installed), h("span", { text: def.title })),
    body,
  );
  // Cursor approvals: what the next install will write. Starts from what is installed.
  let approvals = status.approvals;

  const rebuild = async () => {
    const fresh = await Bridge.hooksStatus(def.target);
    if (fresh) Object.assign(status, fresh);
    approvals = status.approvals;
    clear(body);
    draw();
    const head = section.querySelector("h2")!;
    clear(head);
    head.append(statusDot(status.installed), h("span", { text: def.title }));
  };

  function draw() {
    body.append(
      h("div", { class: "hint", text: status.installed ? def.installedHint : def.idleHint }),
      h("div", { class: "row" },
        h("label", { text: def.fileLabel }),
        h("span", { class: "path", text: status.settingsPath }),
      ),
      h("div", { class: "row" },
        h("label", { text: "Relé" }),
        h("span", { class: "path", text: status.hookPath }),
        statusDot(status.hookReady),
      ),
    );

    if (!status.hookReady) {
      body.append(h("div", {
        class: "notice warn",
        text: "coucou-hook.exe todavía no está en su sitio. Reinicia Coucou; si sigue fallando, compílalo con `cargo build -p coucou-hook`.",
      }));
    } else if (status.installed && !status.upToDate) {
      body.append(h("div", {
        class: "notice warn",
        text: "Esta versión de Coucou trae hooks nuevos (por ejemplo, las preguntas del agente desde la isla). Pulsa «Reinstalar hooks…» para verlos y aplicarlos.",
      }));
    }

    if (def.approvals) {
      body.append(h("div", { class: "row" },
        h("label", { text: "Aprobaciones" }),
        toggle(approvals, (v) => { approvals = v; }),
        h("span", {
          class: "hint",
          text: "Pregúntame en la isla antes de que Cursor ejecute un comando de shell o MCP. Si no contesto, Cursor pregunta en su ventana: nunca un sí automático.",
        }),
      ));
    }

    const actions = h("div", { class: "row" });
    const install = h("button", {
      class: "primary",
      text: status.installed ? "Reinstalar hooks…" : "Instalar hooks…",
      onclick: () => showPreview(true),
    });
    // Writing hook commands that point at a relay which isn't there would give
    // every Claude Code session a broken hook and nothing to show for it.
    if (!status.hookReady) {
      install.disabled = true;
      install.title = "El relé todavía no está instalado.";
    }
    actions.append(install);
    if (status.installed) {
      actions.append(h("button", {
        class: "danger",
        text: "Desinstalar hooks…",
        onclick: () => showPreview(false),
      }));
    }
    body.append(actions);
  }

  async function showPreview(install: boolean) {
    let preview;
    const options = { approvals };
    try {
      preview = await Bridge.hooksPreview(install, def.target, options);
    } catch (err) {
      // An unreadable or invalid settings.json stops here rather than being
      // treated as empty and written over.
      clear(body);
      body.append(
        h("div", { class: "notice err", text: String(err).replace(/^Error:\s*/, "") }),
        h("div", { class: "row" }, h("button", {
          text: "Volver",
          onclick: () => { clear(body); draw(); },
        })),
      );
      return;
    }
    if (!preview) return;
    clear(body);
    body.append(
      h("div", {
        class: "hint",
        text: install
          ? `Esto es exactamente lo que cambiará en tu ${def.fileLabel}. Tus propios hooks no se tocan.`
          : "Esto quita solo las entradas de Coucou. Tus propios hooks no se tocan.",
      }),
      renderDiff(preview.diff),
      h("div", { class: "row" },
        h("span", { class: "path", text: `Copia de seguridad → ${preview.backup}` }),
      ),
    );
    const confirm = h("button", {
      class: install ? "primary" : "danger",
      text: install ? "Respaldar y escribir" : "Respaldar y quitar",
    });
    confirm.addEventListener("click", async () => {
      confirm.disabled = true;
      try {
        const backup = await Bridge.hooksApply(install, preview.fingerprint, def.target, options);
        clear(body);
        body.append(h("div", {
          class: "notice ok",
          text: `Hecho. El archivo anterior quedó en ${backup}. Abre una sesión nueva de ${def.title} para que tome los hooks.`,
        }));
        window.setTimeout(() => void rebuild(), 2600);
      } catch (err) {
        confirm.disabled = false;
        body.append(h("div", { class: "notice err", text: `No se pudo escribir: ${String(err)}` }));
      }
    });
    body.append(h("div", { class: "row" }, confirm, h("button", {
      text: "Cancelar",
      onclick: () => { clear(body); draw(); },
    })));
  }

  draw();
  return section;
}

// ── Integrations section ──────────────────────────────────────────────────────

interface IntegrationDef {
  id: string;
  name: string;
  color: string;
  /** Credential Manager keys, in the order they are shown. */
  fields: { key: string; label: string; placeholder: string; secret: boolean }[];
}

const INTEGRATIONS: IntegrationDef[] = [
  { id: "integration_stripe", name: "Stripe", color: "#0570DE",
    fields: [{ key: "stripe-api-key", label: "Clave secreta", placeholder: "sk_live_…", secret: true }] },
  { id: "integration_github", name: "GitHub", color: "#F4505E",
    fields: [{ key: "github-token", label: "Token", placeholder: "ghp_…", secret: true }] },
  { id: "integration_vercel", name: "Vercel", color: "#7C5CFF",
    fields: [{ key: "vercel-token", label: "Token", placeholder: "…", secret: true }] },
  { id: "integration_n8n", name: "n8n", color: "#F29B38",
    fields: [
      { key: "n8n-url", label: "URL de la instancia", placeholder: "https://n8n.example.com", secret: false },
      { key: "n8n-api-key", label: "Clave de API", placeholder: "…", secret: true },
    ] },
  { id: "integration_resend", name: "Resend", color: "#22C55E",
    fields: [{ key: "resend-api-key", label: "Clave de API", placeholder: "re_…", secret: true }] },
  { id: "integration_notion", name: "Notion", color: "#8C8C8C",
    fields: [{ key: "notion-api-key", label: "Token", placeholder: "ntn_…", secret: true }] },
  { id: "integration_calcom", name: "Cal.com", color: "#C9956A",
    fields: [{ key: "calcom-api-key", label: "Clave de API", placeholder: "cal_…", secret: true }] },
];

const MAX_ACTIVE = 4;
const STORED = "••••••••  (guardada)";

function integrationsSection(present: Record<string, boolean>): HTMLElement {
  const note = h("div", { class: "hint" });
  const list = h("div", { style: "display:flex;flex-direction:column;gap:14px" });

  // Hidden agent pills don't take a slot.
  const used = () =>
    settings.activeIntegrations.filter((id) => !isHiddenAgent(id, settings)).length;

  function updateNote() {
    note.textContent = `Elige hasta ${MAX_ACTIVE} pills para mostrar junto a Mochi (${used()}/${MAX_ACTIVE} en uso). Las claves se guardan en el Administrador de credenciales de Windows, nunca en disco.`;
  }

  for (const def of INTEGRATIONS) {
    const active = settings.activeIntegrations.includes(def.id);
    const sw = h("button", { class: active ? "switch on" : "switch" });
    sw.addEventListener("click", () => {
      const on = settings.activeIntegrations.includes(def.id);
      if (on) {
        settings.activeIntegrations = settings.activeIntegrations.filter((x) => x !== def.id);
      } else {
        if (used() >= MAX_ACTIVE) return;
        settings.activeIntegrations = [...settings.activeIntegrations, def.id];
      }
      sw.classList.toggle("on", !on);
      updateNote();
      void save();
    });

    const rows = h("div", { style: "display:flex;flex-direction:column;gap:6px;flex:1 1 auto;min-width:0" });
    for (const field of def.fields) {
      const input = h("input", {
        type: field.secret ? "password" : "text",
        placeholder: present[field.key] ? STORED : field.placeholder,
        autocomplete: "off",
        spellcheck: "false",
        style: "flex:1 1 auto;min-width:0",
      }) as HTMLInputElement;
      const saveBtn = h("button", { text: "Guardar" });
      const dotEl = statusDot(present[field.key] ?? false);
      saveBtn.addEventListener("click", async () => {
        const value = input.value.trim();
        try {
          await Bridge.secretSet(field.key, value);
          present[field.key] = value.length > 0;
          input.value = "";
          input.placeholder = value ? STORED : field.placeholder;
          dotEl.style.background = value ? "#22c55e" : "#f4505e";
        } catch {
          dotEl.style.background = "#f5a524";
        }
      });
      rows.append(
        h("div", { class: "row" },
          h("label", { style: "min-width:104px", text: field.label }),
          input, saveBtn, dotEl,
        ),
      );
    }

    list.append(
      h("div", { style: "display:flex;gap:12px;align-items:flex-start" },
        h("div", { style: "display:flex;align-items:center;gap:8px;min-width:132px;padding-top:4px" },
          sw,
          h("i", { class: "dot", style: `background:${def.color}` }),
          h("span", { style: "font-size:12.5px", text: def.name }),
        ),
        rows,
      ),
    );
  }

  updateNote();
  return h("section", {}, h("h2", {}, h("span", { text: "Integraciones" })), note, list);
}

// ── Active pills section ──────────────────────────────────────────────────────

/** Agent pills to declare, and which pill the island opens on. */
function pillsSection(): HTMLElement {
  const note = h("div", {
    class: "hint",
    text: "Un agente declarado conserva su pill (en reposo) cuando termina una sesión; uno no declarado aparece con su sesión y se va con ella. Las pills declaradas comparten los 4 huecos con las integraciones.",
  });
  const list = h("div", { style: "display:flex;flex-direction:column;gap:8px" });
  for (const agent of AGENT_PILLS) {
    const active = settings.activeIntegrations.includes(agent.id);
    const sw = h("button", { class: active ? "switch on" : "switch" });
    sw.addEventListener("click", () => {
      const on = settings.activeIntegrations.includes(agent.id);
      if (on) {
        settings.activeIntegrations = settings.activeIntegrations.filter((x) => x !== agent.id);
      } else {
        if (settings.activeIntegrations.length >= MAX_ACTIVE) return;
        settings.activeIntegrations = [...settings.activeIntegrations, agent.id];
      }
      sw.classList.toggle("on", !on);
      void save();
    });
    list.append(h("div", { class: "row" },
      sw,
      h("i", { class: "dot", style: `background:${agent.color}` }),
      h("span", { style: "font-size:12.5px", text: agent.name }),
    ));
  }

  const main = h("select", {}) as HTMLSelectElement;
  const choices: [string, string][] = [
    ["integration_claude", "VS Code (Claude Code)"],
    ...AGENT_PILLS.map((a) => [a.id, a.name] as [string, string]),
  ];
  for (const [id, label] of choices) main.append(h("option", { value: id, text: label }));
  main.value = settings.mainPill;
  main.addEventListener("change", () => {
    settings.mainPill = main.value;
    void save();
  });

  return h("section", {},
    h("h2", {}, h("span", { text: "Pills de agentes" })),
    note,
    h("div", { class: "row" }, h("label", { text: "Pill principal" }), main),
    list,
  );
}

// ── General section ───────────────────────────────────────────────────────────

function generalSection(): HTMLElement {
  const volume = h("input", {
    type: "range", min: "0", max: "0.2", step: "0.005",
    value: String(settings.soundVolume),
  }) as HTMLInputElement;
  volume.addEventListener("input", () => {
    settings.soundVolume = Number(volume.value);
    void save();
  });

  const autoClose = h("input", {
    type: "number", min: "5", max: "120", step: "1",
    value: String(Math.round(settings.autoCloseInterval)),
    style: "width:72px",
  }) as HTMLInputElement;
  autoClose.addEventListener("change", () => {
    settings.autoCloseInterval = Math.max(5, Math.min(120, Number(autoClose.value) || 15));
    autoClose.value = String(settings.autoCloseInterval);
    void save();
  });

  const screen = h("select", {}) as HTMLSelectElement;
  screen.append(
    h("option", { value: "primary", text: "Pantalla principal" }),
    h("option", { value: "cursor", text: "La pantalla donde está el ratón" }),
  );
  screen.value = settings.screen;
  screen.addEventListener("change", () => {
    settings.screen = screen.value as Settings["screen"];
    void save();
  });

  return h(
    "section",
    {},
    h("h2", {}, h("span", { text: "General" })),
    h("div", { class: "row" },
      h("label", { text: "Sonido" }),
      toggle(settings.soundEnabled, (v) => { settings.soundEnabled = v; void save(); }),
      volume,
    ),
    h("div", { class: "row" },
      h("label", { text: "Cierre automático" }),
      autoClose,
      h("span", { class: "hint", text: "segundos después de salir de la isla" }),
    ),
    h("div", { class: "row" },
      h("label", { text: "La isla vive en" }),
      screen,
    ),
    h("div", { class: "row" },
      h("label", { text: "Abrir al iniciar Windows" }),
      toggle(settings.autostart, (v) => { settings.autostart = v; void save(); }),
    ),
    h("div", { class: "row" },
      h("label", { text: "Atajos" }),
      toggle(settings.shortcutsEnabled, (v) => { settings.shortcutsEnabled = v; void save(); }),
      h("span", {
        class: "hint",
        text: "Ctrl+Alt+Espacio preguntar a Mochi · Ctrl+Alt+A ir al aviso · Ctrl+Alt+H mostrar / ocultar · Ctrl+Alt+M silenciar · Ctrl+Alt+] siguiente pill",
      }),
    ),
    h("div", { class: "row" },
      h("label", { text: "Agente de Cursor" }),
      toggle(settings.showCursorAgent, async (v) => {
        settings.showCursorAgent = v;
        await save();
        location.reload();
      }),
      h("span", {
        class: "hint",
        text: "El agente de Cursor IDE en la isla: lo que está haciendo, sus diffs y sus preguntas, que puedes responder desde aquí.",
      }),
    ),
    h("div", { class: "row" },
      h("label", { text: "Otros agentes" }),
      toggle(settings.showAgents, async (v) => {
        settings.showAgents = v;
        await save();
        location.reload();
      }),
      h("span", {
        class: "hint",
        text: "Claude Code (VS Code), Codex y Gemini CLI: sus pills, avisos y hooks. Apagado, la isla solo muestra a Mochi, tus Bots de Grok, el agente de Cursor y tus integraciones.",
      }),
    ),
  );
}

// ── Boot ──────────────────────────────────────────────────────────────────────

async function main() {
  const boot = await Bridge.boot();
  if (boot) {
    settings = { ...settings, ...boot.settings };
    version = boot.version;
  }
  const shownAgents = AGENTS.filter((def) =>
    def.target === "cursor" ? settings.showCursorAgent : settings.showAgents);
  const statuses: HookStatus[] = [];
  for (const def of shownAgents) {
    statuses.push((await Bridge.hooksStatus(def.target)) ?? {
      installed: false, settingsPath: "", hookPath: "", hookReady: false, approvals: false, upToDate: true,
    });
  }

  const ctx = { get: () => settings, save };
  const [assistant, bots, connections, skills, core] = await Promise.all([
    assistantSection(ctx),
    grokBotsSection(),
    connectionsSection(),
    skillsSection(),
    coreSection(),
  ]);

  const keys = [
    "stripe-api-key", "github-token", "vercel-token",
    "n8n-url", "n8n-api-key", "resend-api-key", "notion-api-key", "calcom-api-key",
  ];
  const present: Record<string, boolean> = {};
  for (const k of keys) present[k] = (await Bridge.secretPresent(k)) ?? false;

  clear(root);
  root.append(
    h("h1", {}, h("span", { text: "Coucou" }), h("span", { class: "version", text: version })),
    assistant,
    bots,
    voiceSection(settings.grokBots, settings.voices ?? {}, { get: () => settings, save }),
    connections,
    skills,
    ...shownAgents.map((def, i) => hooksSection(def, statuses[i])),
    ...(settings.showAgents ? [pillsSection()] : []),
    integrationsSection(present),
    generalSection(),
    core,
    h("div", {
      class: "hint",
      text: "Sin telemetría. Las peticiones de red solo van a los servicios que tú configuras.",
    }),
  );

  void onEvent<Settings>("settings-changed", (s) => {
    settings = { ...settings, ...s };
  });
}

void main();
