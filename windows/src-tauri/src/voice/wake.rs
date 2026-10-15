// The wake phrase: "Hey ARIA", "Oye ARIA", "OK ARIA". Pure parts first —
// ports of WakePhrase.swift, WakeWindowBackoff.swift and VoiceWakeFilter.swift
// — then the spotter itself (sherpa-onnx keyword spotting, Windows).

use std::time::Duration;

use super::turn::normalise;

// ── WakePhrase: the phrase in what Whisper heard ─────────────────────────────

#[derive(Debug, PartialEq, Eq)]
pub struct Split {
    pub matched: bool,
    /// What follows the last wake phrase, as heard (case and accents kept).
    pub command: String,
}

/// How Parakeet and Whisper may write the name (accents already folded):
/// «Arya», «área», and a consonant glued from the call before it — «Okay,
/// Garia», «Hola Raria», «Oye, Laria». Only counted right after a call word.
fn is_name(w: &str) -> bool {
    matches!(
        w,
        "aria" | "arya" | "ariah" | "ariia" | "arias" | "area" | "garia" | "raria" | "laria" | "yaria" | "daria" | "haria"
            | "arria"
    )
}

fn is_call(w: &str) -> bool {
    matches!(w, "hey" | "hei" | "ey" | "oye" | "oy" | "oiga" | "ok" | "okay" | "okey" | "hola" | "hi" | "hello")
}

/// An article heard between the call and the name: «Oye el área».
fn is_filler(w: &str) -> bool {
    matches!(w, "el" | "la")
}

/// Finds the last "<call> ARIA" in `raw` and returns what follows it.
/// "ARIA" alone does not wake her: it is too common in a conversation about her.
pub fn split(raw: &str) -> Split {
    let words: Vec<&str> = raw.split_whitespace().collect();
    let norm: Vec<String> = words.iter().map(|w| normalise(w)).collect();
    let mut end = None;
    for i in 1..norm.len() {
        let called = is_call(&norm[i - 1]) || (i >= 2 && is_filler(&norm[i - 1]) && is_call(&norm[i - 2]));
        if is_name(&norm[i]) && called {
            end = Some(i + 1);
        }
    }
    let Some(end) = end else { return Split { matched: false, command: String::new() } };
    let tail = words[end..].join(" ");
    let command = tail.trim_start_matches([',', '.', ';', ':', '-', '—', ' ']).trim().to_string();
    Split { matched: true, command }
}

// ── WakeWindowBackoff: reopening the microphone after failures ───────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    Open,
    Delay(Duration),
    Stop,
}

pub struct Backoff {
    pub fast_threshold: Duration,
    pub max_fast_fails: u32,
    pub base: Duration,
    pub max_delay: Duration,
    streak: u32,
}

impl Default for Backoff {
    fn default() -> Self {
        Backoff {
            fast_threshold: Duration::from_secs(1),
            max_fast_fails: 3,
            base: Duration::from_millis(500),
            max_delay: Duration::from_secs(5),
            streak: 0,
        }
    }
}

impl Backoff {
    /// The microphone stream ended after `elapsed`: reopen now, later, or give up.
    pub fn ended(&mut self, elapsed: Duration) -> Action {
        if elapsed >= self.fast_threshold {
            self.streak = 0;
            return Action::Open;
        }
        self.streak += 1;
        if self.streak >= self.max_fast_fails {
            return Action::Stop;
        }
        let delay = self.base * 2u32.pow(self.streak - 1);
        Action::Delay(delay.min(self.max_delay))
    }

    pub fn reset(&mut self) {
        self.streak = 0;
    }
}

// ── VoiceWakeFilter: when a wake is ignored ──────────────────────────────────

/// Why a wake should be ignored right now, or `None`: an approval waits for a
/// click, or the open island shows a card that needs the owner's hands.
pub fn wake_blocked(view: &str, mode: &str, pending_approval: bool) -> Option<&'static str> {
    if pending_approval {
        return Some("approval shown");
    }
    if mode != "expanded" {
        return None;
    }
    match view {
        "approval" => Some("approval shown"),
        "question" => Some("question shown"),
        "mail" => Some("mail card shown"),
        _ => None,
    }
}

// ── Keywords for the spotter ──────────────────────────────────────────────────

/// Token spellings of the wake phrases for the English zipformer KWS model
/// (its tokens.txt), several per phrase for Spanish pronunciations.
pub const KEYWORDS: &[&str] = &[
    "▁HE Y ▁A RI A @HEY_ARIA",
    "▁HI ▁A RI A @HEY_ARIA",
    "▁O Y E ▁A RI A @OYE_ARIA",
    "▁O Y ▁A RI A @OYE_ARIA",
    "▁HO Y E ▁A RI A @OYE_ARIA",
    "▁O I E ▁A RI A @OYE_ARIA",
    "▁OH ▁YEAH ▁A RI A @OYE_ARIA",
    "▁O ▁YES ▁A RI A @OYE_ARIA",
    "▁O Y E ▁A R IA @OYE_ARIA",
    "▁OKAY ▁A RI A @OK_ARIA",
];

/// The keyword lines whose tokens are all in the model's tokens.txt.
/// sherpa-onnx ends the whole process on an unknown token, so nothing else
/// may reach it.
pub fn checked_keywords(tokens_txt: &str) -> String {
    let known: std::collections::HashSet<&str> =
        tokens_txt.lines().filter_map(|l| l.split_whitespace().next()).collect();
    KEYWORDS
        .iter()
        .filter(|line| {
            let phrase = line.split('@').next().unwrap_or("");
            let mut toks = phrase.split_whitespace().peekable();
            toks.peek().is_some() && toks.all(|t| known.contains(t))
        })
        .copied()
        .collect::<Vec<_>>()
        .join("\n")
}

// ── The spotter (Windows) ─────────────────────────────────────────────────────

#[cfg(windows)]
pub use spotter::Spotter;

#[cfg(windows)]
mod spotter {
    use sherpa_rs::sherpa_rs_sys as sys;
    use std::ffi::{CStr, CString};
    use std::path::Path;

    pub struct Spotter {
        spotter: *const sys::SherpaOnnxKeywordSpotter,
    }

    // The spotter is only used from the voice thread.
    unsafe impl Send for Spotter {}

    impl Spotter {
        /// `dir`: the KWS model folder (encoder, decoder, joiner, tokens.txt).
        pub fn new(dir: &Path) -> Result<Spotter, String> {
            let file = |name: &str| dir.join(name);
            let names = [
                "encoder-epoch-12-avg-2-chunk-16-left-64.int8.onnx",
                "decoder-epoch-12-avg-2-chunk-16-left-64.int8.onnx",
                "joiner-epoch-12-avg-2-chunk-16-left-64.int8.onnx",
                "tokens.txt",
            ];
            if let Some(missing) = names.iter().find(|n| !file(n).is_file()) {
                return Err(format!("falta {missing}"));
            }
            let tokens_txt = std::fs::read_to_string(file("tokens.txt")).map_err(|e| e.to_string())?;
            let keywords = super::checked_keywords(&tokens_txt);
            if keywords.is_empty() {
                return Err("ninguna frase de activación encaja con el modelo".into());
            }
            let c = |p: std::path::PathBuf| CString::new(p.to_string_lossy().into_owned()).map_err(|e| e.to_string());
            let (enc, dec, joi, tok) = (c(file(names[0]))?, c(file(names[1]))?, c(file(names[2]))?, c(file(names[3]))?);
            let kw = CString::new(keywords.clone()).map_err(|e| e.to_string())?;
            let cpu = CString::new("cpu").unwrap();
            let spotter = unsafe {
                let mut cfg: sys::SherpaOnnxKeywordSpotterConfig = std::mem::zeroed();
                cfg.feat_config.sample_rate = 16_000;
                cfg.feat_config.feature_dim = 80;
                cfg.model_config.transducer.encoder = enc.as_ptr();
                cfg.model_config.transducer.decoder = dec.as_ptr();
                cfg.model_config.transducer.joiner = joi.as_ptr();
                cfg.model_config.tokens = tok.as_ptr();
                cfg.model_config.num_threads = 1;
                cfg.model_config.provider = cpu.as_ptr();
                cfg.max_active_paths = 4;
                cfg.keywords_score = 2.0;
                cfg.keywords_threshold = 0.1;
                cfg.num_trailing_blanks = 1;
                cfg.keywords_buf = kw.as_ptr();
                cfg.keywords_buf_size = keywords.len() as i32;
                sys::SherpaOnnxCreateKeywordSpotter(&cfg)
            };
            if spotter.is_null() {
                return Err("no se pudo crear el detector de la frase".into());
            }
            Ok(Spotter { spotter })
        }

        /// The wake phrase heard in 16 kHz mono `samples`, if any.
        pub fn find(&self, samples: &[f32]) -> Option<String> {
            let tail = vec![0.0f32; 8_000];
            let mut found = None;
            unsafe {
                let stream = sys::SherpaOnnxCreateKeywordStream(self.spotter);
                if stream.is_null() {
                    return None;
                }
                for chunk in samples.chunks(1_600).chain(std::iter::once(tail.as_slice())) {
                    sys::SherpaOnnxOnlineStreamAcceptWaveform(stream, 16_000, chunk.as_ptr(), chunk.len() as i32);
                    while sys::SherpaOnnxIsKeywordStreamReady(self.spotter, stream) == 1 {
                        sys::SherpaOnnxDecodeKeywordStream(self.spotter, stream);
                        let r = sys::SherpaOnnxGetKeywordResult(self.spotter, stream);
                        if r.is_null() {
                            continue;
                        }
                        let k = (*r).keyword;
                        if found.is_none() && !k.is_null() {
                            let word = CStr::from_ptr(k).to_string_lossy().into_owned();
                            if !word.is_empty() {
                                found = Some(word);
                            }
                        }
                        sys::SherpaOnnxDestroyKeywordResult(r);
                    }
                    if found.is_some() {
                        break;
                    }
                }
                sys::SherpaOnnxDestroyOnlineStream(stream);
            }
            found
        }
    }

    impl Drop for Spotter {
        fn drop(&mut self) {
            unsafe { sys::SherpaOnnxDestroyKeywordSpotter(self.spotter) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_phrase_is_found_and_the_command_kept() {
        assert_eq!(split("Oye ARIA, abre Spotify."), Split { matched: true, command: "abre Spotify.".into() });
        assert_eq!(split("hey aria"), Split { matched: true, command: String::new() });
        assert_eq!(split("Eh... okay Arya ¿qué hora es?"), Split { matched: true, command: "¿qué hora es?".into() });
        assert_eq!(split("hola aria hola aria pon música").command, "pon música", "the last one counts");
        assert!(!split("ARIA abre Spotify").matched, "the name alone does not wake her");
        assert!(!split("oye María, ven").matched);
        assert!(!split("").matched);
    }

    #[test]
    fn misheard_names_still_wake_her() {
        // What Parakeet and Whisper wrote for a Spanish «Oye/Hey/OK/Hola ARIA».
        assert_eq!(split("Oye el área.").command, "");
        assert!(split("Oye el área.").matched);
        assert!(split("Okay, Garia.").matched);
        assert!(split("Hola Raria.").matched);
        assert_eq!(split("Oye, Arya, ¿qué hora es?").command, "¿qué hora es?");
        assert_eq!(split("Hey Aria, pon música.").command, "pon música.");
        assert!(!split("Mañana voy al área de ventas.").matched, "área without a call");
        assert!(!split("Oye MARIA. Ven acá.").matched);
        assert!(!split("el área de ventas").matched);
    }

    #[test]
    fn backoff_slows_down_then_stops() {
        let mut b = Backoff::default();
        assert_eq!(b.ended(Duration::from_secs(5)), Action::Open);
        assert_eq!(b.ended(Duration::from_millis(100)), Action::Delay(Duration::from_millis(500)));
        assert_eq!(b.ended(Duration::from_millis(100)), Action::Delay(Duration::from_secs(1)));
        assert_eq!(b.ended(Duration::from_millis(100)), Action::Stop);
        b.reset();
        assert_eq!(b.ended(Duration::from_millis(100)), Action::Delay(Duration::from_millis(500)));
        assert_eq!(b.ended(Duration::from_secs(2)), Action::Open, "a normal run resets the streak");
    }

    #[test]
    fn wakes_wait_for_cards_that_need_the_owner() {
        assert_eq!(wake_blocked("overview", "compact", true), Some("approval shown"));
        assert_eq!(wake_blocked("approval", "compact", false), None, "a folded island always accepts");
        assert_eq!(wake_blocked("approval", "expanded", false), Some("approval shown"));
        assert_eq!(wake_blocked("question", "expanded", false), Some("question shown"));
        assert_eq!(wake_blocked("prompt", "expanded", false), None, "the chat takes the next question");
        assert_eq!(wake_blocked("overview", "expanded", false), None);
    }

    /// `cargo test -- --ignored spotter`, with the model and test WAVs in
    /// %TEMP%\aria-voice-dl (not shipped).
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn spotter_hears_the_phrase_and_nothing_else() {
        let dir = std::env::temp_dir().join("aria-voice-dl");
        let spotter = Spotter::new(&dir.join("sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01")).unwrap();
        let wav = |name: &str| -> Vec<f32> {
            let bytes = std::fs::read(dir.join("wavs").join(name)).unwrap();
            let at = bytes.windows(4).position(|w| w == b"data").unwrap() + 8;
            bytes[at..].chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0).collect()
        };
        assert!(spotter.find(&wav("oye_aria.wav")).is_some());
        assert!(spotter.find(&wav("hey_aria_en.wav")).is_some());
        assert!(spotter.find(&wav("maria.wav")).is_none());
        assert!(spotter.find(&wav("nada.wav")).is_none());
    }

    /// `cargo test -- --ignored mic_level --nocapture`: what the microphone hears.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn mic_level() {
        let cap = crate::meeting::Capture::start(true, false).unwrap();
        let mut all = Vec::new();
        for _ in 0..12 {
            std::thread::sleep(Duration::from_millis(500));
            let s = cap.take();
            let peak = s.iter().fold(0f32, |m, x| m.max(x.abs()));
            println!("samples {} rms {:.4} peak {:.3}", s.len(), crate::meeting::rms(&s), peak);
            all.extend(s);
        }
        let dir = std::env::temp_dir().join("aria-voice-dl");
        let spotter = Spotter::new(&dir.join("sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01")).unwrap();
        println!("raw: {:?}", spotter.find(&all));
        println!("louder: {:?}", spotter.find(&crate::voice::listen::louder(all)));
    }

    #[test]
    fn keywords_keep_only_known_tokens() {
        let tokens = "▁HE 1\nY 2\n▁A 3\nRI 4\nA 5\n";
        assert_eq!(checked_keywords(tokens), "▁HE Y ▁A RI A @HEY_ARIA");
        assert_eq!(checked_keywords(""), "");
    }
}
