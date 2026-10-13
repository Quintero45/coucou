// API keys live in the Windows Credential Manager or, on Linux, the Secret
// Service (GNOME Keyring, KWallet) — never on disk and never in the front end — the island can only ask whether a key is present.

use keyring::Entry;

const SERVICE: &str = "dev.miller.aria";
/// Where the keys were kept while ARIA was Coucou. Only read by migrate.rs,
/// and cleared only from Settings, on a click.
const LEGACY_SERVICE: &str = "fr.louisraille.coucou";

/// Every key ARIA may store. Anything outside this list is refused.
pub const KNOWN_KEYS: &[&str] = &[
    "anthropic-api-key",
    "cursor-api-key",
    "xai-api-key",
    "openai-api-key",
    "google-api-key",
    "brave-api-key",
    "n8n-url",
    "n8n-api-key",
    "vercel-token",
    "github-token",
    "stripe-api-key",
    "resend-api-key",
    "notion-api-key",
    "calcom-api-key",
];

/// Secrets of MCP servers (env values, headers): `mcp-secret:<server>:<field>`.
/// mcp.json only ever holds a `${secret:…}` placeholder for them.
pub const MCP_PREFIX: &str = "mcp-secret:";

/// Webhook key of a Grok Bot routine: `grokbot-key:<bot id>`.
pub const GROKBOT_PREFIX: &str = "grokbot-key:";

fn allowed(key: &str) -> bool {
    if KNOWN_KEYS.contains(&key) {
        return true;
    }
    let safe = |rest: &str| {
        !rest.is_empty()
            && rest.len() <= 160
            && rest.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-' | '.'))
    };
    key.strip_prefix(MCP_PREFIX).is_some_and(safe) || key.strip_prefix(GROKBOT_PREFIX).is_some_and(safe)
}

fn entry(key: &str) -> Option<Entry> {
    if !allowed(key) {
        return None;
    }
    Entry::new(SERVICE, key).ok()
}

pub fn get(key: &str) -> Option<String> {
    entry(key)?.get_password().ok().filter(|v| !v.is_empty())
}

pub fn set(key: &str, value: &str) -> Result<(), String> {
    let entry = entry(key).ok_or_else(|| format!("unknown key {key}"))?;
    if value.is_empty() {
        let _ = entry.delete_credential();
        return Ok(());
    }
    entry.set_password(value).map_err(|e| e.to_string())
}

pub fn clear(key: &str) -> Result<(), String> {
    let entry = entry(key).ok_or_else(|| format!("unknown key {key}"))?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

pub fn present(key: &str) -> bool {
    get(key).is_some()
}

fn legacy_entry(key: &str) -> Option<Entry> {
    if !allowed(key) {
        return None;
    }
    Entry::new(LEGACY_SERVICE, key).ok()
}

/// Copies `key` from Coucou's service when ARIA has none of its own yet; the
/// old one stays. True: copied.
pub fn copy_from_legacy(key: &str) -> Result<bool, String> {
    if present(key) {
        return Ok(false);
    }
    let Some(old) = legacy_entry(key).and_then(|e| e.get_password().ok()).filter(|v| !v.is_empty()) else {
        return Ok(false);
    };
    set(key, &old).map(|_| true)
}

/// Whether Coucou's service still holds `key`.
pub fn legacy_present(key: &str) -> bool {
    legacy_entry(key).and_then(|e| e.get_password().ok()).is_some_and(|v| !v.is_empty())
}

/// Deletes `key` from Coucou's service. Never ARIA's own copy.
pub fn clear_legacy(key: &str) -> Result<(), String> {
    let entry = legacy_entry(key).ok_or_else(|| format!("unknown key {key}"))?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

// ── Entries only Rust reads and writes ────────────────────────────────────────
//
// The phone link's certificate, key and pairing token, and the relay's room,
// end-to-end key and access key. No window can reach them: `get`, `set`,
// `clear` and `present` refuse them.

pub const INTERNAL_KEYS: &[&str] = &[
    "phone-link-cert",
    "phone-link-key",
    "phone-link-token",
    "phone-relay-room",
    "phone-relay-key",
    "phone-relay-access",
];

/// Whether the keystore would accept this key through `get_internal` / `set_internal`.
pub fn is_internal(key: &str) -> bool {
    INTERNAL_KEYS.contains(&key)
}

fn internal_entry(key: &str) -> Result<Entry, String> {
    if !is_internal(key) {
        return Err(format!("unknown key {key}"));
    }
    Entry::new(SERVICE, key).map_err(|e| e.to_string())
}

pub fn get_internal(key: &str) -> Option<String> {
    internal_entry(key).ok()?.get_password().ok().filter(|v| !v.is_empty())
}

pub fn set_internal(key: &str, value: &str) -> Result<(), String> {
    let entry = internal_entry(key)?;
    if value.is_empty() {
        // Empty means "forget it"; some keystores refuse an empty password.
        return match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        };
    }
    entry.set_password(value).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_keys_never_reach_a_window() {
        for key in INTERNAL_KEYS {
            assert!(!KNOWN_KEYS.contains(key), "{key}");
            assert!(entry(key).is_none(), "{key}");
            assert!(set(key, "x").is_err(), "{key}");
            assert!(clear(key).is_err(), "{key}");
            assert!(!present(key), "{key}");
        }
        // And the other way round: a key of the island is not an internal one.
        assert!(set_internal("github-token", "x").is_err());
    }
}
