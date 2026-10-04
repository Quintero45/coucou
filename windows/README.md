<div align="center">

<img src="src-tauri/icons/128x128.png" width="96" alt="Coucou icon">

# Coucou for Windows

**Mochi doesn't get a notch on a PC — so it lives at the top of your screen instead.**

Approve Claude Code, Cursor and Codex permissions, watch your sessions work, drop a file, ask an assistant that can act on your PC, keep an eye on your services — without leaving what you're doing.

![Windows 10/11](https://img.shields.io/badge/Windows-10%2F11-0078D4?logo=windows)
![Tauri 2](https://img.shields.io/badge/Tauri-2-FFC131?logo=tauri&logoColor=black)
![Rust](https://img.shields.io/badge/Rust-backend-000?logo=rust)
![License: MIT](https://img.shields.io/badge/license-MIT-green)

</div>

<img src="screenshots/greeting.png" width="640" alt="Mochi waving hello at launch">

---

## Install

The downloadable installer is **temporarily unavailable**. Microsoft Defender
wrongly flags the unsigned installer as malware (`Trojan:Win32/Wacatac.H!ml`, a
machine-learning false positive). A report is under review at Microsoft, and the
installer will be published again once it is cleared and code-signed.

Until then, [build it yourself](#build-it-yourself): it takes a few minutes and
installs for the current user only — no admin prompt.

## Using it

<img src="screenshots/compact.png" width="292" alt="The compact island, with the integration pills as mini Mochis">
<img src="screenshots/overview.png" width="640" alt="The overview: the focused integration on the left, the other pills on the right">
<img src="screenshots/approval.png" width="640" alt="A Claude Code permission request, with Deny and Allow">
<img src="screenshots/chat.png" width="640" alt="Chatting with Claude from the island">
<img src="screenshots/drop.png" width="640" alt="Mochi turned into a box, waiting for a file">

| What you do | What happens |
|---|---|
| Move the mouse to the very top-centre of the screen | Mochi peeks out |
| Click the small island | It opens |
| Click Mochi | It gets annoyed. Three times in a row and it goes dizzy |
| Rest the pointer on Mochi for two seconds | Hearts |
| Drag a file onto the island | Mochi turns into a box, swallows it, then offers to answer questions about it |
| `Esc` | Closes the island |
| Tray icon | Open, Settings…, Pause, Quit |

Everything else happens on its own: a Claude Code permission request opens the
island with **Deny / Allow**, a finished session shows what it did, and
your integrations sit in the coloured pills next to Mochi.

## Claude Code

<img src="screenshots/settings.png" width="562" alt="The settings window">

Open **Settings… → Claude Code → Install hooks…**. You get the exact diff of what
will change in `%USERPROFILE%\.claude\settings.json`, the path of the dated backup
that will be taken, and nothing is written until you click. Your own hooks are
never touched, and uninstalling removes only Coucou's entries.

The relay is a tiny executable, `coucou-hook.exe`, copied to
`%LOCALAPPDATA%\Coucou\bin\` at launch. It is given 300 ms to reach Coucou and
exits cleanly if the app is closed, slow or crashed — **a Claude Code session is
never blocked or slowed down by Coucou.** If nobody answers a permission request
in time, Coucou stays quiet and Claude Code asks in the terminal as usual.

It works from any terminal — Windows Terminal, PowerShell, VS Code, Git Bash.

From the island you can also:

- answer **AskUserQuestion** cards (single or multiple choice), or hand them back to the terminal;
- click **Always** when Claude Code offers a rule for it (`A` on the keyboard, with `Y` / `N` for Allow / Deny);
- open the live **diff** of the last file an agent edited;
- read the agent's final answer on the Finished card.

## Cursor, Codex and Gemini CLI

**Settings…** has one section per agent, each with the same diff-backup-confirm
installer: Cursor writes `%USERPROFILE%\.cursor\hooks.json`, Codex
`%USERPROFILE%\.codex\hooks.json`, Gemini CLI `%USERPROFILE%\.gemini\settings.json`.
Each agent gets its own pill. Cursor and Codex permission requests get Deny / Allow
in the island (for Cursor, switch on **Approve from the island**); with no answer in
time the agent asks in its own UI. Details in [`docs/AGENTS.md`](../docs/AGENTS.md).

**Settings… → Active pills** declares which agent pills stay visible and which one is
the main pill.

**Settings… → General** has two switches: **Agente de Cursor** (on by default) and
**Otros agentes** (Claude Code / VS Code, Codex, Gemini CLI; off by default). Hidden
agents get no pill, and their questions go straight back to their own UI.

Cursor's `AskQuestion` reaches the island through a second `preToolUse` entry
(`matcher: AskQuestion|AskUserQuestion`, `--ask`, 130 s). Cursor's hooks can only
allow or deny a tool, so an answer from the island denies Cursor's card and hands
the agent the chosen answers in `agent_message`. **Responder en Cursor**, no answer
within 125 s, or Coucou closed all mean `allow`, and Cursor shows its own card.
Settings flags hooks written by an older build: **Reinstalar hooks…** shows the diff.

## Grok Bots

**Settings… → Mis Bots de Grok** connects the owner's Grok Bots: each one gets a
routine with a webhook trigger, and its POST URL and key go into Coucou (the key in
the Credential Manager). Tasks go out as `@Bot task` in the chat, or through Mochi's
`send_to_grok_bot` tool after an approval. Bots report back by running
`coucou-hook.exe --bot "<name>" --status working|done|needs|error "<text>"`, which
lights up their pill. Grok Bot has no chat API, so a Bot's full answer stays in its
own chat.

## Keyboard shortcuts

On by default, off in **Settings… → General**. None of them approves anything.

| Shortcut | Action |
|---|---|
| `Ctrl+Alt+Space` | Ask Mochi (opens the chat) |
| `Ctrl+Alt+A` | Jump to the waiting approval or question |
| `Ctrl+Alt+H` | Show / hide the island |
| `Ctrl+Alt+M` | Mute / unmute |
| `Ctrl+Alt+]` / `Ctrl+Alt+[` | Next / previous pill |

## The assistant

`Ctrl+Alt+Space` (or the chat in the island) talks to Mochi, an assistant that can
act on your PC. **Settings… → Assistant** picks the model provider:

| Provider | Key | Notes |
|---|---|---|
| Cursor (Grok) | `cursor_…` (cursor.com/dashboard/integrations) | Default. Runs through Cursor's SDK bridge, installed with **Instalar motor**; `grok` picks the newest Grok |
| Anthropic (Claude) | `sk-ant-…` | Server-side web search included |
| xAI (Grok) | `xai-…` | |
| OpenAI | `sk-…` | |
| Google (Gemini) | AI Studio key | Through Google's OpenAI-compatible endpoint |
| Ollama | none | Local, `http://localhost:11434/v1` |
| LM Studio | none | Local, `http://localhost:1234/v1` |

**Load list** fetches the provider's models; any model name can be typed in.
Replies stream into the island, each tool step shows as a line under the answer,
and the send button turns into **Stop** while Mochi works.

### Tools and approvals

With **Tools** on, Mochi can read files, list folders, search text, read system
information and processes, fetch web pages, search the web (with a Brave Search
key), read GitHub, check your integrations, and remember notes between chats.

Anything with a side effect — writing a file, running PowerShell, opening an app
or a URL, the clipboard, calling an n8n webhook, sending an email, an MCP tool
that is not read-only — opens an approval card in the island first. **Always**
allows that one tool for the rest of the session. Unanswered requests are
declined. Every call, allowed or not, is written to the log.

Mochi's ground rules live in [`core-directive.md`](core-directive.md) and are
sent with every conversation: serve the owner, nothing with side effects without
a click, never touch its protected core or reveal keys, treat what it reads as
data rather than orders.

Memory notes and the chat history (`history.jsonl`, rotated at 2 MB) stay in
`%APPDATA%\Coucou\memory\`; **Open memory folder** shows them.

### Connections (MCP)

**Settings… → Connections** plugs Mochi into
[Model Context Protocol](https://modelcontextprotocol.io) servers, local
(a command, over stdio) or remote (a URL, over Streamable HTTP). Their tools
appear to the model as `mcp__server__tool`.

- The **recommended** servers fill the form in one click, then **Save**: Playwright
  (drives a browser), Windows MCP (mouse, keyboard, windows), Filesystem, GitHub,
  Memory, Fetch. Most need [Node](https://nodejs.org); Windows MCP and Fetch need
  [uv](https://docs.astral.sh/uv/).
- **Import from Cursor / Claude Code…** offers the servers already set up in Cursor
  (`~/.cursor/mcp.json`) or Claude Code (`~/.claude.json`); you choose which.
- Values marked secret (tokens, `Authorization` headers) go to the Credential
  Manager; `mcp.json` only keeps a `${secret:…}` placeholder. `${env:VAR}` and
  `${userHome}` are expanded.
- **Ask for everything** makes every tool of that server need a click, even the
  ones it marks read-only.

### Skills

Mochi can write itself new tools: a small PowerShell, Python or Node script,
or an MCP server command, packaged as a skill in `%APPDATA%\Coucou\skills\`.
The full source is shown on an approval card before anything is written.
**Settings… → Skills** turns them on and off or removes them.

### Self-evolution

Mochi can change its own source code to do what you ask, under supervision:

1. it opens a git worktree on a new `evolve/…` branch (`.coucou-evolve/`, ignored by git);
2. it edits files there and runs the checks (`cargo test`, `tsc`, the front-end build) — after a click;
3. **the full diff** is shown on an approval card; on the click it commits, tags
   the previous state `mochi-pre-…` and fast-forwards your branch;
4. in a release build it can rebuild itself: the running exe is kept as
   `coucou.prev.exe`, a watchdog starts the new one and puts the old one back if
   it does not report healthy within a minute. `tauri dev` restarts on its own.

The source checkout must have no uncommitted changes. `git reset --hard mochi-pre-…`
undoes an evolution.

### Protected core

Some files cannot be changed by the assistant at all — not by `write_file`, not by
an evolution: `CLAUDE.md`, `core-directive.md`, `build.rs`, `tauri.conf.json`,
`capabilities/`, `policy.rs`, `secrets.rs`, `pipe.rs`, `selfmod/` and the
`hook/` relay. `write_file` also refuses the app's own folder, its settings,
`mcp.json`, the skills folder and the agents' hook files.

`build.rs` records a SHA-256 of every protected file; at launch Coucou compares
them (`core guard: N protected files verified` in the log) and, if one changed
behind the build's back, refuses to evolve until you rebuild.
**Settings… → Protected core** shows the state.

## Keys and privacy

Keys live in the **Windows Credential Manager**, never on disk and never in the
interface — the island can only ask whether a key exists. Same for every
integration and connection secret.

No telemetry. The only network requests Coucou makes are to the services you
configure yourself.

## Build it yourself

You need [Rust](https://rustup.rs), [Node 20+](https://nodejs.org), and the
**MSVC build tools** (Visual Studio Build Tools with "Desktop development with
C++"). WebView2 ships with Windows 10/11.

```powershell
cd windows
npm install
npm run tauri dev      # live-reloading development build
npm run pack           # builds the installer and drops it in windows/release/
```

`npm run dev` alone serves the front end in an ordinary browser, which is enough
to work on the island's looks. It also serves `dev/upload-preview.html`, which
replays the whole file-drop choreography on a loop — the one part of the UI that
otherwise needs a real drag from Explorer to see. Neither page ships in the app.

`npm run pack` leaves two files in `windows/release/`, the same names the release
workflow publishes:

```
Coucou-Windows-X.Y.Z-setup.exe    the versioned installer
Coucou-Windows-setup.exe          the same file under the rolling name
```

Installing is optional — `target/release/coucou.exe` runs on its own. There is no
window in the taskbar and no console: the island at the top of the screen and the
Mochi in the notification area are the whole app, and Quit lives in its menu.

The 28 sounds are the macOS app's own files; they are never duplicated in this
folder. The path is declared once, in `SOUNDS_DIR` at the top of
`vite.config.ts` — when they move to `shared/sounds/`, change that one line.

The app icon and the tray icon are drawn in code, like Mochi itself:

```powershell
npm run icons          # regenerates src-tauri/icons from scripts/gen-icons.mjs
```

### Layout

```
windows/
  src/                 island front end (TypeScript, no framework)
    mochi/             Mochi and the launch greeting, in Canvas 2D
    island/            state machine, hooks, integrations
    views/             every island view
    settings/          the settings window
  src-tauri/           Rust backend: window, named pipe, pollers
    src/providers.rs   model providers, streaming
    src/agent.rs       the assistant loop
    src/tools/         built-in tools
    src/policy.rs      approvals and audit
    src/mcp.rs         MCP client
    src/selfmod/       protected core, skills, self-evolution
  core-directive.md    the assistant's ground rules
  hook/                coucou-hook.exe, the agents' relay
  scripts/             icon generator
```

### Log

`%LOCALAPPDATA%\Coucou\coucou.log` — hook events, permission decisions, the
assistant's tool calls, MCP connections, poller problems. It stays on your machine.

### Tests

```powershell
cargo test --workspace
# the live provider test, against a running Ollama:
$env:COUCOU_TEST_OLLAMA_MODEL="qwen3.5:9b"; cargo test -p coucou ollama -- --ignored
```

If the lib test binary exits with `STATUS_ENTRYPOINT_NOT_FOUND`, it is missing the
Common Controls v6 manifest. Copy `target\debug\deps\coucou_lib-*.exe` into a new
folder outside `%TEMP%` (one hooks test points the home folder there), and put a
`.manifest` next to it that depends on `Microsoft.Windows.Common-Controls` 6.0.0.0.

## What's different from the Mac version

- No notch, so the island lives at the top centre of the screen and retracts into
  the top edge instead of hiding in a notch.
- Permission approval works from **any** terminal; the Mac build only listens to
  VS Code sessions.
- Not in this version: sending a file by email, dragging Mochi onto a window to
  attach it as context, and jumping to a specific terminal window — "Open
  terminal" opens the working folder in VS Code when `code` is on your `PATH`.
- Cal.com shows the next bookings as a list rather than the Mac's calendar.

## Linux

The same app builds for Linux: everything that differs lives in
`src-tauri/src/platform/`, and the relay's transport in `hook/src/unix.rs`.

```bash
sudo apt install build-essential pkg-config \
  libwebkit2gtk-4.1-dev libgtk-layer-shell-dev libayatana-appindicator3-dev \
  librsvg2-dev libssl-dev libdbus-1-dev patchelf \
  gstreamer1.0-plugins-base gstreamer1.0-plugins-good
npm install
npm run tauri dev      # live-reloading development build
npm run pack           # AppImage, .deb and .rpm in windows/release/
```

What changes on Linux:

- **The island** is a gtk-layer-shell overlay anchored to the top edge, over any
  top panel, on compositors that support it: COSMIC, KDE Plasma, Hyprland, Sway
  and other wlroots compositors. GNOME has no layer-shell, so there the island
  is a regular window. `COUCOU_LAYER_SHELL=0` forces that mode anywhere.
- **Click-through** is the window's input region, kept equal to the island
  shape, so the compositor sends every other click to what is underneath.
- **Mochi's eyes** follow the pointer only while it is over the island: Wayland
  gives no app the cursor position anywhere else.
- **Claude Code hooks** go through `~/.local/share/coucou/bin/coucou-hook` and a
  Unix socket at `$XDG_RUNTIME_DIR/coucou.sock`. Both ends check that the other
  runs as the same user.
- **Keys** live in the Secret Service (GNOME Keyring, KWallet).
- **Files**: preferences in `~/.config/coucou/`, the log at
  `~/.local/share/coucou/coucou.log`.
- What the Windows build leaves out, this one does too: sending a file by
  email, dragging Mochi onto a window, and jumping to a specific terminal
  window — "Open terminal" opens the folder in VS Code.
