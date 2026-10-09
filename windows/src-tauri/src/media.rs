// Now playing: whatever Windows shows in its media flyout (Spotify, a browser
// tab, a video player…), for the island's music pill. Event driven: the
// session's own change events push a snapshot to the island, nothing polls.
// Every control is a click on the card; ARIA's tools don't go through here.

use serde::Serialize;
use tauri::AppHandle;

#[derive(Serialize, Clone, Default, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub active: bool,
    /// The player's name ("Spotify", "Chrome"…), empty when Windows only gives a hash.
    pub app: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub playing: bool,
    pub position_ms: u64,
    pub duration_ms: u64,
    /// Unix time (ms) at which `position_ms` was true; the island moves the bar on from there.
    pub at_ms: u64,
    pub shuffle: Option<bool>,
    /// "none", "track" or "list".
    pub repeat: Option<String>,
    pub can_prev: bool,
    pub can_next: bool,
    pub can_seek: bool,
    pub can_shuffle: bool,
    pub can_repeat: bool,
    /// The cover as a data: URL, when the player gives one.
    pub cover: Option<String>,
    /// System volume, 0–1.
    pub volume: Option<f32>,
}

/// A player's name from its AppUserModelID ("Spotify.exe", "MSEdge",
/// "SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify", "308046B0AF4A39CB"…).
pub fn app_name(aumid: &str) -> String {
    let lower = aumid.to_lowercase();
    const KNOWN: &[(&str, &str)] = &[
        ("spotify", "Spotify"),
        ("msedge", "Edge"),
        ("chrome", "Chrome"),
        ("firefox", "Firefox"),
        ("zen", "Zen"),
        ("brave", "Brave"),
        ("opera", "Opera"),
        ("vivaldi", "Vivaldi"),
        ("vlc", "VLC"),
        ("applemusic", "Apple Music"),
        ("itunes", "iTunes"),
        ("zunemusic", "Media Player"),
        ("zunevideo", "Media Player"),
        ("tidal", "TIDAL"),
        ("deezer", "Deezer"),
    ];
    if let Some((_, name)) = KNOWN.iter().find(|(key, _)| lower.contains(key)) {
        return (*name).to_string();
    }
    let tail = aumid.rsplit(['!', '\\', '/']).next().unwrap_or_default();
    let tail = tail.strip_suffix(".exe").or_else(|| tail.strip_suffix(".EXE")).unwrap_or(tail);
    // Firefox-family browsers register under a bare hash.
    if tail.len() >= 12 && tail.chars().all(|c| c.is_ascii_hexdigit()) {
        return String::new();
    }
    tail.to_string()
}

/// Whether a new snapshot says something the island doesn't know yet. A bar
/// that only moved on as expected isn't worth a message.
pub fn worth_sending(last: &Snapshot, next: &Snapshot) -> bool {
    let same_but_time = Snapshot { position_ms: 0, at_ms: 0, ..last.clone() } == Snapshot { position_ms: 0, at_ms: 0, ..next.clone() };
    if !same_but_time {
        return true;
    }
    let expected = if last.playing {
        last.position_ms as i64 + next.at_ms as i64 - last.at_ms as i64
    } else {
        last.position_ms as i64
    };
    (next.position_ms as i64 - expected).abs() > 1500
}

/// Windows' DateTime (100 ns since 1601) as Unix ms; 0 when unknown.
pub fn unix_ms(universal_time: i64) -> u64 {
    const EPOCH_DIFF_MS: i64 = 11_644_473_600_000;
    (universal_time / 10_000 - EPOCH_DIFF_MS).max(0) as u64
}

pub fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub use imp::{control, snapshot, start};

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::log;
    use std::sync::{LazyLock, Mutex, MutexGuard, OnceLock};
    use tauri::Emitter;
    use windows::Foundation::{TimeSpan, TypedEventHandler};
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSession as Session,
        GlobalSystemMediaTransportControlsSessionManager as Manager,
        GlobalSystemMediaTransportControlsSessionMediaProperties as Properties,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
    };
    use windows::Media::MediaPlaybackAutoRepeatMode as Repeat;
    use windows::Storage::Streams::DataReader;
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::{eMultimedia, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};

    /// Covers bigger than this are skipped (players send a few hundred KB at most).
    const MAX_COVER: u32 = 3 * 1024 * 1024;

    struct Bound {
        session: Session,
        tokens: [i64; 3],
    }

    #[derive(Default)]
    struct Inner {
        manager: Option<Manager>,
        bound: Option<Bound>,
        cover_for: String,
        cover: Option<String>,
        last: Snapshot,
    }

    static INNER: LazyLock<Mutex<Inner>> = LazyLock::new(Mutex::default);
    static APP: OnceLock<AppHandle> = OnceLock::new();

    fn inner() -> MutexGuard<'static, Inner> {
        INNER.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn start(app: AppHandle) {
        let _ = APP.set(app);
        std::thread::spawn(|| match connect() {
            Ok(()) => log::line("media: listening to the system media controls"),
            Err(e) => log::line(format!("media: unavailable: {e}")),
        });
    }

    fn connect() -> windows::core::Result<()> {
        let manager = Manager::RequestAsync()?.get()?;
        manager.CurrentSessionChanged(&TypedEventHandler::new(|_, _| {
            rebind();
            Ok(())
        }))?;
        inner().manager = Some(manager);
        rebind();
        Ok(())
    }

    /// Follows the session Windows shows now (the last player that took the keys).
    fn rebind() {
        let (manager, old) = {
            let mut g = inner();
            (g.manager.clone(), g.bound.take())
        };
        if let Some(b) = old {
            let _ = b.session.RemoveMediaPropertiesChanged(b.tokens[0]);
            let _ = b.session.RemovePlaybackInfoChanged(b.tokens[1]);
            let _ = b.session.RemoveTimelinePropertiesChanged(b.tokens[2]);
        }
        if let Some(session) = manager.and_then(|m| m.GetCurrentSession().ok()) {
            let tokens = [
                session.MediaPropertiesChanged(&TypedEventHandler::new(|_, _| {
                    push();
                    Ok(())
                })),
                session.PlaybackInfoChanged(&TypedEventHandler::new(|_, _| {
                    push();
                    Ok(())
                })),
                session.TimelinePropertiesChanged(&TypedEventHandler::new(|_, _| {
                    push();
                    Ok(())
                })),
            ]
            .map(|t| t.unwrap_or_default());
            inner().bound = Some(Bound { session, tokens });
        }
        push();
    }

    fn push() {
        let snap = snapshot();
        {
            let mut g = inner();
            if !worth_sending(&g.last, &snap) {
                return;
            }
            g.last = snap.clone();
        }
        if let Some(app) = APP.get() {
            let _ = app.emit_to(crate::island::WINDOW_LABEL, "media", &snap);
        }
    }

    fn session() -> Option<Session> {
        inner().bound.as_ref().map(|b| b.session.clone())
    }

    pub fn snapshot() -> Snapshot {
        let Some(session) = session() else {
            return Snapshot { volume: volume(), ..Snapshot::default() };
        };
        read(&session).unwrap_or_else(|_| Snapshot { volume: volume(), ..Snapshot::default() })
    }

    fn ms(t: TimeSpan) -> u64 {
        (t.Duration.max(0) / 10_000) as u64
    }

    fn read(s: &Session) -> windows::core::Result<Snapshot> {
        let props = s.TryGetMediaPropertiesAsync()?.get()?;
        let info = s.GetPlaybackInfo()?;
        let controls = info.Controls()?;
        let timeline = s.GetTimelineProperties()?;
        let status = info.PlaybackStatus()?;
        let title = props.Title()?.to_string();
        let artist = props.Artist().map(|a| a.to_string()).unwrap_or_default();
        let album = props.AlbumTitle().map(|a| a.to_string()).unwrap_or_default();
        let start = ms(timeline.StartTime()?);
        let end = ms(timeline.EndTime()?);
        let position = ms(timeline.Position()?);
        let updated = timeline.LastUpdatedTime().map(|d| unix_ms(d.UniversalTime)).unwrap_or(0);
        let now = now_ms();
        let at_ms = if updated == 0 || updated > now { now } else { updated };
        let shuffle = info.IsShuffleActive().ok().and_then(|r| r.Value().ok());
        let repeat = info.AutoRepeatMode().ok().and_then(|r| r.Value().ok()).map(|m| {
            match m {
                Repeat::Track => "track",
                Repeat::List => "list",
                _ => "none",
            }
            .to_string()
        });
        let playing = status == Status::Playing;
        Ok(Snapshot {
            active: !title.is_empty() || playing,
            app: app_name(&s.SourceAppUserModelId().map(|a| a.to_string()).unwrap_or_default()),
            cover: cover(&format!("{title}\u{1}{artist}\u{1}{album}"), &props),
            title,
            artist,
            album,
            playing,
            position_ms: position.saturating_sub(start),
            duration_ms: end.saturating_sub(start),
            at_ms,
            shuffle,
            repeat,
            can_prev: controls.IsPreviousEnabled().unwrap_or(false),
            can_next: controls.IsNextEnabled().unwrap_or(false),
            can_seek: controls.IsPlaybackPositionEnabled().unwrap_or(false) && end > start,
            can_shuffle: controls.IsShuffleEnabled().unwrap_or(false),
            can_repeat: controls.IsRepeatEnabled().unwrap_or(false),
            volume: volume(),
        })
    }

    /// The cover for this song, read once and kept until the song changes.
    fn cover(key: &str, props: &Properties) -> Option<String> {
        {
            let g = inner();
            if g.cover_for == key {
                return g.cover.clone();
            }
        }
        let cover = read_cover(props);
        let mut g = inner();
        g.cover_for = key.to_string();
        g.cover = cover.clone();
        cover
    }

    fn read_cover(props: &Properties) -> Option<String> {
        let stream = props.Thumbnail().ok()?.OpenReadAsync().ok()?.get().ok()?;
        let size = u32::try_from(stream.Size().ok()?).ok()?;
        if size == 0 || size > MAX_COVER {
            return None;
        }
        let mime = stream.ContentType().map(|c| c.to_string()).unwrap_or_default();
        let mime = if mime.starts_with("image/") { mime } else { "image/png".into() };
        let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0).ok()?).ok()?;
        reader.LoadAsync(size).ok()?.get().ok()?;
        let mut bytes = vec![0u8; size as usize];
        reader.ReadBytes(&mut bytes).ok()?;
        Some(format!("data:{mime};base64,{}", base64(&bytes)))
    }

    fn endpoint() -> Option<IAudioEndpointVolume> {
        // Already initialised (or in an STA) is fine: the endpoint is free-threaded.
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        unsafe {
            let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
            let device = devices.GetDefaultAudioEndpoint(eRender, eMultimedia).ok()?;
            device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None).ok()
        }
    }

    fn volume() -> Option<f32> {
        let ep = endpoint()?;
        unsafe { ep.GetMasterVolumeLevelScalar() }.ok()
    }

    /// One click on the card: play/pause, skip, seek, shuffle, repeat or volume.
    pub fn control(action: &str, value: Option<f64>) -> Result<(), String> {
        let fail = |e: windows::core::Error| format!("El reproductor no respondió: {e}");
        if action == "volume" {
            let level = value.ok_or("Falta el volumen.")?.clamp(0.0, 1.0) as f32;
            let ep = endpoint().ok_or("No se encontró la salida de audio.")?;
            unsafe { ep.SetMasterVolumeLevelScalar(level, std::ptr::null()) }.map_err(fail)?;
            push();
            return Ok(());
        }
        let s = session().ok_or("Nada está sonando ahora.")?;
        let done = match action {
            "toggle" => s.TryTogglePlayPauseAsync().and_then(|op| op.get()),
            "next" => s.TrySkipNextAsync().and_then(|op| op.get()),
            "prev" => s.TrySkipPreviousAsync().and_then(|op| op.get()),
            "seek" => {
                let start = s.GetTimelineProperties().and_then(|t| t.StartTime()).map(ms).unwrap_or(0);
                let target = (value.ok_or("Falta la posición.")?.max(0.0) as u64 + start) as i64 * 10_000;
                s.TryChangePlaybackPositionAsync(target).and_then(|op| op.get())
            }
            "shuffle" => {
                let on = s.GetPlaybackInfo().and_then(|i| i.IsShuffleActive()).and_then(|r| r.Value()).unwrap_or(false);
                s.TryChangeShuffleActiveAsync(!on).and_then(|op| op.get())
            }
            "repeat" => {
                let mode = s.GetPlaybackInfo().and_then(|i| i.AutoRepeatMode()).and_then(|r| r.Value()).unwrap_or(Repeat::None);
                let next = match mode {
                    Repeat::None => Repeat::List,
                    Repeat::List => Repeat::Track,
                    _ => Repeat::None,
                };
                s.TryChangeAutoRepeatModeAsync(next).and_then(|op| op.get())
            }
            other => return Err(format!("Acción desconocida: {other}")),
        }
        .map_err(fail)?;
        if !done {
            return Err("El reproductor no permite eso ahora.".into());
        }
        push();
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn start(_app: AppHandle) {}

    pub fn snapshot() -> Snapshot {
        Snapshot::default()
    }

    pub fn control(_action: &str, _value: Option<f64>) -> Result<(), String> {
        Err("Los controles de música solo existen en Windows por ahora.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn players_get_their_names() {
        assert_eq!(app_name("Spotify.exe"), "Spotify");
        assert_eq!(app_name("SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify"), "Spotify");
        assert_eq!(app_name("MSEdge"), "Edge");
        assert_eq!(app_name("Chrome"), "Chrome");
        assert_eq!(app_name("308046B0AF4A39CB"), "");
        assert_eq!(app_name(r"C:\Apps\foobar2000.exe"), "foobar2000");
    }

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn windows_time_becomes_unix_ms() {
        // 2024-01-01T00:00:00Z
        assert_eq!(unix_ms(133_485_408_000_000_000), 1_704_067_200_000);
        assert_eq!(unix_ms(0), 0);
    }

    #[test]
    fn a_bar_moving_on_as_expected_is_not_sent() {
        let last = Snapshot { active: true, title: "A".into(), playing: true, position_ms: 10_000, at_ms: 1_000_000, ..Default::default() };
        let on_time = Snapshot { position_ms: 15_000, at_ms: 1_005_000, ..last.clone() };
        assert!(!worth_sending(&last, &on_time));
        let seeked = Snapshot { position_ms: 60_000, at_ms: 1_005_000, ..last.clone() };
        assert!(worth_sending(&last, &seeked));
        let paused = Snapshot { playing: false, ..on_time.clone() };
        assert!(worth_sending(&last, &paused));
        let next_song = Snapshot { title: "B".into(), ..on_time };
        assert!(worth_sending(&last, &next_song));
    }
}
