// When a spoken turn is over, and the words that end or pause a voice
// conversation — port of TurnEndPolicy.swift, in Spanish first.

use std::time::Duration;

pub const BASE_SILENCE: Duration = Duration::from_millis(1600);
/// The last word says more is coming ("y", "para", "abre"…).
pub const EXTENDED_SILENCE: Duration = Duration::from_millis(2600);
/// Free-text answers (who, the subject, what to look up): room to think.
pub const ANSWER_SILENCE: Duration = Duration::from_millis(3200);
pub const MAX_TURN: Duration = Duration::from_secs(15);

/// Lowercase, accents and punctuation out: what the phrase lists compare against.
pub fn normalise(s: &str) -> String {
    let folded: String = s
        .chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'á' | 'à' | 'ä' | 'â' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'ñ' => 'n',
            c if c.is_alphanumeric() => c,
            _ => ' ',
        })
        .collect();
    folded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// How long a silence ends the turn, given what was heard so far.
pub fn silence_delay(transcript: &str, long_answer: bool) -> Duration {
    if long_answer {
        return ANSWER_SILENCE;
    }
    let norm = normalise(transcript);
    match norm.split(' ').next_back() {
        Some(last) if !last.is_empty() && EXTENDING_WORDS.contains(&last) => EXTENDED_SILENCE,
        _ => BASE_SILENCE,
    }
}

/// Words after which the speaker is mid-thought.
const EXTENDING_WORDS: &[&str] = &[
    // ES
    "y", "e", "o", "u", "pero", "luego", "despues", "entonces", "que", "porque",
    "eh", "em", "mmm", "este", "pues", "bueno", "osea", "espera",
    "de", "del", "el", "la", "los", "las", "un", "una", "unos", "unas", "a", "al",
    "en", "con", "para", "por", "sobre", "sin", "mi", "tu", "su",
    "abre", "pon", "busca", "manda", "envia", "escribe", "dile", "llama", "cierra",
    "sube", "baja", "quita", "agrega", "anade", "activa", "desactiva", "reproduce",
    // EN
    "and", "then", "but", "or", "um", "uh", "like", "well", "wait",
    "the", "to", "on", "of", "in", "at", "with", "for",
    "open", "play", "send", "add", "remove", "set", "start",
];

/// Phrases that end the conversation window (no wake word needed until then).
const END_PHRASES: &[&str] = &[
    "gracias", "muchas gracias", "eso es todo", "es todo", "nada mas", "ya esta", "listo",
    "cancela", "cancelar", "olvidalo", "dejalo", "adios", "hasta luego",
    "thanks", "thank you", "that s all", "never mind", "cancel", "stop", "bye",
];

/// Phrases that pause listening until the owner turns it back on.
const PAUSE_PHRASES: &[&str] = &["pausa", "pausate", "deja de escuchar", "no escuches", "pause", "stop listening"];

pub fn ends_conversation(text: &str) -> bool {
    let n = normalise(text);
    END_PHRASES.contains(&n.as_str())
}

pub fn pauses_listening(text: &str) -> bool {
    let n = normalise(text);
    PAUSE_PHRASES.contains(&n.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_depends_on_the_last_word() {
        assert_eq!(silence_delay("", false), BASE_SILENCE);
        assert_eq!(silence_delay("abre Spotify", false), BASE_SILENCE);
        assert_eq!(silence_delay("manda un correo a", false), EXTENDED_SILENCE);
        assert_eq!(silence_delay("busca vuelos y", false), EXTENDED_SILENCE);
        assert_eq!(silence_delay("Abre…", false), EXTENDED_SILENCE, "punctuation does not hide the word");
        assert_eq!(silence_delay("abre Spotify", true), ANSWER_SILENCE);
    }

    #[test]
    fn phrases_are_matched_whole_and_without_accents() {
        assert!(ends_conversation("¡Gracias!"));
        assert!(ends_conversation("Eso es todo."));
        assert!(ends_conversation("Olvídalo"));
        assert!(!ends_conversation("gracias, ahora abre Spotify"));
        assert!(pauses_listening("Pausa."));
        assert!(pauses_listening("deja de escuchar"));
        assert!(!pauses_listening("pausa la música"));
        assert_eq!(normalise("  ¿Qué  HORA es? "), "que hora es");
    }
}
