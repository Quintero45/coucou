// Dictation and meetings: the microphone (and, for meetings, what the PC is
// playing, through WASAPI loopback) captured with cpal, downmixed and resampled
// to 16 kHz mono, transcribed locally by whisper.cpp's prebuilt whisper-cli.exe.
//
// whisper-cli and its model are downloaded at first use into
// %LOCALAPPDATA%\Coucou\voice\whisper\ (progress on `voice-engine`). Meeting
// transcripts go to %LOCALAPPDATA%\Coucou\meetings\<timestamp>.txt; each ~45 s
// chunk is also sent to the chosen Grok Bot, and at the end the whole transcript
// with a request for a summary and tasks.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter};

use crate::voice::{self, download, emit_ready, extract_zip, find_file, install_lock, now_ms, tmp_dir, tmp_file};
use crate::{grokbot, log, platform, settings};

/// Whisper model. ggml-base.bin (142 MB) is the fallback if small is too slow.
const WHISPER_MODEL: &str = "ggml-small.bin";
const MODEL_BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";
const WHISPER_RELEASES: &str = "https://api.github.com/repos/ggml-org/whisper.cpp/releases?per_page=15";
const WHISPER_ASSET: &str = "whisper-bin-x64.zip";
/// Known-good release with the CPU x64 build, if the API cannot be reached.
const WHISPER_FALLBACK: &str = "https://github.com/ggml-org/whisper.cpp/releases/download/v1.9.2/whisper-bin-x64.zip";

pub(crate) const RATE: usize = 16_000;
const MEETING_CHUNK_SECS: usize = 45;
const DICTATION_CHUNK_SECS: usize = 6;
/// Never buffer more than this per source (10 min), whatever happens downstream.
const MAX_BUFFER: usize = RATE * 600;
/// Below this RMS a chunk is silence: whisper would only hallucinate on it.
const SILENCE_RMS: f32 = 0.002;

// ── whisper-cli ───────────────────────────────────────────────────────────────

fn whisper_dir() -> PathBuf {
    voice::voice_dir().join("whisper")
}

fn whisper_exe() -> Option<PathBuf> {
    find_file(&whisper_dir(), if cfg!(windows) { "whisper-cli.exe" } else { "whisper-cli" }, 2)
}

fn model_path() -> PathBuf {
    whisper_dir().join(WHISPER_MODEL)
}

async fn whisper_release_url() -> String {
    let found = async {
        let client = reqwest::Client::builder().user_agent("Coucou").timeout(Duration::from_secs(15)).build().ok()?;
        let releases: Value = client.get(WHISPER_RELEASES).send().await.ok()?.json().await.ok()?;
        releases.as_array()?.iter().find_map(|r| {
            r["assets"].as_array()?.iter().find_map(|a| {
                (a["name"].as_str()? == WHISPER_ASSET).then(|| a["browser_download_url"].as_str().map(str::to_string))?
            })
        })
    }
    .await;
    found.unwrap_or_else(|| WHISPER_FALLBACK.to_string())
}

/// whisper-cli.exe and the model, downloaded on first use.
pub(crate) async fn ensure_whisper(app: &AppHandle) -> Result<(PathBuf, PathBuf), String> {
    if let (Some(exe), true) = (whisper_exe(), model_path().is_file()) {
        return Ok((exe, model_path()));
    }
    let _guard = install_lock().lock().await;
    if whisper_exe().is_none() {
        let url = whisper_release_url().await;
        let zip = whisper_dir().join(WHISPER_ASSET);
        // The latest release may differ from the one a leftover .part came from.
        let _ = std::fs::remove_file(voice::part_path(&zip));
        download(app, "whisper", "whisper", &url, &zip).await?;
        let result = extract_zip(zip.clone(), whisper_dir()).await;
        let _ = std::fs::remove_file(&zip);
        result?;
    }
    let exe = whisper_exe().ok_or("whisper-cli.exe no apareció tras descomprimir")?;
    if !model_path().is_file() {
        download(app, "whisper", WHISPER_MODEL, &format!("{MODEL_BASE_URL}/{WHISPER_MODEL}"), &model_path()).await?;
    }
    emit_ready(app, "whisper", true);
    Ok((exe, model_path()))
}

/// Finishes a Whisper download a dropped connection or a restart left half done.
pub(crate) async fn resume_pending(app: &AppHandle) {
    let zip_pending = voice::part_path(&whisper_dir().join(WHISPER_ASSET)).exists() && whisper_exe().is_none();
    let model_pending = voice::part_path(&model_path()).exists() && !model_path().is_file();
    if !zip_pending && !model_pending {
        return;
    }
    log::line("voice: resuming the Whisper download");
    if let Err(e) = ensure_whisper(app).await {
        log::line(format!("voice: Whisper still missing: {e}"));
    }
}

fn wav_16k(samples: &[f32]) -> Vec<u8> {
    let mut pcm = Vec::with_capacity(samples.len() * 2);
    for s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        pcm.extend_from_slice(&v.to_le_bytes());
    }
    voice::wav_from_pcm16(&pcm, RATE as u32)
}

pub(crate) fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

/// Lines whisper produces on silence or music rather than speech.
fn is_noise(line: &str) -> bool {
    let l = line.trim().to_lowercase();
    l.is_empty()
        || (l.starts_with('[') && l.ends_with(']'))
        || (l.starts_with('(') && l.ends_with(')'))
        || l.contains("amara.org")
        || l.contains("subtítulos realizados por")
        || l.contains("subtitulado por")
        || l == "gracias." && line.len() < 10
}

/// Spanish text for 16 kHz mono samples ("" for silence).
pub(crate) async fn transcribe(exe: PathBuf, model: PathBuf, samples: Vec<f32>) -> Result<String, String> {
    if samples.len() < RATE / 2 || rms(&samples) < SILENCE_RMS {
        return Ok(String::new());
    }
    tauri::async_runtime::spawn_blocking(move || {
        std::fs::create_dir_all(tmp_dir()).map_err(|e| e.to_string())?;
        let wav = tmp_file("chunk", "wav");
        std::fs::write(&wav, wav_16k(&samples)).map_err(|e| e.to_string())?;
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8);
        let mut cmd = Command::new(&exe);
        cmd.arg("-m")
            .arg(&model)
            .arg("-f")
            .arg(&wav)
            .args(["-l", "es", "-nt", "-np", "-t", &threads.to_string()])
            .current_dir(exe.parent().unwrap_or(std::path::Path::new(".")))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let out = platform::no_console(&mut cmd).output();
        let _ = std::fs::remove_file(&wav);
        let out = out.map_err(|e| format!("whisper: {e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
            return Err(format!("whisper falló: {last}"));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !is_noise(l)).collect();
        Ok(lines.join(" "))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Where to cut `buf` near `target` samples: the quietest 100 ms in the 3 s
/// before it, so words are not split between chunks.
fn split_point(buf: &[f32], target: usize) -> usize {
    let target = target.min(buf.len());
    let block = RATE / 10;
    let from = target.saturating_sub(RATE * 3);
    let mut best = (f32::MAX, target);
    let mut start = from;
    while start + block <= target {
        let e = rms(&buf[start..start + block]);
        if e < best.0 {
            best = (e, start + block / 2);
        }
        start += block;
    }
    best.1.max(1)
}

// ── Capture ───────────────────────────────────────────────────────────────────

/// Linear resampler to 16 kHz with a one-pole low-pass in front (cheap anti-alias).
struct Resampler {
    step: f64,
    pos: f64,
    last: f32,
    lp: f32,
    alpha: f32,
}

impl Resampler {
    fn new(rate: u32) -> Self {
        let rate = rate.max(1) as f32;
        let cutoff = 7_000f32.min(rate * 0.45);
        Resampler {
            step: rate as f64 / RATE as f64,
            pos: 0.0,
            last: 0.0,
            lp: 0.0,
            alpha: 1.0 - (-2.0 * std::f32::consts::PI * cutoff / rate).exp(),
        }
    }

    fn push(&mut self, input: &mut [f32], out: &mut Vec<f32>) {
        if input.is_empty() {
            return;
        }
        if self.step > 1.0 {
            for x in input.iter_mut() {
                self.lp += self.alpha * (*x - self.lp);
                *x = self.lp;
            }
        }
        let len = input.len();
        let at = |i: isize, last: f32| if i < 0 { last } else { input[(i as usize).min(len - 1)] };
        while self.pos <= (len - 1) as f64 {
            let i = self.pos.floor() as isize;
            let frac = (self.pos - i as f64) as f32;
            let a = at(i, self.last);
            let b = at(i + 1, self.last);
            out.push(a + (b - a) * frac);
            self.pos += self.step;
        }
        self.pos -= len as f64;
        self.last = input[len - 1];
    }
}

type Buffer = Arc<Mutex<Vec<f32>>>;

/// Running capture: a thread owns the cpal streams (they are not Send) and
/// fills one 16 kHz buffer per source until stopped.
pub(crate) struct Capture {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    mic: Buffer,
    sys: Buffer,
}

impl Capture {
    pub(crate) fn start(mic: bool, system: bool) -> Result<Capture, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let mic_buf: Buffer = Arc::default();
        let sys_buf: Buffer = Arc::default();
        let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let thread = {
            let (stop, mic_buf, sys_buf) = (stop.clone(), mic_buf.clone(), sys_buf.clone());
            std::thread::Builder::new()
                .name("coucou-capture".into())
                .spawn(move || capture_thread(mic, system, stop, mic_buf, sys_buf, tx))
                .map_err(|e| e.to_string())?
        };
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(Capture { stop, thread: Some(thread), mic: mic_buf, sys: sys_buf }),
            Ok(Err(e)) => {
                stop.store(true, Ordering::SeqCst);
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                stop.store(true, Ordering::SeqCst);
                Err("audio_timeout".into())
            }
        }
    }

    /// Everything captured so far, both sources mixed, and clears the buffers.
    pub(crate) fn take(&self) -> Vec<f32> {
        let mic = std::mem::take(&mut *self.mic.lock().unwrap());
        let sys = std::mem::take(&mut *self.sys.lock().unwrap());
        match (mic.is_empty(), sys.is_empty()) {
            (_, true) => mic,
            (true, false) => sys,
            (false, false) => {
                let n = mic.len().max(sys.len());
                (0..n)
                    .map(|i| (mic.get(i).copied().unwrap_or(0.0) + sys.get(i).copied().unwrap_or(0.0)).clamp(-1.0, 1.0))
                    .collect()
            }
        }
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(windows)]
fn capture_thread(
    mic: bool,
    system: bool,
    stop: Arc<AtomicBool>,
    mic_buf: Buffer,
    sys_buf: Buffer,
    ready: std::sync::mpsc::Sender<Result<(), String>>,
) {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let mut streams = Vec::new();
    let mut errors = Vec::new();
    if mic {
        let opened = host
            .default_input_device()
            .ok_or_else(|| "no hay micrófono".to_string())
            .and_then(|d| {
                let cfg = d.default_input_config().map_err(|e| e.to_string())?;
                open_stream(&d, &cfg, mic_buf, "mic")
            });
        match opened {
            Ok(s) => streams.push(s),
            Err(e) => errors.push(format!("micrófono: {e}")),
        }
    }
    if system {
        // WASAPI loopback: an input stream built on the render (output) device.
        let opened = host
            .default_output_device()
            .ok_or_else(|| "no hay salida de audio".to_string())
            .and_then(|d| {
                let cfg = d.default_output_config().map_err(|e| e.to_string())?;
                open_stream(&d, &cfg, sys_buf, "system")
            });
        match opened {
            Ok(s) => streams.push(s),
            Err(e) => errors.push(format!("audio del sistema: {e}")),
        }
    }
    streams.retain(|s| match s.play() {
        Ok(()) => true,
        Err(e) => {
            errors.push(e.to_string());
            false
        }
    });
    for e in &errors {
        log::line(format!("capture: {e}"));
    }
    if streams.is_empty() {
        let _ = ready.send(Err(if errors.is_empty() { "sin fuentes de audio".into() } else { errors.join("; ") }));
        return;
    }
    let _ = ready.send(Ok(()));
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(streams);
}

#[cfg(windows)]
fn open_stream(
    device: &cpal::Device,
    supported: &cpal::SupportedStreamConfig,
    buf: Buffer,
    label: &'static str,
) -> Result<cpal::Stream, String> {
    let config = supported.config();
    match supported.sample_format() {
        cpal::SampleFormat::F32 => build_stream::<f32>(device, &config, buf, label),
        cpal::SampleFormat::I16 => build_stream::<i16>(device, &config, buf, label),
        cpal::SampleFormat::U16 => build_stream::<u16>(device, &config, buf, label),
        cpal::SampleFormat::I32 => build_stream::<i32>(device, &config, buf, label),
        other => Err(format!("formato de audio no soportado: {other:?}")),
    }
}

#[cfg(windows)]
fn build_stream<T>(device: &cpal::Device, config: &cpal::StreamConfig, buf: Buffer, label: &'static str) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    use cpal::traits::DeviceTrait;
    let channels = (config.channels as usize).max(1);
    let mut resampler = Resampler::new(config.sample_rate.0);
    let mut mono: Vec<f32> = Vec::new();
    let mut out: Vec<f32> = Vec::new();
    device
        .build_input_stream(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                mono.clear();
                for frame in data.chunks(channels) {
                    let sum: f32 = frame.iter().map(|s| s.to_sample::<f32>()).sum();
                    mono.push(sum / frame.len() as f32);
                }
                out.clear();
                resampler.push(&mut mono, &mut out);
                if let Ok(mut b) = buf.lock() {
                    if b.len() + out.len() <= MAX_BUFFER {
                        b.extend_from_slice(&out);
                    }
                }
            },
            move |e| log::line(format!("capture {label}: {e}")),
            None,
        )
        .map_err(|e| e.to_string())
}

#[cfg(not(windows))]
fn capture_thread(
    _mic: bool,
    _system: bool,
    _stop: Arc<AtomicBool>,
    _mic_buf: Buffer,
    _sys_buf: Buffer,
    ready: std::sync::mpsc::Sender<Result<(), String>>,
) {
    let _ = ready.send(Err("La captura de audio solo está disponible en Windows por ahora.".into()));
}

// ── Dictation ─────────────────────────────────────────────────────────────────

struct Dictation {
    capture: Arc<Mutex<Capture>>,
    stop: Arc<AtomicBool>,
    text: Arc<Mutex<String>>,
    pending: Arc<Mutex<Vec<f32>>>,
    worker: tauri::async_runtime::JoinHandle<()>,
    exe: PathBuf,
    model: PathBuf,
}

fn dictation() -> &'static Mutex<Option<Dictation>> {
    static D: OnceLock<Mutex<Option<Dictation>>> = OnceLock::new();
    D.get_or_init(|| Mutex::new(None))
}

fn append(text: &Mutex<String>, piece: &str) -> String {
    let mut t = text.lock().unwrap();
    if !piece.is_empty() {
        if !t.is_empty() {
            t.push(' ');
        }
        t.push_str(piece);
    }
    t.clone()
}

#[derive(Serialize, Clone)]
struct Partial {
    text: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct DictationResult {
    pub text: String,
}

#[tauri::command]
pub async fn start_dictation(app: AppHandle) -> Result<(), String> {
    if dictation().lock().unwrap().is_some() {
        return Ok(());
    }
    let (exe, model) = ensure_whisper(&app).await?;
    let capture = Arc::new(Mutex::new(Capture::start(true, false)?));
    let stop = Arc::new(AtomicBool::new(false));
    let text = Arc::new(Mutex::new(String::new()));
    let pending: Arc<Mutex<Vec<f32>>> = Arc::default();
    let worker = {
        let (app, capture, stop, text, pending) = (app.clone(), capture.clone(), stop.clone(), text.clone(), pending.clone());
        let (exe, model) = (exe.clone(), model.clone());
        tauri::async_runtime::spawn(async move {
            while !stop.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(500)).await;
                let piece = {
                    let fresh = capture.lock().unwrap().take();
                    let mut p = pending.lock().unwrap();
                    p.extend_from_slice(&fresh);
                    if p.len() < RATE * DICTATION_CHUNK_SECS {
                        continue;
                    }
                    let cut = split_point(&p, RATE * DICTATION_CHUNK_SECS);
                    p.drain(..cut).collect::<Vec<f32>>()
                };
                match transcribe(exe.clone(), model.clone(), piece).await {
                    Ok(t) if !t.is_empty() => {
                        let all = append(&text, &t);
                        let _ = app.emit("dictation-partial", Partial { text: all });
                    }
                    Ok(_) => {}
                    Err(e) => log::line(format!("dictation: {e}")),
                }
            }
        })
    };
    let mut slot = dictation().lock().unwrap();
    if slot.is_some() {
        // Another start won the race: keep that one.
        stop.store(true, Ordering::SeqCst);
        return Ok(());
    }
    *slot = Some(Dictation { capture, stop, text, pending, worker, exe, model });
    log::line("dictation started");
    Ok(())
}

#[tauri::command]
pub async fn stop_dictation(app: AppHandle) -> Result<DictationResult, String> {
    let Some(d) = dictation().lock().unwrap().take() else {
        return Ok(DictationResult { text: String::new() });
    };
    d.stop.store(true, Ordering::SeqCst);
    let _ = d.worker.await;
    let rest = {
        let mut capture = d.capture.lock().unwrap();
        let fresh = capture.take();
        capture.stop();
        let mut p = d.pending.lock().unwrap();
        p.extend_from_slice(&fresh);
        std::mem::take(&mut *p)
    };
    let text = match transcribe(d.exe, d.model, rest).await {
        Ok(t) => append(&d.text, &t),
        Err(e) => {
            log::line(format!("dictation: {e}"));
            d.text.lock().unwrap().clone()
        }
    };
    let _ = app.emit("dictation-partial", Partial { text: text.clone() });
    log::line(format!("dictation stopped ({} chars)", text.chars().count()));
    Ok(DictationResult { text })
}

// ── Meetings ──────────────────────────────────────────────────────────────────

#[derive(Deserialize, Default, Debug, Clone, Copy)]
#[serde(default)]
pub struct MeetingSources {
    pub mic: bool,
    pub system: bool,
}

#[derive(Serialize, Clone)]
struct MeetingState {
    active: bool,
    bot: String,
    since: u64,
}

#[derive(Serialize, Clone)]
struct MeetingChunk {
    bot: String,
    text: String,
    ts: u64,
}

#[derive(Serialize, Clone, Debug)]
pub struct MeetingResult {
    pub path_transcript: String,
}

/// What every chunk needs, shared by the worker and stop_meeting.
#[derive(Clone)]
struct MeetingCtx {
    app: AppHandle,
    bot: String,
    path: PathBuf,
    exe: PathBuf,
    model: PathBuf,
    count: Arc<Mutex<u32>>,
}

struct Meeting {
    ctx: MeetingCtx,
    since: u64,
    capture: Arc<Mutex<Capture>>,
    pending: Arc<Mutex<Vec<f32>>>,
    stop: Arc<AtomicBool>,
    worker: tauri::async_runtime::JoinHandle<()>,
}

fn meeting() -> &'static Mutex<Option<Meeting>> {
    static M: OnceLock<Mutex<Option<Meeting>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(None))
}

fn clock() -> String {
    let t = platform::local_time();
    format!("{:02}:{:02}:{:02}", t.hour, t.minute, t.second)
}

fn append_file(path: &PathBuf, line: &str) {
    use std::io::Write;
    match std::fs::OpenOptions::new().create(true).append(true).open(path) {
        Ok(mut f) => {
            let _ = f.write_all(line.as_bytes());
        }
        Err(e) => log::line(format!("meeting: transcript not written: {e}")),
    }
}

/// Fire-and-forget send to the Bot; failures are logged, never fatal.
fn send_to_bot(app: &AppHandle, bot: &str, message: String) {
    let (app, bot) = (app.clone(), bot.to_string());
    tauri::async_runtime::spawn(async move {
        if let Err(code) = grokbot::send(&app, &bot, &message, Vec::new()).await {
            log::line(format!("meeting: send to {bot} failed: {code}"));
        }
    });
}

async fn process_chunk(ctx: &MeetingCtx, samples: Vec<f32>) {
    let text = match transcribe(ctx.exe.clone(), ctx.model.clone(), samples).await {
        Ok(t) => t,
        Err(e) => {
            log::line(format!("meeting: {e}"));
            return;
        }
    };
    if text.is_empty() {
        return;
    }
    let n = {
        let mut c = ctx.count.lock().unwrap();
        *c += 1;
        *c
    };
    let at = clock();
    append_file(&ctx.path, &format!("[{at}] {text}\n"));
    let _ = ctx.app.emit("meeting-chunk", MeetingChunk { bot: ctx.bot.clone(), text: text.clone(), ts: now_ms() });
    send_to_bot(
        &ctx.app,
        &ctx.bot,
        format!("[ARIA · reunión en curso · fragmento {n} · {at}] Transcripción parcial, solo para tu contexto (no hace falta responder):\n{text}"),
    );
}

/// Starts recording a meeting. Sources come as `options`/`sources: {mic, system}`
/// or as top-level `mic` / `system`; default is both.
#[tauri::command]
pub async fn start_meeting(
    app: AppHandle,
    bot: String,
    options: Option<MeetingSources>,
    sources: Option<MeetingSources>,
    mic: Option<bool>,
    system: Option<bool>,
) -> Result<(), String> {
    let chosen = options.or(sources);
    let mic = mic.or(chosen.map(|s| s.mic)).unwrap_or(true);
    let system = system.or(chosen.map(|s| s.system)).unwrap_or(true);
    if !mic && !system {
        return Err("no_sources".into());
    }
    if meeting().lock().unwrap().is_some() {
        return Err("meeting_active".into());
    }
    let bot = voice::resolve_bot(&app, &bot);
    let (exe, model) = ensure_whisper(&app).await?;
    let capture = Arc::new(Mutex::new(Capture::start(mic, system)?));

    let t = platform::local_time();
    let dir = settings::local_dir().join("meetings");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!(
        "{:04}-{:02}-{:02}_{:02}-{:02}-{:02}.txt",
        t.year, t.month, t.day, t.hour, t.minute, t.second
    ));
    let sources_label = match (mic, system) {
        (true, true) => "micrófono + sistema",
        (true, false) => "micrófono",
        _ => "sistema",
    };
    append_file(
        &path,
        &format!(
            "Reunión {:04}-{:02}-{:02} {:02}:{:02} · Bot: {bot} · Fuentes: {sources_label}\n\n",
            t.year, t.month, t.day, t.hour, t.minute
        ),
    );

    let since = now_ms();
    let ctx = MeetingCtx { app: app.clone(), bot: bot.clone(), path, exe, model, count: Arc::default() };
    let stop = Arc::new(AtomicBool::new(false));
    let pending: Arc<Mutex<Vec<f32>>> = Arc::default();
    let worker = {
        let (ctx, capture, stop, pending) = (ctx.clone(), capture.clone(), stop.clone(), pending.clone());
        tauri::async_runtime::spawn(async move {
            while !stop.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let chunk = {
                    let fresh = capture.lock().unwrap().take();
                    let mut p = pending.lock().unwrap();
                    p.extend_from_slice(&fresh);
                    if p.len() < RATE * MEETING_CHUNK_SECS {
                        continue;
                    }
                    let cut = split_point(&p, RATE * MEETING_CHUNK_SECS);
                    p.drain(..cut).collect::<Vec<f32>>()
                };
                process_chunk(&ctx, chunk).await;
            }
        })
    };
    {
        let mut slot = meeting().lock().unwrap();
        if slot.is_some() {
            stop.store(true, Ordering::SeqCst);
            return Err("meeting_active".into());
        }
        *slot = Some(Meeting { ctx, since, capture, pending, stop, worker });
    }
    let _ = app.emit("meeting-state", MeetingState { active: true, bot: bot.clone(), since });
    log::line(format!("meeting started with {bot} ({sources_label})"));
    Ok(())
}

/// The transcript as a file card on the Bot's pill (`bot-attach` {bot, path,
/// name, mime, size}), through botcards' own path so open_attachment accepts it.
fn announce_transcript(app: &AppHandle, bot: &str, path: &str) {
    let name = grokbot::list(app)
        .into_iter()
        .find(|b| b.id == bot)
        .map(|b| b.name)
        .unwrap_or_else(|| bot.to_string());
    let payload = serde_json::json!({
        "coucou_bot": name,
        "coucou_agent": format!("bot-{}", grokbot::slug(&name)),
        "path": path,
        "caption": "Transcripción de la reunión",
    });
    // TODO(Aerys): switch to `crate::botcards::announce_attachment(app, bot, path, caption)` once it exists;
    // until then botcards' existing pub `attach` does the same (validate, allowlist, emit `bot-attach`).
    let answer = crate::botcards::attach(app, &payload);
    if answer["ok"] != Value::Bool(true) {
        log::line(format!("meeting: transcript card not shown: {}", answer["error"]));
    }
}

#[tauri::command]
pub async fn stop_meeting(app: AppHandle) -> Result<MeetingResult, String> {
    let Some(m) = meeting().lock().unwrap().take() else {
        return Err("no_meeting".into());
    };
    m.stop.store(true, Ordering::SeqCst);
    let _ = m.worker.await;
    let rest = {
        let mut capture = m.capture.lock().unwrap();
        let fresh = capture.take();
        capture.stop();
        let mut p = m.pending.lock().unwrap();
        p.extend_from_slice(&fresh);
        std::mem::take(&mut *p)
    };
    process_chunk(&m.ctx, rest).await;

    let t = platform::local_time();
    append_file(&m.ctx.path, &format!("\n— Fin {:02}:{:02} —\n", t.hour, t.minute));
    let _ = app.emit("meeting-state", MeetingState { active: false, bot: m.ctx.bot.clone(), since: m.since });

    let path_str = m.ctx.path.to_string_lossy().to_string();
    announce_transcript(&app, &m.ctx.bot, &path_str);
    let transcript = std::fs::read_to_string(&m.ctx.path).unwrap_or_default();
    if *m.ctx.count.lock().unwrap() > 0 {
        // Stay well under grokbot's 2 MB body ceiling.
        const MAX: usize = 1_500_000;
        let body = if transcript.len() > MAX {
            let mut end = MAX;
            while !transcript.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}\n[…recortada; completa en {path_str}]", &transcript[..end])
        } else {
            transcript
        };
        send_to_bot(
            &app,
            &m.ctx.bot,
            format!(
                "[ARIA · reunión terminada] Aquí está la transcripción completa. Hazme un resumen en español con: \
                 puntos clave, decisiones tomadas, tareas (con responsable y fecha si se mencionan) y próximos pasos.\n\
                 (Archivo en el PC: {path_str})\n\n{body}"
            ),
        );
    }
    log::line(format!("meeting stopped: {path_str}"));
    Ok(MeetingResult { path_transcript: path_str })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_rate() {
        let mut r = Resampler::new(48_000);
        let mut out = Vec::new();
        for _ in 0..100 {
            let mut buf = vec![0.1f32; 480];
            r.push(&mut buf, &mut out);
        }
        // 1 s at 48 kHz → ~16000 samples
        assert!((out.len() as i64 - 16_000).abs() <= 2, "{}", out.len());
    }

    #[test]
    fn split_prefers_silence() {
        let mut buf = vec![0.5f32; RATE * 10];
        for s in &mut buf[RATE * 8..RATE * 8 + RATE / 10] {
            *s = 0.0;
        }
        let cut = split_point(&buf, RATE * 9);
        assert!(cut >= RATE * 8 && cut <= RATE * 8 + RATE / 10);
    }

    #[test]
    fn noise_lines_dropped() {
        assert!(is_noise("[Música]"));
        assert!(is_noise("Subtítulos realizados por la comunidad de Amara.org"));
        assert!(!is_noise("Hola equipo, empecemos."));
    }
}
