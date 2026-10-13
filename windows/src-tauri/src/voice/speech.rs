// ARIA's voice: Piper (es-MX, downloaded with "Hey ARIA") when it is there,
// else a Windows voice (Sabina) through System.Speech. Nothing is written to
// disk: Piper's raw audio is played from memory.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

use crate::{log, platform};

/// What is said: no markdown, no links, no code — a few sentences at most.
pub fn speakable(text: &str) -> String {
    let mut out = String::new();
    let mut in_code = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        let line = line.trim().trim_start_matches(['#', '>', '-', '*', '•']).trim();
        if line.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(line);
    }
    let mut cleaned = String::with_capacity(out.len());
    for word in out.split_whitespace() {
        if word.starts_with("http://") || word.starts_with("https://") {
            continue;
        }
        let w: String = word.chars().filter(|c| !matches!(c, '*' | '`' | '_' | '#' | '[' | ']')).collect();
        if w.is_empty() {
            continue;
        }
        if !cleaned.is_empty() {
            cleaned.push(' ');
        }
        cleaned.push_str(&w);
    }
    let mut chars = 0;
    let mut cut = cleaned.len();
    for (i, c) in cleaned.char_indices() {
        chars += 1;
        if chars > 400 && matches!(c, '.' | '!' | '?') {
            cut = i + c.len_utf8();
            break;
        }
    }
    cleaned.truncate(cut);
    cleaned
}

/// Bumped by every `say` and `stop`: an older speech sees it and ends.
static TURN: AtomicU32 = AtomicU32::new(0);
static SPEAKING: AtomicBool = AtomicBool::new(false);
static CHILD: Mutex<Option<std::process::Child>> = Mutex::new(None);

pub fn speaking() -> bool {
    SPEAKING.load(Ordering::SeqCst)
}

pub fn stop() {
    TURN.fetch_add(1, Ordering::SeqCst);
    if let Some(mut child) = CHILD.lock().unwrap().take() {
        let _ = child.kill();
    }
}

/// Says `text` and returns when done (or stopped). Blocking: call it off the UI.
pub fn say(text: &str, voice: &str) -> Result<(), String> {
    stop();
    let turn = TURN.load(Ordering::SeqCst);
    let text = speakable(text);
    if text.is_empty() {
        return Ok(());
    }
    SPEAKING.store(true, Ordering::SeqCst);
    let result = match super::models::piper_exe().filter(|_| voice == "piper" && super::models::speaking_ready()) {
        Some(exe) => piper(&exe, &text, turn).or_else(|e| {
            log::line(format!("voice: Piper failed ({e}), using a Windows voice"));
            sapi(&text, "", turn)
        }),
        None => sapi(&text, if voice == "piper" { "" } else { voice }, turn),
    };
    SPEAKING.store(false, Ordering::SeqCst);
    result
}

fn piper(exe: &std::path::Path, text: &str, turn: u32) -> Result<(), String> {
    use std::io::{Read, Write};
    let mut cmd = Command::new(exe);
    cmd.arg("--model")
        .arg(super::models::piper_voice())
        .arg("--output_raw")
        .current_dir(exe.parent().unwrap_or(std::path::Path::new(".")))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = platform::no_console(&mut cmd).spawn().map_err(|e| e.to_string())?;
    let mut stdin = child.stdin.take().ok_or("piper: sin stdin")?;
    let mut stdout = child.stdout.take().ok_or("piper: sin stdout")?;
    *CHILD.lock().unwrap() = Some(child);
    // One line: Piper reads a sentence per line.
    let line = text.replace(['\r', '\n'], " ") + "\n";
    stdin.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    drop(stdin);
    let mut raw = Vec::new();
    stdout.read_to_end(&mut raw).map_err(|e| e.to_string())?;
    if let Some(mut child) = CHILD.lock().unwrap().take() {
        let _ = child.wait();
    }
    if TURN.load(Ordering::SeqCst) != turn {
        return Ok(());
    }
    if raw.len() < 2 {
        return Err("piper no produjo audio".into());
    }
    let samples: Vec<i16> = raw.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
    play(samples, 22_050, turn)
}

#[cfg(windows)]
fn play(samples: Vec<i16>, rate: u32, turn: u32) -> Result<(), String> {
    let (_stream, handle) = rodio::OutputStream::try_default().map_err(|e| e.to_string())?;
    let sink = rodio::Sink::try_new(&handle).map_err(|e| e.to_string())?;
    sink.append(rodio::buffer::SamplesBuffer::new(1, rate, samples));
    while !sink.empty() {
        if TURN.load(Ordering::SeqCst) != turn {
            sink.stop();
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Ok(())
}

#[cfg(not(windows))]
fn play(_samples: Vec<i16>, _rate: u32, _turn: u32) -> Result<(), String> {
    Err("sin salida de audio fuera de Windows".into())
}

/// System.Speech: the text and the voice travel in environment variables,
/// never in the command line.
const SAPI: &str = r#"
Add-Type -AssemblyName System.Speech
$s = New-Object System.Speech.Synthesis.SpeechSynthesizer
$want = $env:ARIA_VOICE
$voices = $s.GetInstalledVoices() | Where-Object { $_.Enabled } | ForEach-Object { $_.VoiceInfo }
$pick = $null
if ($want) { $pick = $voices | Where-Object { $_.Name -eq $want } | Select-Object -First 1 }
if (-not $pick) { $pick = $voices | Where-Object { $_.Culture.Name -like 'es-*' } | Select-Object -First 1 }
if ($pick) { $s.SelectVoice($pick.Name) }
$s.Speak($env:ARIA_SAY)
"#;

/// The Windows voices, for Settings → Voice.
pub fn windows_voices() -> Vec<String> {
    let script = "Add-Type -AssemblyName System.Speech; (New-Object System.Speech.Synthesis.SpeechSynthesizer).GetInstalledVoices() | Where-Object { $_.Enabled } | ForEach-Object { $_.VoiceInfo.Name + '|' + $_.VoiceInfo.Culture.Name }";
    let mut cmd = Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let Ok(out) = platform::no_console(&mut cmd).output() else { return Vec::new() };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let (name, culture) = l.trim().split_once('|')?;
            // Spanish first; English ones too, the owner may want them.
            (culture.starts_with("es") || culture.starts_with("en")).then(|| name.to_string())
        })
        .collect()
}

fn sapi(text: &str, voice: &str, turn: u32) -> Result<(), String> {
    let encoded = {
        let utf16: Vec<u8> = SAPI.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        crate::media::base64(&utf16)
    };
    let mut cmd = Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded])
        .env("ARIA_SAY", text)
        .env("ARIA_VOICE", voice)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let child = platform::no_console(&mut cmd).spawn().map_err(|e| e.to_string())?;
    *CHILD.lock().unwrap() = Some(child);
    loop {
        if TURN.load(Ordering::SeqCst) != turn {
            return Ok(());
        }
        let mut guard = CHILD.lock().unwrap();
        match guard.as_mut().map(|c| c.try_wait()) {
            Some(Ok(Some(_))) | None | Some(Err(_)) => {
                guard.take();
                return Ok(());
            }
            Some(Ok(None)) => {}
        }
        drop(guard);
        std::thread::sleep(std::time::Duration::from_millis(80));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_words_are_spoken() {
        assert_eq!(speakable("## Hecho\n- Abrí **Spotify**.\n```\nrm -rf\n```\nMira https://x.com"), "Hecho Abrí Spotify. Mira");
        assert_eq!(speakable("   "), "");
        let long = "Frase larga de prueba. ".repeat(40);
        assert!(speakable(&long).chars().count() <= 425);
    }
}
