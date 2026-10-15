// Meeting notes: ARIA records a call (the microphone, and what the PC plays
// for the other people, through WASAPI loopback), writes a live transcript,
// then the whole one person by person with Parakeet and the speaker
// separation, and a summary with the to-dos by the owner's AI. From Coucou's
// notes/mod.rs (the Notetaker).
//
// Everything stays in %LOCALAPPDATA%\ARIA\Notes\<id>\note.json; the audio is
// deleted once the transcript is written. Only the transcript's text (never
// the audio) goes to the AI, for the summary. No bot joins the call: the
// other people should be told it is recorded.

pub mod builder;

use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::flow::pill;
use crate::i18n::{t, tf};
use crate::meeting::Capture;
use crate::providers::{self, Msg, Part, Provider, Request};
use crate::voice::{diarizer, stt};
use builder::{Line, Speaker, TimedWord, Todo};

const RATE: usize = 16_000;
/// The summary waits this long for the AI (Cursor starts an agent first).
const SUMMARY_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Recording,
    Processing,
    Ready,
    Failed,
}

/// A call noted by ARIA.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    pub id: String,
    pub title: String,
    /// Set once the owner renames the note: the summary then keeps the title.
    #[serde(default)]
    pub title_edited: bool,
    /// Milliseconds since 1970.
    pub start: u64,
    #[serde(default)]
    pub end: Option<u64>,
    /// The app in front when the recording started ("Teams", "Chrome").
    #[serde(default)]
    pub app: Option<String>,
    pub status: Status,
    #[serde(default)]
    pub speakers: Vec<Speaker>,
    #[serde(default)]
    pub lines: Vec<Line>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub key_points: Vec<String>,
    #[serde(default)]
    pub todos: Vec<Todo>,
    #[serde(default)]
    pub error: Option<String>,
    /// The start written out in local time ("2026-10-15 11:30"), for the Markdown.
    #[serde(default)]
    pub start_local: String,
}

// ── WAV on disk ───────────────────────────────────────────────────────────────

/// 16-bit mono WAV written as the call goes; the sizes are filled in at the end.
struct WavWriter {
    file: std::io::BufWriter<std::fs::File>,
    samples: u32,
}

impl WavWriter {
    fn create(path: &Path) -> std::io::Result<Self> {
        let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
        file.write_all(&crate::voice::wav_from_pcm16(&[], RATE as u32))?;
        Ok(WavWriter { file, samples: 0 })
    }

    fn write(&mut self, samples: &[f32]) -> std::io::Result<()> {
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for s in samples {
            bytes.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
        }
        self.samples += samples.len() as u32;
        self.file.write_all(&bytes)
    }

    fn finish(mut self) -> std::io::Result<()> {
        let data = self.samples * 2;
        self.file.seek(SeekFrom::Start(4))?;
        self.file.write_all(&(36 + data).to_le_bytes())?;
        self.file.seek(SeekFrom::Start(40))?;
        self.file.write_all(&data.to_le_bytes())?;
        self.file.flush()
    }
}

/// The samples of a 16-bit mono WAV written by WavWriter (or any 16 kHz one).
fn read_wav(path: &Path) -> std::io::Result<Vec<f32>> {
    let bytes = std::fs::read(path)?;
    let at = bytes.windows(4).position(|w| w == b"data").map(|p| p + 8).unwrap_or(bytes.len());
    Ok(bytes[at..].chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0).collect())
}

// ── One side of the call ──────────────────────────────────────────────────────

/// One side of the call being written to disk, with what the live transcript
/// has not taken yet.
struct Channel {
    writer: Option<WavWriter>,
    /// Samples since the last live chunk.
    live: Vec<f32>,
    /// Where `live` starts, in samples from the start of the recording.
    live_start: usize,
    total: usize,
}

impl Channel {
    fn new(path: &Path) -> std::io::Result<Channel> {
        Ok(Channel { writer: Some(WavWriter::create(path)?), live: Vec::new(), live_start: 0, total: 0 })
    }

    fn push(&mut self, samples: &[f32]) {
        if let Some(w) = self.writer.as_mut() {
            let _ = w.write(samples);
        }
        self.live.extend_from_slice(samples);
        self.total += samples.len();
    }

    /// A stretch ending on a pause (or a long one) for the live transcript.
    fn take_chunk(&mut self) -> Option<(f64, Vec<f32>)> {
        if self.live.len() < RATE * 3 / 2 {
            return None;
        }
        let tail = &self.live[self.live.len().saturating_sub(RATE / 2)..];
        let quiet = tail.iter().fold(0.0f32, |m, &s| m.max(s.abs())) < 0.01;
        if !quiet && self.live.len() < RATE * 12 {
            return None;
        }
        let start = self.live_start as f64 / RATE as f64;
        let chunk = std::mem::take(&mut self.live);
        self.live_start = self.total;
        let peak = chunk.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        if peak < 0.01 {
            return None;
        }
        Some((start, chunk))
    }

    fn close(&mut self) {
        if let Some(w) = self.writer.take() {
            let _ = w.finish();
        }
    }
}

/// The loopback sends nothing while the PC is silent: after a second with
/// nothing from it, the PC's side gets silence up to the microphone's length,
/// so both stay on the same clock. While it plays it only runs a little behind
/// (it comes in bursts): silence put there would cut its words in two.
fn align(mic: &Channel, system: &mut Channel, played_nothing: bool) {
    if played_nothing && system.total < mic.total {
        let gap = vec![0.0; mic.total - system.total];
        system.push(&gap);
    }
}

// ── The note taker ────────────────────────────────────────────────────────────

struct Recording {
    id: String,
    stop: Arc<AtomicBool>,
    pump: Option<std::thread::JoinHandle<()>>,
}

pub struct NoteTaker {
    notes: Mutex<Vec<Note>>,
    recording: Mutex<Option<Recording>>,
    /// Notes whose transcript or summary is being written.
    working: Mutex<Vec<String>>,
    starting: AtomicBool,
}

pub fn folder() -> PathBuf {
    crate::settings::local_dir().join("Notes")
}

fn note_dir(id: &str) -> PathBuf {
    folder().join(id)
}

fn get(app: &AppHandle) -> Option<Arc<NoteTaker>> {
    app.try_state::<Arc<NoteTaker>>().map(|s| s.inner().clone())
}

pub fn is_recording(app: &AppHandle) -> bool {
    get(app).is_some_and(|n| n.recording.lock().unwrap().is_some())
}

fn local_stamp() -> String {
    let t = crate::platform::local_time();
    format!("{:04}-{:02}-{:02} {:02}:{:02}", t.year, t.month, t.day, t.hour, t.minute)
}

fn person_name(n: usize) -> String {
    tf("Person {0}", &[("0", &n.to_string())])
}

fn changed(app: &AppHandle, id: &str) {
    let _ = app.emit("notes-changed", id);
}

impl NoteTaker {
    fn save(&self, note: &Note) {
        let dir = note_dir(&note.id);
        if crate::platform::ensure_private_dir(&dir).is_err() {
            return;
        }
        if let Ok(text) = serde_json::to_string_pretty(note) {
            let tmp = dir.join("note.json.tmp");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(tmp, dir.join("note.json"));
            }
        }
    }

    fn note(&self, id: &str) -> Option<Note> {
        self.notes.lock().unwrap().iter().find(|n| n.id == id).cloned()
    }

    /// Writes a change onto the note as it is now (it may have been renamed meanwhile).
    fn change(&self, app: &AppHandle, id: &str, f: impl FnOnce(&mut Note)) -> Option<Note> {
        let updated = {
            let mut notes = self.notes.lock().unwrap();
            let note = notes.iter_mut().find(|n| n.id == id)?;
            f(note);
            note.clone()
        };
        self.save(&updated);
        changed(app, &updated.id);
        Some(updated)
    }
}

pub fn setup(app: &AppHandle) {
    let mut notes: Vec<Note> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(folder()) {
        for entry in entries.flatten() {
            if let Ok(text) = std::fs::read_to_string(entry.path().join("note.json")) {
                if let Ok(note) = serde_json::from_str::<Note>(&text) {
                    notes.push(note);
                }
            }
        }
    }
    notes.sort_by(|a, b| b.start.cmp(&a.start));
    // A call left recording or being written when ARIA last quit: written now.
    let unfinished: Vec<String> = notes
        .iter()
        .filter(|n| matches!(n.status, Status::Recording | Status::Processing))
        .map(|n| n.id.clone())
        .collect();
    for n in notes.iter_mut().filter(|n| n.status == Status::Recording) {
        n.status = Status::Processing;
        n.end.get_or_insert(n.start);
    }
    let taker = Arc::new(NoteTaker {
        notes: Mutex::new(notes),
        recording: Mutex::new(None),
        working: Mutex::new(Vec::new()),
        starting: AtomicBool::new(false),
    });
    app.manage(taker);
    for id in unfinished {
        let app = app.clone();
        std::thread::spawn(move || process(&app, &id));
    }
}

fn needs_parakeet() -> String {
    t("To take notes, download Parakeet first: Settings → Voice → Speech recognition.")
}

pub fn start(app: &AppHandle) -> Result<String, String> {
    let taker = get(app).ok_or_else(|| "not ready".to_string())?;
    if taker.recording.lock().unwrap().is_some() || taker.starting.swap(true, Ordering::SeqCst) {
        return Err(t("ARIA is already taking notes."));
    }
    let result = start_inner(app, &taker);
    taker.starting.store(false, Ordering::SeqCst);
    result
}

fn start_inner(app: &AppHandle, taker: &Arc<NoteTaker>) -> Result<String, String> {
    if !stt::is_ready() {
        return Err(needs_parakeet());
    }
    let now = crate::flow::store::now_ms();
    let stamp = local_stamp();
    let id = crate::flow::text::new_id();
    let note = Note {
        id: id.clone(),
        title: tf("Call · {0}", &[("0", stamp.get(5..).unwrap_or(&stamp))]),
        title_edited: false,
        start: now,
        end: None,
        app: crate::flow::os::foreground_app().filter(|(id, _)| !crate::flow::is_shell(id)).map(|(_, name)| name),
        status: Status::Recording,
        speakers: Vec::new(),
        lines: Vec::new(),
        summary: None,
        key_points: Vec::new(),
        todos: Vec::new(),
        error: None,
        start_local: stamp,
    };
    let dir = note_dir(&id);
    crate::platform::ensure_private_dir(&dir).map_err(|e| e.to_string())?;
    let opened = (|| {
        let mic = Channel::new(&dir.join("mic.wav")).map_err(|e| e.to_string())?;
        let system = Channel::new(&dir.join("system.wav")).map_err(|e| e.to_string())?;
        let capture = Capture::start(true, true).map_err(|e| {
            crate::log::line(format!("notes: no audio: {e}"));
            t("ARIA can't hear the call: the microphone is not allowed or not available.")
        })?;
        Ok::<_, String>((mic, system, capture))
    })();
    let (mic, system, capture) = match opened {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
    };
    taker.notes.lock().unwrap().insert(0, note.clone());
    taker.save(&note);
    let stop = Arc::new(AtomicBool::new(false));
    let pump = {
        let (app, taker, id, stop) = (app.clone(), taker.clone(), id.clone(), stop.clone());
        std::thread::Builder::new()
            .name("aria-notes".into())
            .spawn(move || pump(app, taker, id, capture, mic, system, stop))
            .map_err(|e| e.to_string())?
    };
    *taker.recording.lock().unwrap() = Some(Recording { id: id.clone(), stop, pump: Some(pump) });
    stt::preload();
    pill::update(app, |s| s.note_started_at = Some(note.start));
    changed(app, &id);
    crate::log::line("notes: recording");
    Ok(id)
}

/// Every second while recording: the audio goes to disk, and the stretches
/// ending on a pause go to Parakeet for the live transcript (the final one is
/// written again from the whole recording).
fn pump(
    app: AppHandle,
    taker: Arc<NoteTaker>,
    id: String,
    mut capture: Capture,
    mut mic: Channel,
    mut system: Channel,
    stop: Arc<AtomicBool>,
) {
    let me = t("Me");
    let others = t("Others");
    loop {
        let stopping = stop.load(Ordering::SeqCst);
        if !stopping {
            std::thread::sleep(Duration::from_secs(1));
        }
        let (heard, played) = capture.take_split();
        mic.push(&heard);
        system.push(&played);
        align(&mic, &mut system, played.is_empty());
        if stopping {
            break;
        }
        for (channel, speaker) in [(&mut mic, builder::ME), (&mut system, "others")] {
            let Some((start, samples)) = channel.take_chunk() else { continue };
            let Some(text) = stt::transcribe(&samples) else { continue };
            let text = text.trim().to_string();
            if text.is_empty() || stop.load(Ordering::SeqCst) {
                continue;
            }
            let end = start + samples.len() as f64 / RATE as f64;
            taker.change(&app, &id, |n| {
                if n.speakers.is_empty() {
                    n.speakers = vec![
                        Speaker { id: builder::ME.into(), name: me.clone(), color: 0, is_me: true },
                        Speaker { id: "others".into(), name: others.clone(), color: 1, is_me: false },
                    ];
                }
                n.lines.push(Line { id: crate::flow::text::new_id(), speaker: speaker.into(), start, end, text });
                n.lines.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
            });
        }
    }
    capture.stop();
    mic.close();
    system.close();
}

fn finish_recording(app: &AppHandle) -> Option<String> {
    let taker = get(app)?;
    let mut rec = taker.recording.lock().unwrap().take()?;
    rec.stop.store(true, Ordering::SeqCst);
    if let Some(pump) = rec.pump.take() {
        let _ = pump.join();
    }
    taker.change(app, &rec.id, |n| {
        n.end = Some(crate::flow::store::now_ms());
        n.status = Status::Processing;
    });
    pill::update(app, |s| s.note_started_at = None);
    crate::log::line("notes: recording stopped");
    Some(rec.id)
}

pub fn stop(app: &AppHandle) {
    if let Some(id) = finish_recording(app) {
        let app = app.clone();
        std::thread::spawn(move || process(&app, &id));
    }
}

/// ARIA quits during a call: the recordings are closed properly, and the
/// transcript is written at the next launch.
pub fn shutdown(app: &AppHandle) {
    finish_recording(app);
}

fn words_in(path: &Path) -> Result<(Vec<TimedWord>, Vec<f32>), String> {
    if !path.exists() {
        return Ok((Vec::new(), Vec::new()));
    }
    let samples = read_wav(path).map_err(|e| e.to_string())?;
    if samples.iter().all(|s| s.abs() < 0.003) {
        return Ok((Vec::new(), samples));
    }
    let heard = stt::transcribe_timed(&samples).ok_or_else(needs_parakeet)?;
    Ok((heard.words, samples))
}

/// Writes the transcript from the recordings, then the summary, then deletes the audio.
fn process(app: &AppHandle, id: &str) {
    let Some(taker) = get(app) else { return };
    {
        let mut working = taker.working.lock().unwrap();
        if working.iter().any(|w| w == id) {
            return;
        }
        working.push(id.to_string());
    }
    changed(app, id);
    let result = transcript(app, &taker, id);
    if let Err(err) = &result {
        crate::log::line(format!("notes: transcript failed: {err}"));
        taker.change(app, id, |n| {
            n.status = Status::Failed;
            n.error = Some(err.clone());
        });
    }
    taker.working.lock().unwrap().retain(|w| w != id);
    changed(app, id);
    if result.is_ok() {
        summarize(app, id);
        let title = taker.note(id).map(|n| n.title).unwrap_or_default();
        pill::announce(app, "check", "#34D399", tf("Notes ready: {0}", &[("0", &title)]), Some((t("See"), "openNotes")), 12);
    }
}

fn transcript(app: &AppHandle, taker: &Arc<NoteTaker>, id: &str) -> Result<(), String> {
    let dir = note_dir(id);
    let mic_path = dir.join("mic.wav");
    let system_path = dir.join("system.wav");
    if !mic_path.exists() && !system_path.exists() {
        return Err(t("The recording of this call is gone."));
    }
    let me = t("Me");
    let (mine, mic_samples) = words_in(&mic_path)?;
    let (theirs, system_samples) = words_in(&system_path)?;
    let built = if theirs.len() >= 5 {
        // A call: the owner on the microphone, the others on the PC's sound.
        let turns = diarizer::diarize(&system_samples);
        builder::build(&mine, &theirs, &turns, &me, person_name)
    } else if mine.len() >= 20 {
        // People around the computer: one recording, the voices told apart.
        let turns = diarizer::diarize(&mic_samples);
        let voices: std::collections::HashSet<&str> = turns.iter().map(|t| t.speaker.as_str()).collect();
        if voices.len() > 1 {
            builder::build_room(&mine, &turns, person_name)
        } else {
            builder::build(&mine, &[], &[], &me, person_name)
        }
    } else {
        builder::build(&mine, &theirs, &[], &me, person_name)
    };
    let (speakers, lines) = built;
    let count = lines.len();
    taker.change(app, id, |n| {
        n.speakers = speakers;
        n.lines = lines;
        n.status = Status::Ready;
        n.error = None;
    });
    // Debug builds only: ARIA_NOTES_KEEP_AUDIO keeps the recordings to listen to.
    if !(cfg!(debug_assertions) && std::env::var_os("ARIA_NOTES_KEEP_AUDIO").is_some()) {
        let _ = std::fs::remove_file(&mic_path);
        let _ = std::fs::remove_file(&system_path);
    }
    crate::log::line(format!("notes: transcript of {count} lines, audio deleted"));
    Ok(())
}

fn plain_transcript(note: &Note) -> String {
    note.lines
        .iter()
        .map(|l| {
            let name = note.speakers.iter().find(|s| s.id == l.speaker).map(|s| s.name.clone()).unwrap_or_else(|| l.speaker.clone());
            format!("{name}: {}", l.text)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The summary through the owner's AI: the assistant's provider (or the first
/// one with a key or a local model), else Cursor when the assistant runs on it.
async fn ask_ai(app: &AppHandle, system: &str, user: &str) -> Result<String, String> {
    let cancel = AtomicBool::new(false);
    if let Some(ep) = crate::flow::cleanup::endpoint(app) {
        let messages = [Msg::User(vec![Part::Text(user.to_string())])];
        let mut ignore = |_: &str| {};
        let call = providers::complete(&ep, Request { system, messages: &messages, tools: &[], web_search: false }, &mut ignore, &cancel);
        return match tokio::time::timeout(SUMMARY_TIMEOUT, call).await {
            Ok(Ok(turn)) => {
                crate::log::line(format!("notes: summary by {}", ep.provider.id()));
                Ok(turn.text)
            }
            Ok(Err(e)) => Err(e),
            Err(_) => Err(t("The AI took too long to answer.")),
        };
    }
    let settings = app.state::<crate::Shared>().settings.lock().unwrap().clone();
    if settings.provider == Provider::Cursor {
        let ep = providers::Endpoint::new(&settings, Provider::Cursor)?;
        let call = crate::cursor::answer(&ep, system, user, &cancel);
        return match tokio::time::timeout(SUMMARY_TIMEOUT, call).await {
            Ok(Ok(text)) => {
                crate::log::line("notes: summary by cursor");
                Ok(text)
            }
            Ok(Err(e)) => Err(e),
            Err(_) => {
                cancel.store(true, Ordering::SeqCst);
                Err(t("The AI took too long to answer."))
            }
        };
    }
    Err(t("No AI to write the summary: set one up in Settings → Assistant."))
}

/// Writes (or writes again) the summary, key points and to-dos of a note.
fn summarize(app: &AppHandle, id: &str) {
    let Some(taker) = get(app) else { return };
    let Some(note) = taker.note(id) else { return };
    if note.lines.is_empty() {
        return;
    }
    taker.working.lock().unwrap().push(id.to_string());
    changed(app, id);
    let mut transcript = plain_transcript(&note);
    if transcript.len() > 400_000 {
        let mut cut = 400_000;
        while !transcript.is_char_boundary(cut) {
            cut -= 1;
        }
        transcript.truncate(cut);
    }
    let user = format!("<transcript>\n{transcript}\n</transcript>");
    let reply = tauri::async_runtime::block_on(ask_ai(app, builder::SUMMARY_INSTRUCTIONS, &user));
    taker.working.lock().unwrap().retain(|w| w != id);
    match reply {
        Ok(reply) => {
            taker.change(app, id, |n| {
                match builder::parse_summary(&reply) {
                    Some(parsed) => {
                        if let Some(title) = parsed.title.filter(|_| !n.title_edited) {
                            n.title = title;
                        }
                        n.summary = Some(parsed.summary);
                        n.key_points = parsed.key_points;
                        n.todos = parsed.todos;
                    }
                    None => n.summary = Some(reply.trim().to_string()),
                }
                n.error = None;
            });
        }
        Err(err) => {
            crate::log::line(format!("notes: no summary: {err}"));
            taker.change(app, id, |n| n.error = Some(err));
        }
    }
}

/// The note as Markdown: title, date, summary, key points, to-dos, then the transcript.
pub fn markdown(note: &Note) -> String {
    let mut out = format!("# {}\n\n", note.title);
    let minutes = (((note.end.unwrap_or(note.start).saturating_sub(note.start)) as f64 / 60_000.0).round() as u64).max(1);
    out += &format!("_{} · {} min_", note.start_local, minutes);
    if !note.speakers.is_empty() {
        out += &format!("  \n_{}_", note.speakers.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", "));
    }
    out += "\n\n";
    if let Some(summary) = note.summary.as_ref().filter(|s| !s.is_empty()) {
        out += &format!("## {}\n\n{summary}\n\n", t("Summary"));
    }
    if !note.key_points.is_empty() {
        out += &format!("## {}\n\n", t("Key points"));
        out += &note.key_points.iter().map(|p| format!("- {p}")).collect::<Vec<_>>().join("\n");
        out += "\n\n";
    }
    if !note.todos.is_empty() {
        out += &format!("## {}\n\n", t("To do"));
        let lines: Vec<String> = note
            .todos
            .iter()
            .map(|todo| {
                let mut line = format!("- [{}] {}", if todo.done { "x" } else { " " }, todo.text);
                let extra: Vec<&str> =
                    [todo.owner.as_deref(), todo.due.as_deref()].into_iter().flatten().filter(|s| !s.is_empty()).collect();
                if !extra.is_empty() {
                    line += &format!(" ({})", extra.join(", "));
                }
                line
            })
            .collect();
        out += &lines.join("\n");
        out += "\n\n";
    }
    if !note.lines.is_empty() {
        out += &format!("## {}\n\n", t("Transcript"));
        let lines: Vec<String> = note
            .lines
            .iter()
            .map(|l| {
                let name = note.speakers.iter().find(|s| s.id == l.speaker).map(|s| s.name.as_str()).unwrap_or(&l.speaker);
                format!("**{name}** ({}): {}", builder::clock(l.start), l.text)
            })
            .collect();
        out += &lines.join("\n\n");
        out += "\n";
    }
    out
}

// ── Commands ──────────────────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotesState {
    notes: Vec<Note>,
    recording: Option<String>,
    working: Vec<String>,
    parakeet: bool,
    speakers_model: bool,
    speakers_model_mb: u64,
}

#[tauri::command]
pub fn notes_state(taker: State<Arc<NoteTaker>>) -> NotesState {
    NotesState {
        notes: taker.notes.lock().unwrap().clone(),
        recording: taker.recording.lock().unwrap().as_ref().map(|r| r.id.clone()),
        working: taker.working.lock().unwrap().clone(),
        parakeet: stt::is_ready(),
        speakers_model: diarizer::is_ready(),
        speakers_model_mb: diarizer::total_mb(),
    }
}

#[tauri::command]
pub async fn notes_start(app: AppHandle) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || start(&app)).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn notes_stop(app: AppHandle) {
    let _ = tauri::async_runtime::spawn_blocking(move || stop(&app)).await;
}

#[tauri::command]
pub fn notes_rename(app: AppHandle, taker: State<Arc<NoteTaker>>, id: String, title: String) {
    let title = title.trim().to_string();
    if title.is_empty() {
        return;
    }
    taker.change(&app, &id, |n| {
        n.title = title;
        n.title_edited = true;
    });
}

#[tauri::command]
pub fn notes_rename_speaker(app: AppHandle, taker: State<Arc<NoteTaker>>, id: String, speaker: String, name: String) {
    let name = name.trim().to_string();
    if name.is_empty() {
        return;
    }
    taker.change(&app, &id, |n| {
        if let Some(s) = n.speakers.iter_mut().find(|s| s.id == speaker) {
            s.name = name;
        }
    });
}

#[tauri::command]
pub fn notes_toggle_todo(app: AppHandle, taker: State<Arc<NoteTaker>>, id: String, todo: String) {
    taker.change(&app, &id, |n| {
        if let Some(t) = n.todos.iter_mut().find(|t| t.id == todo) {
            t.done = !t.done;
        }
    });
}

#[tauri::command]
pub fn notes_delete(app: AppHandle, taker: State<Arc<NoteTaker>>, id: String) {
    if taker.recording.lock().unwrap().as_ref().is_some_and(|r| r.id == id) {
        return;
    }
    taker.notes.lock().unwrap().retain(|n| n.id != id);
    let _ = std::fs::remove_dir_all(note_dir(&id));
    changed(&app, &id);
}

#[tauri::command]
pub fn notes_summarize(app: AppHandle, id: String) {
    std::thread::spawn(move || summarize(&app, &id));
}

#[tauri::command]
pub fn notes_retry(app: AppHandle, id: String) {
    std::thread::spawn(move || process(&app, &id));
}

#[tauri::command]
pub fn notes_markdown(taker: State<Arc<NoteTaker>>, id: String) -> Option<String> {
    taker.note(&id).map(|n| markdown(&n))
}

/// Saves the note as Markdown in Documents (or the home folder) and shows it.
#[tauri::command]
pub fn notes_export(taker: State<Arc<NoteTaker>>, id: String) -> Result<String, String> {
    let note = taker.note(&id).ok_or("no such note")?;
    let home = crate::platform::home_dir();
    let dir = [home.join("Documents"), home.clone()].into_iter().find(|d| d.is_dir()).unwrap_or(home);
    let safe: String = note
        .title
        .chars()
        .map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let base = if safe.is_empty() { "ARIA notes".to_string() } else { safe };
    let mut path = dir.join(format!("{base}.md"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{base} ({n}).md"));
        n += 1;
    }
    std::fs::write(&path, markdown(&note)).map_err(|e| e.to_string())?;
    crate::log::line(format!("notes: exported to {}", path.display()));
    crate::platform::reveal_folder(&path.to_string_lossy());
    Ok(path.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn notes_download_speakers(app: AppHandle) -> Result<(), String> {
    diarizer::ensure(&app).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn el_audio_del_pc_se_rellena_hasta_el_del_microfono() {
        let dir = std::env::temp_dir().join(format!("aria-notes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut mic = Channel::new(&dir.join("mic.wav")).unwrap();
        let mut system = Channel::new(&dir.join("system.wav")).unwrap();
        mic.push(&vec![0.5; RATE]);
        system.push(&vec![0.25; RATE / 4]);
        align(&mic, &mut system, false);
        assert_eq!(system.total, RATE / 4, "playing, only a little behind: left as it is");
        align(&mic, &mut system, true);
        assert_eq!(system.total, RATE);
        mic.close();
        system.close();
        let back = read_wav(&dir.join("system.wav")).unwrap();
        assert_eq!(back.len(), RATE);
        assert!((back[0] - 0.25).abs() < 0.001);
        assert_eq!(back[RATE - 1], 0.0);
        let bytes = std::fs::read(dir.join("mic.wav")).unwrap();
        assert_eq!(u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]), RATE as u32 * 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn el_directo_corta_en_las_pausas() {
        let dir = std::env::temp_dir().join(format!("aria-notes-live-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut c = Channel::new(&dir.join("a.wav")).unwrap();
        c.push(&vec![0.3; RATE]);
        assert!(c.take_chunk().is_none(), "too short");
        c.push(&vec![0.0; RATE]);
        let (start, chunk) = c.take_chunk().expect("ends on a pause");
        assert_eq!((start, chunk.len()), (0.0, 2 * RATE));
        c.push(&vec![0.0; 2 * RATE]);
        assert!(c.take_chunk().is_none(), "silence is not a line");
        c.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// With Parakeet and the speaker models on disk:
    /// `ARIA_TEST_CALL=llamada.wav cargo test -- --ignored una_llamada_grabada --nocapture`.
    #[test]
    #[ignore]
    fn una_llamada_grabada_se_escribe_persona_por_persona() {
        let samples = read_wav(Path::new(&std::env::var("ARIA_TEST_CALL").expect("ARIA_TEST_CALL"))).unwrap();
        let heard = stt::transcribe_timed(&samples).expect("Parakeet");
        println!("TEXT: {}", heard.text);
        println!("WORDS: {:?}", heard.words.iter().map(|w| format!("{}@{:.1}", w.text, w.start)).collect::<Vec<_>>());
        let turns = diarizer::diarize(&samples);
        println!("TURNS: {turns:?}");
        let (speakers, lines) = builder::build(&[], &heard.words, &turns, "Yo", |n| format!("Persona {n}"));
        for l in &lines {
            let name = speakers.iter().find(|s| s.id == l.speaker).map(|s| s.name.as_str()).unwrap_or("?");
            println!("{name} [{:.1}-{:.1}] {}", l.start, l.end, l.text);
        }
        assert!(speakers.len() >= 2);
    }

    #[test]
    fn el_markdown_lleva_resumen_tareas_y_transcripcion() {
        let note = Note {
            id: "n".into(),
            title: "Revisión".into(),
            title_edited: false,
            start: 0,
            end: Some(300_000),
            app: None,
            status: Status::Ready,
            speakers: vec![Speaker { id: "me".into(), name: "Miller".into(), color: 0, is_me: true }],
            lines: vec![Line { id: "l".into(), speaker: "me".into(), start: 65.0, end: 66.0, text: "Hola".into() }],
            summary: Some("Se decidió lanzar.".into()),
            key_points: vec!["Lanzar el lunes".into()],
            todos: vec![Todo { id: "t".into(), text: "Enviar".into(), owner: Some("Ana".into()), due: None, done: true }],
            error: None,
            start_local: "2026-10-15 11:30".into(),
        };
        let md = markdown(&note);
        assert!(md.starts_with("# Revisión\n"));
        assert!(md.contains("5 min"));
        assert!(md.contains("Se decidió lanzar."));
        assert!(md.contains("- [x] Enviar (Ana)"));
        assert!(md.contains("**Miller** (1:05): Hola"));
    }
}
