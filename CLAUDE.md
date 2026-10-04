# Coucou — guide for AI coding agents

This is an independent fork of Coucou (MIT, upstream by Louis Raille). Work happens on Windows, in `windows/` — the Tauri app. `NotchBuddy/` is the original macOS app: it stays as a reference and is not modified.

Mochi lives at the top of the screen. It shows AI coding agent sessions (Claude Code, Cursor, Codex, Gemini CLI, Antigravity), lets the user approve, answer, chat and drop files, and is a full assistant: several AI providers (Anthropic, xAI Grok, OpenAI, Google, Ollama, LM Studio), local tools, MCP connections, skills it writes itself, and changes to its own code under supervision.

## Where things are
- `windows/src-tauri/` — Rust: `pipe.rs` (relay server), `hooks.rs` (agent hook installers), `providers.rs` (AI APIs), `agent.rs` (tool loop), `tools/` (local tools), `policy.rs` (approvals and audit), `mcp.rs` (MCP client), `selfmod/` (core guard, skills, self-evolution).
- `windows/src/` — TypeScript, no framework: island, views, settings window, Mochi in `mochi/`.
- `windows/hook/` — the `coucou-hook` relay every agent's hooks call.
- `windows/core-directive.md` — Mochi's core directive. Protected.
- `NotchBuddy/Sources/CoucouKit/PillCatalog.swift` — pill IDs, names and colours (reference for `windows/src/core/state.ts`).
- `docs/AGENTS.md`, `windows/README.md` — agent integrations and Windows specifics.

## Build
```
cd windows && npm install && npm run tauri dev
cargo test --workspace        # from windows/
npx tsc --noEmit              # from windows/
```

## Rules
- No third-party dependencies unless truly unavoidable. The character is drawn in code, no Rive/Lottie/images.
- Secrets live in the Windows Credential Manager, never on disk or in git.
- No telemetry. Network calls only to services the user configured.
- Never block an agent: if the app doesn't answer, the hook exits immediately and the agent asks in its own UI.
- Never overwrite an agent's config (`~/.claude/settings.json`, `~/.cursor/hooks.json`, `~/.codex/hooks.json`, `~/.gemini/settings.json`): dated backup, merge, show the diff, write only after the user confirms.
- Nothing with side effects happens without an explicit click: sending an email, approving an agent's permission, running a command, writing a file, opening an app, installing a skill, applying a change to Mochi's own code.
- The protected core cannot be changed by Mochi itself: `windows/core-directive.md`, `policy.rs`, `secrets.rs`, `pipe.rs`, `selfmod/`, `hook/`, `tauri.conf.json`, `capabilities/`, this file. Only a human edits them.
- Every tool call the assistant makes is written to the audit log (`coucou.log`).
- Performance: 0 % CPU when the island is hidden.
- Keep the identifier `fr.louisraille.coucou` until the rebrand (Credential Manager items and preferences depend on it); the rebrand migrates them.
- Pill IDs are stable contract values (Credential Manager, settings, hook routing): never rename an existing pill ID.
- New views follow the existing app style.
- No public release or installer until the rebrand: the upstream character, sounds and icons need their own licence first. Keep the upstream MIT notice.
