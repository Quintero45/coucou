// Voice calls with the Grok Bots and the Cursor agent: the owner talks, each
// participant answers aloud in its own voice.
//
// The microphone stays open (meeting.rs's capture, mic only). A simple voice
// activity detector cuts each utterance at ~0.8 s of silence; whisper-cli turns
// it into text. Whoever is named answers ("Aerys, …", "todos…"); with no name,
// the last one who spoke answers and the others may chip in, or pass. Replies
// come from a fast model playing each Bot (xAI when its key is set, else the
// assistant's provider — Cursor's Grok by default), not from the Bots' cloud
// routines, which take far too long for a conversation. Long work goes to the
// real Bot: the reply ends with a `TAREA:` line that is sent to its webhook.
//
// While a participant speaks the microphone is ignored (no echo loop). Notices
// meant for `speak` during a call (Cursor finished, a Bot's cloud result)
// wait in a queue and are read in the next gap.
//
// Events: `call-state` (who is in, phase, speaker, muted) and `call-line`
// (one line of the transcript). Nothing is written to disk.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::meeting::{self, Capture, RATE};
use crate::providers::{self, Endpoint, Msg, Part, Provider, Request};
use crate::{grokbot, log, secrets, voice};

/// 20 ms analysis frames.
const FRAME: usize = RATE / 50;
/// Kept before the first loud frame so the first syllable isn't clipped.
const PREROLL: usize = RATE * 3 / 10;
/// Loud this long (100 ms) before it counts as speech.
const START_VOICED: usize = RATE / 10;
/// Silence that ends an utterance.
const END_SILENCE: usize = RATE * 8 / 10;
/// Shorter bursts (a cough, a click) are dropped.
const MIN_VOICED: usize = RATE * 3 / 10;
/// Cut a monologue here and answer anyway.
const MAX_UTTERANCE: usize = RATE * 25;
/// Lines of transcript each participant sees.
const HISTORY: usize = 16;
const CURSOR_ID: &str = voice::CURSOR_ID;

#[derive(Clone, Serialize, Debug, PartialEq)]
pub struct Participant {
    pub id: String,
    pub name: String,
    pub color: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CallState {
    active: bool,
    participants: Vec<Participant>,
    muted: bool,
    /// listening · hearing · thinking · speaking
    phase: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    speaker: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Clone, Serialize)]
struct CallLine {
    /// `me` or a participant id.
    who: String,
    name: String,
    text: String,
}

#[derive(Clone)]
enum Brain {
    Api(Endpoint),
    Cursor(Endpoint),
}

struct Call {
    participants: Vec<Participant>,
    muted: bool,
    phase: &'static str,
    speaker: Option<String>,
    history: VecDeque<(String, String)>,
    last_speaker: Option<String>,
    announcements: VecDeque<(String, String)>,
    cursor_status: String,
    brain: Brain,
    stop: Arc<AtomicBool>,
}

fn slot() -> &'static Mutex<Option<Call>> {
    static CALL: OnceLock<Mutex<Option<Call>>> = OnceLock::new();
    CALL.get_or_init(|| Mutex::new(None))
}

pub fn active() -> bool {
    slot().lock().unwrap().is_some()
}

/// A notice for a participant to read in the next gap (from `speak`).
pub fn announce(who: &str, text: &str) {
    if let Some(call) = slot().lock().unwrap().as_mut() {
        if call.participants.iter().any(|p| p.id == who) && call.announcements.len() < 8 {
            log::line(format!("call: notice queued for {who}"));
            call.announcements.push_back((who.to_string(), text.to_string()));
        }
    }
}

fn emit_state(app: &AppHandle, error: Option<String>) {
    let state = match slot().lock().unwrap().as_ref() {
        Some(c) => CallState {
            active: true,
            participants: c.participants.clone(),
            muted: c.muted,
            phase: c.phase,
            speaker: c.speaker.clone(),
            error,
        },
        None => CallState { active: false, participants: Vec::new(), muted: false, phase: "listening", speaker: None, error },
    };
    let _ = app.emit("call-state", state);
}

fn set_phase(app: &AppHandle, phase: &'static str, speaker: Option<&str>) {
    if let Some(c) = slot().lock().unwrap().as_mut() {
        c.phase = phase;
        c.speaker = speaker.map(str::to_string);
    }
    emit_state(app, None);
}

fn add_line(app: &AppHandle, who: &str, name: &str, text: &str) {
    if let Some(c) = slot().lock().unwrap().as_mut() {
        c.history.push_back((name.to_string(), text.to_string()));
        while c.history.len() > HISTORY {
            c.history.pop_front();
        }
    }
    let _ = app.emit("call-line", CallLine { who: who.into(), name: name.into(), text: text.into() });
}

fn participants_for(app: &AppHandle, ids: &[String]) -> Vec<Participant> {
    let bots = grokbot::list(app);
    let mut out = Vec::new();
    for id in ids {
        let id = voice::resolve_bot(app, id);
        if out.iter().any(|p: &Participant| p.id == id) {
            continue;
        }
        if id == CURSOR_ID {
            out.push(Participant { id, name: "Cursor".into(), color: "#e5e7eb".into() });
        } else if let Some(b) = bots.iter().find(|b| b.id == id) {
            out.push(Participant { id: b.id.clone(), name: b.name.clone(), color: b.color.clone() });
        }
    }
    out
}

/// xAI when its key is set (fastest Grok), else the assistant's provider.
async fn brain(app: &AppHandle) -> Result<Brain, String> {
    let settings = app.state::<crate::Shared>().settings.lock().unwrap().clone();
    if let Some(key) = Provider::Xai.info().key {
        if secrets::present(key) {
            return Endpoint::new(&settings, Provider::Xai).map(Brain::Api);
        }
    }
    let mut ep = Endpoint::new(&settings, settings.provider)?;
    if ep.provider == Provider::Cursor {
        ep.model = crate::cursor::model(&ep).await?;
        Ok(Brain::Cursor(ep))
    } else {
        Ok(Brain::Api(ep))
    }
}

// ── Turn taking ───────────────────────────────────────────────────────────────

/// Utterance detector over 16 kHz samples, with an adaptive noise floor.
struct Turn {
    buf: Vec<f32>,
    speaking: bool,
    voiced: usize,
    silence: usize,
    floor: f32,
}

impl Default for Turn {
    fn default() -> Self {
        Turn { buf: Vec::new(), speaking: false, voiced: 0, silence: 0, floor: 0.003 }
    }
}

impl Turn {
    fn threshold(&self) -> f32 {
        (self.floor * 3.0).max(0.006)
    }

    fn reset(&mut self) {
        self.buf.clear();
        self.speaking = false;
        self.voiced = 0;
        self.silence = 0;
    }

    /// Feeds samples; a whole utterance once it ends.
    fn push(&mut self, samples: &[f32]) -> Option<Vec<f32>> {
        for frame in samples.chunks(FRAME) {
            let level = meeting::rms(frame);
            let loud = level >= self.threshold();
            self.buf.extend_from_slice(frame);
            if !self.speaking {
                if !loud {
                    self.floor = self.floor * 0.95 + level * 0.05;
                }
                if self.buf.len() > PREROLL {
                    let extra = self.buf.len() - PREROLL;
                    self.buf.drain(..extra);
                }
                self.voiced = if loud { self.voiced + frame.len() } else { 0 };
                if self.voiced >= START_VOICED {
                    self.speaking = true;
                    self.silence = 0;
                }
                continue;
            }
            if loud {
                self.silence = 0;
                self.voiced += frame.len();
            } else {
                self.silence += frame.len();
            }
            if self.silence >= END_SILENCE || self.buf.len() >= MAX_UTTERANCE {
                let voiced = self.voiced;
                let out = std::mem::take(&mut self.buf);
                self.reset();
                if voiced >= MIN_VOICED {
                    return Some(out);
                }
            }
        }
        None
    }
}

// ── Who answers ───────────────────────────────────────────────────────────────

fn fold(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| match c {
            'á' | 'à' | 'ä' => 'a',
            'é' | 'è' | 'ë' => 'e',
            'í' | 'ì' | 'ï' => 'i',
            'ó' | 'ò' | 'ö' => 'o',
            'ú' | 'ù' | 'ü' => 'u',
            c => c,
        })
        .collect()
}

fn words(s: &str) -> Vec<String> {
    fold(s).split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect()
}

fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push((prev[j] + usize::from(ca != *cb)).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Whisper writes names as it hears them ("Aeris", "Egon"): one letter off still counts.
fn is_name(word: &str, name: &str) -> bool {
    word == name || (name != "cursor" && word.chars().count() >= 4 && distance(word, name) <= 1)
}

/// Participants addressed in `text`: the opening words first, then anywhere.
fn addressed(text: &str, participants: &[Participant]) -> Vec<usize> {
    let ws = words(text);
    let names: Vec<String> = participants.iter().map(|p| words(&p.name).into_iter().next().unwrap_or_default()).collect();
    let find = |scope: &[String]| -> Vec<usize> {
        names
            .iter()
            .enumerate()
            .filter(|(_, n)| !n.is_empty() && scope.iter().any(|w| is_name(w, n)))
            .map(|(i, _)| i)
            .collect()
    };
    let opening = find(&ws[..ws.len().min(4)]);
    if opening.is_empty() {
        find(&ws)
    } else {
        opening
    }
}

fn addresses_everyone(text: &str) -> bool {
    let ws = words(text);
    ws.iter().any(|w| w == "ustedes")
        || ws.iter().take(4).any(|w| matches!(w.as_str(), "todos" | "todas" | "chicos" | "chicas" | "equipo"))
}

/// The spoken part and, from a `TAREA:` line, the work for the cloud Bot.
fn split_task(reply: &str) -> (String, Option<String>) {
    let mut speech = Vec::new();
    let mut task = None;
    for line in reply.lines() {
        let t = line.trim().trim_start_matches(['*', '-', '>']).trim();
        let upper = fold(t);
        if upper.starts_with("tarea:") {
            let body = t[t.find(':').map(|i| i + 1).unwrap_or(0)..].trim();
            if !body.is_empty() {
                task = Some(body.to_string());
            }
        } else if !t.is_empty() {
            speech.push(t.to_string());
        }
    }
    (speech.join(" "), task)
}

fn is_pass(reply: &str) -> bool {
    let t = fold(reply.trim().trim_matches(|c: char| !c.is_alphanumeric()));
    t.is_empty() || (t.starts_with("paso") && t.len() <= 12)
}

/// Drops a leading "Aerys:" the model sometimes writes before its own line.
fn strip_own_name(reply: &str, name: &str) -> String {
    let t = reply.trim();
    match t.split_once(':') {
        Some((head, rest)) if fold(head.trim()) == fold(name) => rest.trim().to_string(),
        _ => t.to_string(),
    }
}

// ── Replies ───────────────────────────────────────────────────────────────────

fn persona(p: &Participant) -> String {
    let who = if p.id == CURSOR_ID {
        "Eres el agente de Cursor que programa en el ordenador de tu dueño. Estás en una llamada de voz con él (y quizá con sus \
         Bots de Grok). Cuéntale lo que estás haciendo según el estado que te pasan; si no lo sabes, dilo. Desde la llamada no \
         ejecutas nada."
            .to_string()
    } else {
        format!(
            "Eres {}, uno de los Bots de Grok de tu dueño: un compañero de IA con su propia computadora en la nube. Estás en una \
             llamada de voz con él. Si te piden un trabajo largo (investigar, programar, escribir, revisar, enviar o publicar algo), \
             no lo hagas en la llamada: di en una frase que te pones con ello y termina con una línea aparte que empiece por \
             «TAREA:» con la instrucción completa para tu yo en la nube.",
            p.name
        )
    };
    format!(
        "{who}\n\nHablas en español, como en una llamada: de 1 a 3 frases cortas y naturales, sin markdown, listas, emojis ni \
         enlaces. Habla solo por ti: nunca escribas lo que dirían los demás ni pongas tu nombre delante."
    )
}

struct Ask {
    brain: Brain,
    who: Participant,
    roster: String,
    transcript: String,
    optional: bool,
    cursor_status: String,
    stop: Arc<AtomicBool>,
}

async fn reply(ask: Ask) -> Result<String, String> {
    let mut query = format!("En la llamada: tu dueño, {}.\n", ask.roster);
    if ask.who.id == CURSOR_ID {
        let status = if ask.cursor_status.trim().is_empty() { "nada registrado todavía" } else { ask.cursor_status.trim() };
        query.push_str(&format!("Lo que estás haciendo ahora: {status}\n"));
    }
    query.push_str(&format!("\nConversación reciente:\n{}\n\n", ask.transcript));
    if ask.optional {
        query.push_str("No te han hablado a ti. Interviene solo si tienes algo útil que añadir; si no, responde exactamente PASO.\n");
    }
    query.push_str(&format!("Responde ahora como {}.", ask.who.name));
    let system = persona(&ask.who);
    let text = match &ask.brain {
        Brain::Cursor(ep) => crate::cursor::call_turn(&ask.who.id, ep, &system, &query, &ask.stop).await?,
        Brain::Api(ep) => {
            let messages = vec![Msg::User(vec![Part::Text(query)])];
            let mut ignore = |_: &str| {};
            let req = Request { system: &system, messages: &messages, tools: &[], web_search: false };
            providers::complete(ep, req, &mut ignore, &ask.stop).await?.text
        }
    };
    Ok(strip_own_name(&text, &ask.who.name))
}

async fn speak_line(app: &AppHandle, who: &str, text: &str, stop: &AtomicBool) {
    set_phase(app, "speaking", Some(who));
    match voice::synth_for(app, who, text).await {
        Ok(wav) if !stop.load(Ordering::Relaxed) => {
            if let Err(e) = voice::play_until_end(wav).await {
                log::line(format!("call: playback for {who} failed: {e}"));
            }
        }
        Ok(_) => {}
        Err(e) => log::line(format!("call: no voice for {who}: {e}")),
    }
}

async fn respond(app: &AppHandle, heard: &str, stop: &Arc<AtomicBool>) {
    let Some((participants, last, transcript, cursor_status, brain)) = slot().lock().unwrap().as_ref().map(|c| {
        let transcript = c.history.iter().map(|(n, t)| format!("{n}: {t}")).collect::<Vec<_>>().join("\n");
        (c.participants.clone(), c.last_speaker.clone(), transcript, c.cursor_status.clone(), c.brain.clone())
    }) else {
        return;
    };
    if participants.is_empty() {
        return;
    }
    let named = addressed(heard, &participants);
    let (primary, optional): (Vec<Participant>, Vec<Participant>) = if addresses_everyone(heard) {
        (participants.clone(), Vec::new())
    } else if !named.is_empty() {
        (named.iter().map(|&i| participants[i].clone()).collect(), Vec::new())
    } else {
        let lead = last
            .and_then(|id| participants.iter().find(|p| p.id == id).cloned())
            .unwrap_or_else(|| participants[0].clone());
        let rest = participants.iter().filter(|p| p.id != lead.id).cloned().collect();
        (vec![lead], rest)
    };
    let roster = participants.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ");
    set_phase(app, "thinking", Some(&primary[0].id));
    log::line(format!(
        "call: {} chars heard -> {} (may join: {})",
        heard.chars().count(),
        primary.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", "),
        optional.len()
    ));

    // Everyone thinks at once; they speak in order.
    let jobs: Vec<(Participant, bool, tauri::async_runtime::JoinHandle<Result<String, String>>)> = primary
        .into_iter()
        .map(|p| (p, false))
        .chain(optional.into_iter().map(|p| (p, true)))
        .map(|(p, opt)| {
            let ask = Ask {
                brain: brain.clone(),
                who: p.clone(),
                roster: roster.clone(),
                transcript: transcript.clone(),
                optional: opt,
                cursor_status: cursor_status.clone(),
                stop: stop.clone(),
            };
            (p, opt, tauri::async_runtime::spawn(reply(ask)))
        })
        .collect();

    for (p, optional, job) in jobs {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let text = match job.await.map_err(|e| e.to_string()).and_then(|r| r) {
            Ok(t) => t,
            Err(e) => {
                log::line(format!("call: {} could not answer: {e}", p.name));
                if !optional {
                    emit_state(app, Some(format!("{} no pudo responder: {e}", p.name)));
                }
                continue;
            }
        };
        if optional && is_pass(&text) {
            continue;
        }
        let (speech, task) = split_task(&text);
        if let Some(task) = task.filter(|_| p.id != CURSOR_ID) {
            let (app2, id, name) = (app.clone(), p.id.clone(), p.name.clone());
            tauri::async_runtime::spawn(async move {
                let message = format!("(Desde una llamada de voz con tu dueño) {task}");
                match grokbot::send(&app2, &id, &message, Vec::new()).await {
                    Ok(_) => log::line(format!("call: task handed to {name}")),
                    Err(e) => log::line(format!("call: could not hand the task to {name}: {e}")),
                }
            });
        }
        if speech.is_empty() {
            continue;
        }
        add_line(app, &p.id, &p.name, &speech);
        if !optional {
            if let Some(c) = slot().lock().unwrap().as_mut() {
                c.last_speaker = Some(p.id.clone());
            }
        }
        speak_line(app, &p.id, &speech, stop).await;
    }
}

// ── The call loop ─────────────────────────────────────────────────────────────

async fn run(app: AppHandle, stop: Arc<AtomicBool>) {
    let result = listen(&app, &stop).await;
    let error = result.err();
    if let Some(e) = &error {
        log::line(format!("call: ended with an error: {e}"));
    }
    voice::stop_playback();
    crate::cursor::end_call_sessions();
    slot().lock().unwrap().take();
    log::line("call: ended");
    emit_state(&app, error);
}

async fn listen(app: &AppHandle, stop: &Arc<AtomicBool>) -> Result<(), String> {
    let (exe, model) = meeting::ensure_whisper(app).await?;
    let capture = Capture::start(true, false)?;
    let mut turn = Turn::default();
    set_phase(app, "listening", None);
    while !stop.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let samples = capture.take();
        let (muted, notice) = match slot().lock().unwrap().as_mut() {
            Some(c) => (c.muted, if turn.speaking { None } else { c.announcements.pop_front() }),
            None => break,
        };
        if muted {
            turn.reset();
            continue;
        }
        if let Some((who, text)) = notice {
            speak_line(app, &who, &text, stop).await;
            let _ = capture.take();
            set_phase(app, "listening", None);
            continue;
        }
        let Some(utterance) = turn.push(&samples) else { continue };
        set_phase(app, "hearing", None);
        let heard = match meeting::transcribe(exe.clone(), model.clone(), utterance).await {
            Ok(t) => t.trim().to_string(),
            Err(e) => {
                log::line(format!("call: transcription failed: {e}"));
                String::new()
            }
        };
        if !heard.is_empty() && !stop.load(Ordering::Relaxed) {
            add_line(app, "me", "Tú", &heard);
            respond(app, &heard, stop).await;
        }
        // What the microphone picked up while the others spoke is not the owner.
        let _ = capture.take();
        turn.reset();
        set_phase(app, "listening", None);
    }
    Ok(())
}

// ── Commands ──────────────────────────────────────────────────────────────────

/// Starts a call with `participants` (Bot ids or names, and/or `cursor`), or
/// changes who is in the one already running.
#[tauri::command]
pub async fn start_call(app: AppHandle, participants: Vec<String>) -> Result<(), String> {
    let list = participants_for(&app, &participants);
    if list.is_empty() {
        return Err("Elige al menos un Bot para la llamada.".into());
    }
    {
        let mut s = slot().lock().unwrap();
        if let Some(c) = s.as_mut() {
            log::line(format!("call: now with {}", list.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")));
            c.participants = list;
            drop(s);
            emit_state(&app, None);
            return Ok(());
        }
    }
    let brain = brain(&app).await?;
    let stop = Arc::new(AtomicBool::new(false));
    {
        let mut s = slot().lock().unwrap();
        if s.is_some() {
            return Ok(());
        }
        log::line(format!("call: started with {}", list.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")));
        *s = Some(Call {
            participants: list,
            muted: false,
            phase: "listening",
            speaker: None,
            history: VecDeque::new(),
            last_speaker: None,
            announcements: VecDeque::new(),
            cursor_status: String::new(),
            brain,
            stop: stop.clone(),
        });
    }
    emit_state(&app, None);
    tauri::async_runtime::spawn(run(app, stop));
    Ok(())
}

#[tauri::command]
pub fn stop_call() {
    if let Some(c) = slot().lock().unwrap().as_ref() {
        c.stop.store(true, Ordering::SeqCst);
    }
    voice::stop_playback();
}

#[tauri::command]
pub fn call_mute(app: AppHandle, muted: bool) {
    if let Some(c) = slot().lock().unwrap().as_mut() {
        c.muted = muted;
    }
    emit_state(&app, None);
}

/// What the Cursor agent is doing, as the island sees it (for its answers).
#[tauri::command]
pub fn call_cursor_status(text: String) {
    if let Some(c) = slot().lock().unwrap().as_mut() {
        c.cursor_status = text.chars().take(600).collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(id: &str, name: &str) -> Participant {
        Participant { id: id.into(), name: name.into(), color: String::new() }
    }

    #[test]
    fn names_are_heard_loosely() {
        let ps = [p("aegon", "Aegon"), p("aerys", "Aerys"), p("daemond", "Daemond"), p("cursor", "Cursor")];
        assert_eq!(addressed("Aeris, ¿cómo vas?", &ps), vec![1]);
        assert_eq!(addressed("Egon y Daymond, escuchen", &ps), vec![0, 2]);
        assert_eq!(addressed("Cursor, ¿qué haces?", &ps), vec![3]);
        assert!(addressed("¿eres tú el del curso?", &ps).is_empty());
    }

    #[test]
    fn everyone_is_addressed() {
        assert!(addresses_everyone("Todos, una pregunta"));
        assert!(addresses_everyone("¿Qué opinan ustedes?"));
        assert!(!addresses_everyone("Lo hago casi siempre, como todos los días"));
    }

    #[test]
    fn task_lines_are_split_off() {
        let (speech, task) = split_task("Claro, me pongo con ello.\nTAREA: investiga los precios de X");
        assert_eq!(speech, "Claro, me pongo con ello.");
        assert_eq!(task.as_deref(), Some("investiga los precios de X"));
        assert_eq!(split_task("Hola").1, None);
    }

    #[test]
    fn passing_and_own_name() {
        assert!(is_pass("PASO"));
        assert!(is_pass("Paso."));
        assert!(!is_pass("Paso a contarte algo importante sobre esto"));
        assert_eq!(strip_own_name("Aerys: hola", "Aerys"), "hola");
        assert_eq!(strip_own_name("Hola: qué tal", "Aerys"), "Hola: qué tal");
    }

    #[test]
    fn utterance_ends_after_silence() {
        let mut t = Turn::default();
        let quiet = vec![0.0005f32; RATE];
        let loud: Vec<f32> = (0..RATE).map(|i| if i % 2 == 0 { 0.2 } else { -0.2 }).collect();
        assert!(t.push(&quiet).is_none());
        assert!(t.push(&loud).is_none());
        let got = t.push(&quiet).expect("utterance");
        assert!(got.len() >= RATE && got.len() < RATE * 2 + PREROLL);
        // A click is not speech.
        let mut t = Turn::default();
        t.push(&quiet);
        t.push(&loud[..RATE / 20]);
        assert!(t.push(&quiet).is_none());
    }
}
