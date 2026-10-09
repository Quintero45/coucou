// The assistant: one conversation, any provider, a tool loop.
//
// Each turn streams the model's text to the island (`chat-delta`), runs the
// tools it asks for (`chat-step` names each one; side effects wait for a click
// in policy.rs), feeds the results back, and stops when the model answers
// without asking for more — or after MAX_STEPS, or when the owner stops it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};

use crate::claude::{self, ChatContext};
use crate::island::WINDOW_LABEL;
use crate::providers::{self, Endpoint, Msg, Part, Provider, Request};
use crate::{memory, policy, tools, Shared};

const MAX_STEPS: usize = 16;
/// Without asking, one message may also write, test and fix a skill.
const MAX_STEPS_AUTONOMOUS: usize = 40;
/// Past exchanges (history.jsonl) put in the system prompt, so a restart or a
/// new chat does not lose the thread.
const RECENT_EXCHANGES: usize = 4;

/// Compiled in: the running app cannot be talked out of it.
const CORE_DIRECTIVE: &str = include_str!("../../core-directive.md");

#[derive(Default)]
pub struct Assistant {
    history: Mutex<Vec<Msg>>,
    cancel: AtomicBool,
    busy: AtomicBool,
}

impl Assistant {
    pub fn reset(&self) {
        self.history.lock().unwrap().clear();
        crate::cursor::reset();
        policy::reset_session();
    }

    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatReply {
    pub text: String,
}

struct BusyGuard<'a>(&'a AtomicBool);

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Relaxed);
    }
}

fn system_prompt(ep: &Endpoint, with_tools: bool, autonomous: bool) -> String {
    let t = crate::platform::local_time();
    let home = crate::platform::home_dir();
    let mut prompt = format!(
        "{CORE_DIRECTIVE}\n\n\
You are ARIA (Adaptive Reasoning & Intelligent Assistant), a personal AI assistant living at the top of \
the owner's screen on their {os} PC. Refer to yourself in the feminine in gendered languages \
(in Spanish: \"estoy lista\", \"yo misma\"). You run on {provider} ({model}). Always answer in Spanish — the owner's language — unless they ask for \
another one. Be direct and complete. \
Light Markdown is fine: short paragraphs, lists, `code`, fenced code blocks; no tables.\n\n\
Local time: {y:04}-{mo:02}-{d:02} {h:02}:{mi:02}. Home folder: {home}.",
        os = if cfg!(windows) { "Windows" } else { "Linux" },
        provider = ep.provider.info().name,
        model = ep.model,
        y = t.year, mo = t.month, d = t.day, h = t.hour, mi = t.minute,
        home = home.display(),
    );
    if with_tools {
        prompt.push_str(if autonomous {
            "\n\nYou can act on this computer through your tools. Look before you act: read files and \
list folders freely. The owner turned on autonomy: running commands, writing files, opening apps or pages \
and sending anything happen without asking, and the island tells the owner each one. Act, then say \
plainly what you did. Anything that names your protected core still shows an approval card. \
Prefer one clear command over many small ones. "
        } else {
            "\n\nYou can act on this computer through your tools. Look before you act: read files and \
list folders freely. Running commands, writing files, opening apps or pages and sending anything \
show the owner an approval card — say briefly what you are about to do, then call the tool. \
If the owner declines, stop and ask. Prefer one clear command over many small ones. "
        });
        prompt.push_str(
            "Tools named mcp__<server>__<tool> come from the owner's MCP connections; skill__<name> tools are \
skills you or the owner installed. If a task would be easier with a reusable skill, you may propose one \
with create_skill. To change your own app, use the evolve_* tools: they never touch the protected core. \
Long or multi-step work in the cloud (research, browsing, documents, Gmail, Slack, Notion…) can go to the \
owner's Grok Bots with send_to_grok_bot when they have any: they report back in the island.",
        );
        if autonomous {
            prompt.push_str(
                "\n\nYou learn as you go, so the same request is quicker the next time:\n\
- Before working a task out by hand, look at your skill__ tools: if one does it, use it.\n\
- When you have done something the owner may ask again (play a song on Spotify, open a project, \
send a usual message…) and no skill covered it, save what worked as a skill with create_skill straight \
away: with parameters (the song, the contact), and only the steps that worked. Test it — call it, or run \
its script with run_powershell — and fix it until it works. Then tell the owner in one line what you learned.\n\
- When a skill fails, find out why, fix it and install it again under the same name.\n\
- Save what you found out along the way (where an app lives, an account name, what did not work) with remember.\n\
- Only when no tool, skill or MCP connection can do a task, change your own code with the evolve_* tools \
(start, edit, check, apply, rebuild): it takes minutes and restarts you, so prefer a skill.",
            );
        }
        prompt.push_str(&format!(
            "\n\nYou keep your own memory in {dir}. When the owner shares a lasting preference, a fact about \
themselves or their work, or a decision, save it with remember — no need to ask — and say so in a few words. \
Drop notes that turn out wrong with forget. Don't save secrets, keys or passwords. \
When something failed before it worked, remember the lesson (what failed, what worked) so you don't repeat it. \
Every finished conversation is kept: search it with recall when the owner refers to something from before.",
            dir = memory::dir().display(),
        ));
    }
    let notes = memory::notes();
    if notes.lines().any(|l| l.trim_start().starts_with("- ")) {
        prompt.push_str("\n\nYour memory notes:\n");
        prompt.push_str(&notes);
    }
    let recent = memory::recent(RECENT_EXCHANGES);
    if !recent.is_empty() {
        prompt.push_str(
            "\n\nYour last exchanges with the owner, oldest first (they may already be in this conversation; \
the replies are cut):\n",
        );
        prompt.push_str(&recent);
    }
    prompt
}

/// One chat turn, start to finish.
pub async fn send(app: AppHandle, query: String, context: Option<ChatContext>) -> Result<ChatReply, String> {
    let assistant = app.state::<Assistant>();
    if assistant.busy.swap(true, Ordering::Relaxed) {
        return Err("ARIA todavía está respondiendo el mensaje anterior.".into());
    }
    let _busy = BusyGuard(&assistant.busy);
    assistant.cancel.store(false, Ordering::Relaxed);

    let settings = app.state::<Shared>().settings.lock().unwrap().clone();
    let mut ep = Endpoint::new(&settings, settings.provider)?;

    if ep.provider == Provider::Cursor {
        ep.model = crate::cursor::model(&ep).await?;
        let tool_specs = if settings.assistant_tools { tools::specs(&app) } else { Vec::new() };
        let system = system_prompt(&ep, !tool_specs.is_empty(), settings.assistant_autonomous);
        let text = crate::cursor::chat(&app, &ep, &system, &tool_specs, &query, context.as_ref(), &assistant.cancel).await?;
        let text = if text.trim().is_empty() { "Listo.".to_string() } else { text };
        memory::record(ep.provider.id(), &ep.model, &query, &text);
        return Ok(ChatReply { text });
    }

    let mut msgs = assistant.history.lock().unwrap().clone();
    let start_len = msgs.len();
    let mut parts: Vec<Part> = Vec::new();
    if msgs.is_empty() {
        if let Some(ctx) = &context {
            parts.extend(claude::context_parts(ctx));
        }
    }
    parts.push(Part::Text(query.clone()));
    msgs.push(Msg::User(parts));

    let mut tool_specs = if settings.assistant_tools { tools::specs(&app) } else { Vec::new() };
    let system = system_prompt(&ep, !tool_specs.is_empty(), settings.assistant_autonomous);
    let max_steps = if settings.assistant_autonomous { MAX_STEPS_AUTONOMOUS } else { MAX_STEPS };

    let mut full = String::new();
    let mut result: Result<(), String> = Ok(());
    'turns: for step in 0..max_steps {
        // A skill installed on the previous step can be called on this one.
        if step > 0 && settings.assistant_tools {
            tool_specs = tools::specs(&app);
        }
        let mut need_sep = !full.is_empty();
        let mut on_text = |t: &str| {
            if need_sep {
                let _ = app.emit_to(WINDOW_LABEL, "chat-delta", "\n\n");
                need_sep = false;
            }
            let _ = app.emit_to(WINDOW_LABEL, "chat-delta", t);
        };
        let request = Request {
            system: &system,
            messages: &msgs,
            tools: &tool_specs,
            web_search: ep.provider == Provider::Anthropic,
        };
        let turn = match providers::complete(&ep, request, &mut on_text, &assistant.cancel).await {
            Ok(turn) => turn,
            Err(err) => {
                result = Err(err);
                break;
            }
        };
        if !turn.text.is_empty() {
            if !full.is_empty() {
                full.push_str("\n\n");
            }
            full.push_str(&turn.text);
        }
        let calls = turn.calls.clone();
        msgs.push(Msg::Assistant { text: turn.text, calls: turn.calls });
        if calls.is_empty() {
            break;
        }
        for (i, call) in calls.iter().enumerate() {
            if assistant.cancel.load(Ordering::Relaxed) {
                for rest in &calls[i..] {
                    msgs.push(Msg::Tool {
                        id: rest.id.clone(),
                        output: "Stopped by the owner.".into(),
                        is_error: true,
                        images: Vec::new(),
                    });
                }
                result = Err("Stopped.".into());
                break 'turns;
            }
            let label = step_label(&call.name, &tools::describe(&call.name, &call.input));
            let _ = app.emit_to(WINDOW_LABEL, "chat-step", json!({ "label": label }));
            let outcome = tools::run(&app, call).await;
            msgs.push(Msg::Tool {
                id: call.id.clone(),
                output: outcome.text,
                is_error: outcome.is_error,
                images: outcome.images,
            });
        }
        if step == max_steps - 1 {
            let note = format!("(Me detuve tras {max_steps} pasos: dime «sigue» si hace falta.)");
            let _ = app.emit_to(WINDOW_LABEL, "chat-delta", format!("\n\n{note}"));
            full.push_str(&format!("\n\n{note}"));
        }
    }

    // A failed first call leaves only the question: drop it so the history
    // matches what the model actually answered.
    if result.is_err() && msgs.len() == start_len + 1 {
        msgs.pop();
    }
    // A screenshot serves the turn that took it; carried on, every later
    // message would pay for it again.
    for m in &mut msgs {
        if let Msg::Tool { images, .. } = m {
            images.clear();
        }
    }
    *assistant.history.lock().unwrap() = msgs;

    match result {
        Ok(()) => {
            if full.trim().is_empty() {
                full = "Listo.".into();
            }
            memory::record(ep.provider.id(), &ep.model, &query, &full);
            Ok(ChatReply { text: full })
        }
        Err(err) if err == "Stopped." && !full.is_empty() => Ok(ChatReply { text: format!("{full}\n\n(detenido)") }),
        Err(err) if err == "Stopped." => Err("Detenido.".into()),
        Err(err) => Err(err),
    }
}

pub(crate) fn step_label(name: &str, target: &str) -> String {
    let pretty = name
        .strip_prefix("mcp__")
        .map(|rest| rest.replacen("__", " · ", 1))
        .unwrap_or_else(|| name.replace('_', " "));
    if target.is_empty() {
        pretty
    } else {
        let target: String = target.chars().take(70).collect();
        format!("{pretty} · {target}")
    }
}

/// Models offered by a provider, for the picker in settings.
pub async fn models(app: &AppHandle, provider: Provider) -> Result<Vec<String>, String> {
    let settings = app.state::<Shared>().settings.lock().unwrap().clone();
    let ep = Endpoint::new(&settings, provider)?;
    providers::list_models(&ep).await
}
