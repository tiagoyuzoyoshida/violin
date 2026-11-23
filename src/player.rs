use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime};

use flate2::read::GzDecoder;
use futures_util::{SinkExt, StreamExt};
use tar::Archive;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, Command as ProcessCommand};
use tokio::sync::mpsc;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

use crate::app::{AppEvent, Command};
use crate::domain::{PlaybackStatus, Track, TrackId};

pub const SOLOIST_DEVICE_NAME: &str = "violin";
const SOLOIST_URL_X86_64: &str =
    "https://soloist-builds.spotifycdn.com/soloist_release_x86_64.tar.gz";
const SOLOIST_URL_AARCH64: &str =
    "https://soloist-builds.spotifycdn.com/soloist_release_arm64.tar.gz";
const SOLOIST_API_KEY_URL: &str = "https://developer.spotify.com/documentation/soloist";
const SOLOIST_MAX_AGE_SECS: u64 = 80 * 24 * 60 * 60;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);
const WS_PORT_POLL_TIMEOUT: Duration = Duration::from_secs(15);
const WS_PORT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const AUTH_READY_TIMEOUT: Duration = Duration::from_secs(15);
const PENDING_PLAY_SYNC_GRACE: Duration = Duration::from_secs(5);

const SOLOIST_BIN_NAME: &str = "soloist";
const SOLOIST_DATA_DIR_NAME: &str = "data";
const INSTALLED_AT_MARKER_NAME: &str = ".installed_at";
const API_KEY_MARKER_NAME: &str = ".api_key_configured";
const PAIRED_MARKER_NAME: &str = ".paired";
const WS_PORT_FILE_NAME: &str = "ws.port";

/// Errors returned while downloading, configuring, or running Spotify Soloist.
#[derive(Debug, Error)]
pub enum PlayerError {
    #[error("unsupported Linux CPU architecture for Spotify Soloist: {0}")]
    UnsupportedArch(String),
    #[error("failed to download Spotify Soloist: {0}")]
    Download(String),
    #[error("Soloist I/O error at {}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("Spotify Soloist setup failed: {0}")]
    Setup(String),
    #[error("Spotify Soloist process failed: {0}")]
    Process(String),
    #[error("Spotify Soloist WebSocket error: {0}")]
    WebSocket(String),
}

struct ActiveSoloist {
    _child: Child,
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

/// Parsed readiness state from a Soloist `auth_state` WebSocket message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SoloistAuthState {
    pub logged_in: bool,
    pub is_active: bool,
}

/// Parses a Soloist `auth_state` WebSocket JSON message, if applicable.
pub fn parse_auth_state(raw: &str) -> Option<SoloistAuthState> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    if value.get("type")?.as_str()? != "auth_state" {
        return None;
    }
    let logged_in = value
        .get("logged_in")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let is_active = value
        .get("is_active")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    Some(SoloistAuthState {
        logged_in,
        is_active,
    })
}

/// Maps a target CPU architecture (`std::env::consts::ARCH`) to its Soloist release tarball URL.
pub fn soloist_download_url(arch: &str) -> Result<&'static str, PlayerError> {
    match arch {
        "x86_64" => Ok(SOLOIST_URL_X86_64),
        "aarch64" => Ok(SOLOIST_URL_AARCH64),
        other => Err(PlayerError::UnsupportedArch(other.to_string())),
    }
}

/// Serializes a playback `Command` into a Soloist WebSocket JSON command, if applicable.
pub fn build_soloist_command_json(cmd: &Command) -> Option<String> {
    let payload = match cmd {
        Command::PlayTrack(track) => serde_json::json!({
            "type": "command",
            "command": "play",
            "uri": format!("spotify:track:{}", track.id.as_str()),
        }),
        Command::PausePlayback | Command::StopPlayback => serde_json::json!({
            "type": "command",
            "command": "pause",
        }),
        Command::ResumePlayback => serde_json::json!({
            "type": "command",
            "command": "play",
        }),
        Command::SkipNext => serde_json::json!({
            "type": "command",
            "command": "skip_next",
        }),
        Command::SkipPrevious => serde_json::json!({
            "type": "command",
            "command": "skip_prev",
        }),
        Command::Seek(position) => serde_json::json!({
            "type": "command",
            "command": "seek",
            "position_ms": position.as_millis() as u64,
        }),
        Command::SetVolume(volume) => serde_json::json!({
            "type": "command",
            "command": "set_volume",
            "volume": volume,
        }),
        Command::Search(_)
        | Command::FetchSavedAlbums
        | Command::FetchAlbumTracks { .. }
        | Command::FetchTrackRadio(_)
        | Command::SetTrackLiked { .. }
        | Command::CreatePlaylist(_)
        | Command::AddTrackToPlaylist { .. }
        | Command::RemovePlaylist { .. }
        | Command::RemoveTrackFromPlaylist { .. }
        | Command::FetchJamSession
        | Command::CreateJamSession
        | Command::JoinJamSession(_)
        | Command::LeaveJamSession { .. } => {
            return None;
        }
    };
    Some(payload.to_string())
}

/// Translates an incoming Soloist WebSocket JSON message into an `AppEvent`, if relevant.
pub fn parse_soloist_event(raw: &str, active_track_uri: Option<&str>) -> Option<AppEvent> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let event_type = value.get("type")?.as_str()?;

    match event_type {
        "position_sync" => {
            let ms = value.get("position")?.get("position_ms")?.as_u64()?;
            Some(AppEvent::PlaybackPositionUpdated(Duration::from_millis(ms)))
        }
        "playback_state" => {
            let status_str = value.get("status").and_then(serde_json::Value::as_str);
            let item = value.get("item").or_else(|| value.get("track"));
            let item_is_none = item.is_none_or(serde_json::Value::is_null);
            if status_str == Some("stopped")
                || (status_str == Some("idle") && active_track_uri.is_some() && item_is_none)
            {
                return Some(AppEvent::TrackEnded);
            }

            let position_ms = value
                .get("position")
                .and_then(|p| p.get("position_ms"))
                .and_then(serde_json::Value::as_u64);

            if let Some(item_obj) = item.filter(|v| v.is_object())
                && let Some(item_uri) = item_obj.get("uri").and_then(serde_json::Value::as_str)
                && active_track_uri != Some(item_uri)
                && let Some(track) = parse_soloist_item_track(item_obj)
            {
                let position = Duration::from_millis(position_ms.unwrap_or(0));
                let status = match status_str {
                    Some("paused") => PlaybackStatus::Paused,
                    Some("stopped") => PlaybackStatus::Stopped,
                    _ => PlaybackStatus::Playing,
                };
                return Some(AppEvent::TrackSynced {
                    track,
                    position,
                    status,
                });
            }

            let ms = position_ms?;
            Some(AppEvent::PlaybackPositionUpdated(Duration::from_millis(ms)))
        }
        "command_result" => {
            if value.get("success").and_then(serde_json::Value::as_bool) == Some(false) {
                let message = value
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("Spotify Soloist playback command failed")
                    .to_string();
                return Some(AppEvent::PlaybackFailed(message));
            }
            None
        }
        "queue_changed" | "queue" => {
            let entries = value
                .get("upcoming")
                .or_else(|| value.get("tracks"))
                .and_then(serde_json::Value::as_array)?;
            let tracks = entries
                .iter()
                .filter_map(|entry| {
                    let item = entry.get("item").unwrap_or(entry);
                    parse_soloist_item_track(item)
                })
                .collect();
            Some(AppEvent::QueueUpdated(tracks))
        }
        "volume_changed" => {
            let vol = value.get("volume").and_then(serde_json::Value::as_u64)?;
            Some(AppEvent::VolumeChanged(vol.min(100) as u8))
        }
        _ => None,
    }
}

fn parse_soloist_item_track(item: &serde_json::Value) -> Option<Track> {
    let uri = item.get("uri")?.as_str()?;
    let raw_id = uri.strip_prefix("spotify:track:")?;
    let id = TrackId::new(raw_id).ok()?;

    let decorations = item.get("decorations");
    let title = decorations
        .and_then(|d| d.get("identity"))
        .and_then(|i| i.get("name"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| item.get("name").and_then(serde_json::Value::as_str))?
        .trim();
    if title.is_empty() {
        return None;
    }

    let artists: Vec<String> = decorations
        .and_then(|d| d.get("creators"))
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    c.get("entity")
                        .and_then(|e| e.get("decorations"))
                        .and_then(|d| d.get("identity"))
                        .and_then(|i| i.get("name"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(ToOwned::to_owned)
                })
                .collect::<Vec<_>>()
        })
        .filter(|v| !v.is_empty())
        .or_else(|| {
            item.get("artists")
                .and_then(serde_json::Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|a| {
                            a.get("name")
                                .and_then(serde_json::Value::as_str)
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                                .map(ToOwned::to_owned)
                        })
                        .collect::<Vec<_>>()
                })
        })
        .unwrap_or_default();

    let album_name = decorations
        .and_then(|d| d.get("parent"))
        .and_then(|p| p.get("entity"))
        .and_then(|e| e.get("decorations"))
        .and_then(|d| d.get("identity"))
        .and_then(|i| i.get("name"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            item.get("album")
                .and_then(|a| a.get("name"))
                .and_then(serde_json::Value::as_str)
        })
        .unwrap_or("")
        .to_string();

    let duration_ms = decorations
        .and_then(|d| d.get("playback"))
        .and_then(|p| p.get("duration_ms"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);

    Some(Track {
        id,
        title: title.to_string(),
        artists,
        album_name,
        duration: Duration::from_millis(duration_ms),
    })
}

/// Ensures the Soloist binary is downloaded/fresh, API key is configured, and device is paired.
pub async fn ensure_soloist_ready(soloist_dir: &Path) -> Result<PathBuf, PlayerError> {
    let data_dir = soloist_dir.join(SOLOIST_DATA_DIR_NAME);
    create_dir_all(soloist_dir)?;
    create_dir_all(&data_dir)?;

    let binary_path = soloist_dir.join(SOLOIST_BIN_NAME);
    if needs_binary_refresh(soloist_dir, &binary_path)? {
        download_and_unpack_soloist(soloist_dir, &binary_path).await?;
    }

    let api_key_marker = soloist_dir.join(API_KEY_MARKER_NAME);
    if !api_key_marker.exists() {
        setup_api_key(soloist_dir, &binary_path, &data_dir, &api_key_marker).await?;
    }

    let paired_marker = soloist_dir.join(PAIRED_MARKER_NAME);
    if !paired_marker.exists() {
        pair_device(soloist_dir, &binary_path, &data_dir, &paired_marker).await?;
    }

    Ok(binary_path)
}

/// Removes the Soloist pairing marker so the next launch prompts for Connect pairing again.
pub fn clear_soloist_pairing(soloist_dir: &Path) -> Result<(), PlayerError> {
    let paired_marker = soloist_dir.join(PAIRED_MARKER_NAME);
    match fs::remove_file(&paired_marker) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(source) => Err(PlayerError::Io {
            path: paired_marker,
            source,
        }),
    }
}

fn bundled_loader_name(arch: &str) -> Option<(&'static str, &'static str)> {
    match arch {
        "x86_64" => Some((
            "ld-linux-x86-64.so.2",
            "/usr/lib/x86_64-linux-gnu:/lib/x86_64-linux-gnu:/usr/lib64:/usr/lib",
        )),
        "aarch64" => Some((
            "ld-linux-aarch64.so.1",
            "/usr/lib/aarch64-linux-gnu:/lib/aarch64-linux-gnu:/usr/lib64:/usr/lib",
        )),
        _ => None,
    }
}

/// Returns the bundled `glibc` dynamic linker path and prefix arguments when `<soloist_dir>/lib/<loader>` exists.
pub fn bundled_loader_invocation(
    soloist_dir: &Path,
    binary_path: &Path,
    arch: &str,
) -> Option<(PathBuf, Vec<OsString>)> {
    let (loader_name, sys_dirs) = bundled_loader_name(arch)?;
    let lib_dir = soloist_dir.join("lib");
    let loader_path = lib_dir.join(loader_name);
    if !loader_path.exists() {
        return None;
    }

    let mut library_path = OsString::from(lib_dir.as_os_str());
    library_path.push(":");
    library_path.push(sys_dirs);

    Some((
        loader_path,
        vec![
            OsString::from("--library-path"),
            library_path,
            binary_path.as_os_str().to_os_string(),
        ],
    ))
}

fn build_soloist_process(soloist_dir: &Path, binary_path: &Path) -> ProcessCommand {
    let native_ok = std::process::Command::new(binary_path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if native_ok {
        return ProcessCommand::new(binary_path);
    }

    if let Some((loader, prefix_args)) =
        bundled_loader_invocation(soloist_dir, binary_path, std::env::consts::ARCH)
    {
        let mut cmd = ProcessCommand::new(loader);
        cmd.args(prefix_args);
        return cmd;
    }

    ProcessCommand::new(binary_path)
}

/// Returns `true` when a `TrackSynced` URI is a stale pre-play state within the 5s grace window after `PlayTrack`.
pub fn should_suppress_stale_sync(
    pending: &Option<(String, Instant)>,
    now: Instant,
    synced_track_uri: &str,
) -> bool {
    let Some((expected, since)) = pending else {
        return false;
    };
    now.saturating_duration_since(*since) < PENDING_PLAY_SYNC_GRACE && synced_track_uri != expected
}

fn playback_state_item_uri(raw: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    if value.get("type")?.as_str()? != "playback_state" {
        return None;
    }
    value
        .get("item")
        .or_else(|| value.get("track"))?
        .get("uri")?
        .as_str()
        .map(str::to_owned)
}

/// Runs the background Soloist playback worker until `cmd_rx` closes.
pub async fn run_player_worker(
    soloist_dir: PathBuf,
    mut cmd_rx: mpsc::Receiver<Command>,
    event_tx: mpsc::Sender<AppEvent>,
) {
    let mut active: Option<ActiveSoloist> = None;
    let mut active_track_uri: Option<String> = None;
    let mut pending_play_uri: Option<(String, Instant)> = None;

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                match cmd {
                    None => {
                        drop(active);
                        break;
                    }
                    Some(cmd) => {
                        let Some(json) = build_soloist_command_json(&cmd) else {
                            continue;
                        };

                        if let Command::PlayTrack(ref track) = cmd {
                            if active.is_none() {
                                match start_soloist(&soloist_dir).await {
                                    Ok(ap) => active = Some(ap),
                                    Err(err) => {
                                        if event_tx
                                            .send(AppEvent::PlaybackFailed(err.to_string()))
                                            .await
                                            .is_err()
                                        {
                                            break;
                                        }
                                        continue;
                                    }
                                }
                            }
                            let track_uri = format!("spotify:track:{}", track.id.as_str());
                            active_track_uri = Some(track_uri.clone());
                            pending_play_uri = Some((track_uri, Instant::now()));
                            if let Some(ap) = active.as_mut()
                                && let Err(err) = ap.ws.send(Message::Text(json.into())).await
                            {
                                active = None;
                                active_track_uri = None;
                                pending_play_uri = None;
                                if event_tx
                                    .send(AppEvent::PlaybackFailed(format!(
                                        "Spotify Soloist WebSocket send failed: {err}"
                                    )))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                        } else {
                            if let Some(ap) = active.as_mut()
                                && let Err(err) = ap.ws.send(Message::Text(json.into())).await
                            {
                                active = None;
                                active_track_uri = None;
                                pending_play_uri = None;
                                if event_tx
                                    .send(AppEvent::PlaybackFailed(format!(
                                        "Spotify Soloist WebSocket send failed: {err}"
                                    )))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            if matches!(cmd, Command::StopPlayback) {
                                active_track_uri = None;
                                pending_play_uri = None;
                            }
                        }
                    }
                }
            }
            msg = next_ws_message(&mut active), if active.is_some() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Some((ref expected, _)) = pending_play_uri
                            && playback_state_item_uri(text.as_str()).as_deref()
                                == Some(expected.as_str())
                        {
                            pending_play_uri = None;
                        }
                        if let Some(ev) = parse_soloist_event(text.as_str(), active_track_uri.as_deref()) {
                            match &ev {
                                AppEvent::TrackEnded => {
                                    active_track_uri = None;
                                }
                                AppEvent::TrackSynced { track, .. } => {
                                    let synced_uri =
                                        format!("spotify:track:{}", track.id.as_str());
                                    if should_suppress_stale_sync(
                                        &pending_play_uri,
                                        Instant::now(),
                                        &synced_uri,
                                    ) {
                                        continue;
                                    }
                                    pending_play_uri = None;
                                    active_track_uri = Some(synced_uri);
                                }
                                _ => {}
                            }
                            if event_tx.send(ev).await.is_err() {
                                break;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => {
                        active = None;
                    }
                    Some(Ok(_)) => {}
                }
            }
        }
    }
}

async fn next_ws_message(
    active: &mut Option<ActiveSoloist>,
) -> Option<Result<Message, tokio_tungstenite::tungstenite::Error>> {
    match active {
        Some(ap) => ap.ws.next().await,
        None => std::future::pending().await,
    }
}

async fn start_soloist(soloist_dir: &Path) -> Result<ActiveSoloist, PlayerError> {
    let binary_path = soloist_dir.join(SOLOIST_BIN_NAME);
    if !binary_path.exists() {
        return Err(PlayerError::Process(
            "Spotify Soloist binary is missing; run violin login".to_string(),
        ));
    }

    let data_dir = soloist_dir.join(SOLOIST_DATA_DIR_NAME);
    let ws_port_path = data_dir.join(WS_PORT_FILE_NAME);
    match fs::remove_file(&ws_port_path) {
        Ok(()) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(source) => {
            return Err(PlayerError::Io {
                path: ws_port_path,
                source,
            });
        }
    }

    let mut child = build_soloist_process(soloist_dir, &binary_path)
        .args(["-n", SOLOIST_DEVICE_NAME, "-D"])
        .arg(&data_dir)
        .args(["-w", "127.0.0.1:0", "-i", "80"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|source| PlayerError::Io {
            path: binary_path,
            source,
        })?;

    let deadline = Instant::now() + WS_PORT_POLL_TIMEOUT;
    let port = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|err| PlayerError::Process(err.to_string()))?
        {
            return Err(PlayerError::Process(format!(
                "Spotify Soloist exited early with status {status}"
            )));
        }

        if let Ok(raw) = fs::read_to_string(&ws_port_path)
            && let Ok(port) = raw.trim().parse::<u16>()
        {
            break port;
        }

        if Instant::now() >= deadline {
            return Err(PlayerError::Process(
                "timed out waiting for Spotify Soloist WebSocket port".to_string(),
            ));
        }

        tokio::time::sleep(WS_PORT_POLL_INTERVAL).await;
    };

    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .map_err(|err| PlayerError::WebSocket(err.to_string()))?;

    let is_active = wait_for_auth_ready(&mut ws).await?;
    if !is_active {
        let activate = serde_json::json!({
            "type": "command",
            "command": "activate",
        });
        ws.send(Message::Text(activate.to_string().into()))
            .await
            .map_err(|err| PlayerError::WebSocket(err.to_string()))?;
    }

    let get_queue = serde_json::json!({
        "type": "command",
        "command": "get_queue",
        "limit": 20,
    });
    ws.send(Message::Text(get_queue.to_string().into()))
        .await
        .map_err(|err| PlayerError::WebSocket(err.to_string()))?;

    Ok(ActiveSoloist { _child: child, ws })
}

async fn wait_for_auth_ready(
    ws: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
) -> Result<bool, PlayerError> {
    tokio::time::timeout(AUTH_READY_TIMEOUT, async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(text))) => {
                    if let Some(auth) = parse_auth_state(text.as_str()) {
                        if auth.logged_in {
                            return Ok(auth.is_active);
                        }
                    } else if let Ok(value) =
                        serde_json::from_str::<serde_json::Value>(text.as_str())
                        && value.get("type").and_then(serde_json::Value::as_str)
                            == Some("playback_state")
                    {
                        let is_active = value
                            .get("is_active")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false);
                        return Ok(is_active);
                    }
                }
                Some(Ok(Message::Close(_))) | None => {
                    return Err(PlayerError::WebSocket(
                        "Spotify Soloist WebSocket closed before auth_state".to_string(),
                    ));
                }
                Some(Err(err)) => return Err(PlayerError::WebSocket(err.to_string())),
                Some(Ok(_)) => {}
            }
        }
    })
    .await
    .map_err(|_| {
        PlayerError::Process("timed out waiting for Spotify Soloist auth_state".to_string())
    })?
}

fn create_dir_all(path: &Path) -> Result<(), PlayerError> {
    fs::create_dir_all(path).map_err(|source| PlayerError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn needs_binary_refresh(soloist_dir: &Path, binary_path: &Path) -> Result<bool, PlayerError> {
    if !binary_path.exists() {
        return Ok(true);
    }

    let marker_path = soloist_dir.join(INSTALLED_AT_MARKER_NAME);
    match fs::metadata(&marker_path) {
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(true),
        Err(source) => Err(PlayerError::Io {
            path: marker_path,
            source,
        }),
        Ok(meta) => {
            let modified = meta.modified().map_err(|source| PlayerError::Io {
                path: marker_path,
                source,
            })?;
            let age = SystemTime::now()
                .duration_since(modified)
                .unwrap_or(Duration::ZERO);
            Ok(age.as_secs() > SOLOIST_MAX_AGE_SECS)
        }
    }
}

async fn download_and_unpack_soloist(
    soloist_dir: &Path,
    binary_path: &Path,
) -> Result<(), PlayerError> {
    eprintln!("Downloading Spotify Soloist headless audio engine...");
    let url = soloist_download_url(std::env::consts::ARCH)?;
    let client = reqwest::Client::builder()
        .timeout(DOWNLOAD_TIMEOUT)
        .build()
        .map_err(|err| PlayerError::Download(err.to_string()))?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|err| PlayerError::Download(err.to_string()))?
        .error_for_status()
        .map_err(|err| PlayerError::Download(err.to_string()))?;
    let bytes = response
        .bytes()
        .await
        .map_err(|err| PlayerError::Download(err.to_string()))?;

    let dir = soloist_dir.to_path_buf();
    let dest = binary_path.to_path_buf();
    tokio::task::spawn_blocking(move || unpack_soloist_binary(&bytes, &dir, &dest))
        .await
        .map_err(|err| PlayerError::Download(format!("unpack task failed: {err}")))??;

    eprintln!("Spotify Soloist installed.");
    Ok(())
}

fn unpack_soloist_binary(
    archive_bytes: &[u8],
    soloist_dir: &Path,
    binary_path: &Path,
) -> Result<(), PlayerError> {
    let gz = GzDecoder::new(archive_bytes);
    let mut archive = Archive::new(gz);
    let entries = archive
        .entries()
        .map_err(|err| PlayerError::Download(format!("invalid Soloist archive: {err}")))?;

    let tmp_path = soloist_dir.join(".soloist.tmp");
    let mut found = false;
    for entry_res in entries {
        let mut entry = entry_res
            .map_err(|err| PlayerError::Download(format!("failed reading archive entry: {err}")))?;
        let entry_path = entry
            .path()
            .map_err(|err| PlayerError::Download(format!("invalid entry path: {err}")))?;
        if entry_path.file_name().and_then(|n| n.to_str()) == Some(SOLOIST_BIN_NAME) {
            entry.unpack(&tmp_path).map_err(|source| PlayerError::Io {
                path: tmp_path.clone(),
                source,
            })?;
            fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o755)).map_err(
                |source| PlayerError::Io {
                    path: tmp_path.clone(),
                    source,
                },
            )?;
            fs::rename(&tmp_path, binary_path).map_err(|source| PlayerError::Io {
                path: binary_path.to_path_buf(),
                source,
            })?;
            found = true;
            break;
        }
    }

    if !found {
        return Err(PlayerError::Download(
            "Soloist archive did not contain the soloist binary".to_string(),
        ));
    }

    write_private_marker(&soloist_dir.join(INSTALLED_AT_MARKER_NAME))?;
    Ok(())
}

async fn setup_api_key(
    soloist_dir: &Path,
    binary_path: &Path,
    data_dir: &Path,
    marker_path: &Path,
) -> Result<(), PlayerError> {
    eprintln!(
        "One-time audio setup (Spotify Soloist):\n\
         1. Open this link in your browser and generate your personal Soloist API key:\n\n\
         {SOLOIST_API_KEY_URL}\n\n\
         2. Paste your Spotify Soloist API key here and press Enter:"
    );

    let mut line = String::new();
    BufReader::new(tokio::io::stdin())
        .read_line(&mut line)
        .await
        .map_err(|err| PlayerError::Setup(format!("failed reading API key from stdin: {err}")))?;
    let api_key = line.trim();
    if api_key.is_empty() {
        return Err(PlayerError::Setup(
            "Spotify Soloist API key cannot be empty".to_string(),
        ));
    }

    let mut child = build_soloist_process(soloist_dir, binary_path)
        .args(["setup-api-key", "--data-dir"])
        .arg(data_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| PlayerError::Io {
            path: binary_path.to_path_buf(),
            source,
        })?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(format!("{api_key}\n").as_bytes())
            .await
            .map_err(|err| {
                PlayerError::Setup(format!("failed writing API key to Soloist: {err}"))
            })?;
        stdin
            .shutdown()
            .await
            .map_err(|err| PlayerError::Setup(format!("failed closing Soloist stdin: {err}")))?;
    }

    let output = child
        .wait_with_output()
        .await
        .map_err(|err| PlayerError::Setup(format!("failed waiting for setup-api-key: {err}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = if stderr.is_empty() {
            format!("soloist setup-api-key exited with status {}", output.status)
        } else {
            format!(
                "soloist setup-api-key exited with status {}: {stderr}",
                output.status
            )
        };
        return Err(PlayerError::Setup(detail));
    }

    write_private_marker(marker_path)
}

async fn pair_device(
    soloist_dir: &Path,
    binary_path: &Path,
    data_dir: &Path,
    marker_path: &Path,
) -> Result<(), PlayerError> {
    eprintln!(
        "One-time device pairing:\n\
         1. Open the Spotify mobile app on your phone (must be on the same Wi-Fi network — the web browser cannot discover local devices).\n\
         2. Open any song, tap the Devices (speaker) icon at the bottom of Now Playing, and select \"{SOLOIST_DEVICE_NAME}\".\n\
         (After this one tap, violin plays standalone with no phone or Spotify app needed.)"
    );

    let output = build_soloist_process(soloist_dir, binary_path)
        .args(["-n", SOLOIST_DEVICE_NAME, "-D"])
        .arg(data_dir)
        .arg("--pair")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|source| PlayerError::Io {
            path: binary_path.to_path_buf(),
            source,
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = if stderr.is_empty() {
            format!("soloist --pair exited with status {}", output.status)
        } else {
            format!(
                "soloist --pair exited with status {}: {stderr}",
                output.status
            )
        };
        return Err(PlayerError::Setup(detail));
    }

    write_private_marker(marker_path)?;
    eprintln!("Spotify Soloist paired and ready.");
    Ok(())
}

fn write_private_marker(path: &Path) -> Result<(), PlayerError> {
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|source| PlayerError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Album, AlbumId, Track, TrackId};
    use crate::worker::{COMMAND_CHANNEL_CAPACITY, EVENT_CHANNEL_CAPACITY};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn sample_track(id: &str) -> Track {
        Track {
            id: TrackId::new(id).unwrap(),
            title: "Test Track".to_string(),
            artists: vec!["Artist".to_string()],
            album_name: "Album".to_string(),
            duration: Duration::from_secs(180),
        }
    }

    fn missing_dir_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "violin-player-test-{name}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn soloist_download_url_maps_supported_arches_and_rejects_others() {
        assert_eq!(soloist_download_url("x86_64").unwrap(), SOLOIST_URL_X86_64);
        assert_eq!(
            soloist_download_url("aarch64").unwrap(),
            SOLOIST_URL_AARCH64
        );

        let err = soloist_download_url("riscv64").unwrap_err();
        assert!(matches!(err, PlayerError::UnsupportedArch(arch) if arch == "riscv64"));
    }

    #[test]
    fn build_soloist_command_json_serializes_playback_and_ignores_api_commands() {
        let track = sample_track("4iV5W9uYEdYUVa79Axb7Rh");
        let play_json: serde_json::Value =
            serde_json::from_str(&build_soloist_command_json(&Command::PlayTrack(track)).unwrap())
                .unwrap();
        assert_eq!(
            play_json,
            serde_json::json!({
                "type": "command",
                "command": "play",
                "uri": "spotify:track:4iV5W9uYEdYUVa79Axb7Rh"
            })
        );

        for cmd in [Command::PausePlayback, Command::StopPlayback] {
            let pause_json: serde_json::Value =
                serde_json::from_str(&build_soloist_command_json(&cmd).unwrap()).unwrap();
            assert_eq!(
                pause_json,
                serde_json::json!({
                    "type": "command",
                    "command": "pause"
                })
            );
        }

        let resume_json: serde_json::Value =
            serde_json::from_str(&build_soloist_command_json(&Command::ResumePlayback).unwrap())
                .unwrap();
        assert_eq!(
            resume_json,
            serde_json::json!({
                "type": "command",
                "command": "play"
            })
        );

        let skip_next_json: serde_json::Value =
            serde_json::from_str(&build_soloist_command_json(&Command::SkipNext).unwrap()).unwrap();
        assert_eq!(
            skip_next_json,
            serde_json::json!({
                "type": "command",
                "command": "skip_next"
            })
        );

        let skip_prev_json: serde_json::Value =
            serde_json::from_str(&build_soloist_command_json(&Command::SkipPrevious).unwrap())
                .unwrap();
        assert_eq!(
            skip_prev_json,
            serde_json::json!({
                "type": "command",
                "command": "skip_prev"
            })
        );

        let volume_json: serde_json::Value =
            serde_json::from_str(&build_soloist_command_json(&Command::SetVolume(65)).unwrap())
                .unwrap();
        assert_eq!(
            volume_json,
            serde_json::json!({
                "type": "command",
                "command": "set_volume",
                "volume": 65
            })
        );

        let seek_json: serde_json::Value = serde_json::from_str(
            &build_soloist_command_json(&Command::Seek(Duration::from_millis(12_345))).unwrap(),
        )
        .unwrap();
        assert_eq!(
            seek_json,
            serde_json::json!({
                "type": "command",
                "command": "seek",
                "position_ms": 12_345
            })
        );

        assert_eq!(
            build_soloist_command_json(&Command::Search("bach".to_string())),
            None
        );
        assert_eq!(build_soloist_command_json(&Command::FetchSavedAlbums), None);
        let album = Album {
            id: AlbumId::new("a1").unwrap(),
            name: "Album".to_string(),
            artists: vec!["Artist".to_string()],
            release_year: Some(2020),
            total_tracks: 10,
        };
        assert_eq!(
            build_soloist_command_json(&Command::FetchAlbumTracks {
                album,
                auto_play: false,
            }),
            None
        );
        let track = Track {
            id: TrackId::new("t1").unwrap(),
            title: "One".to_string(),
            artists: vec!["Artist".to_string()],
            album_name: "Album".to_string(),
            duration: Duration::from_secs(60),
        };
        assert_eq!(
            build_soloist_command_json(&Command::FetchTrackRadio(track)),
            None
        );
    }

    #[test]
    fn parse_auth_state_parses_logged_in_and_active_flags_and_ignores_others() {
        let ready_inactive =
            r#"{"type":"auth_state","logged_in":true,"is_active":false,"device_name":"violin"}"#;
        assert_eq!(
            parse_auth_state(ready_inactive),
            Some(SoloistAuthState {
                logged_in: true,
                is_active: false,
            })
        );

        let ready_active =
            r#"{"type":"auth_state","logged_in":true,"is_active":true,"device_name":"violin"}"#;
        assert_eq!(
            parse_auth_state(ready_active),
            Some(SoloistAuthState {
                logged_in: true,
                is_active: true,
            })
        );

        let not_logged_in = r#"{"type":"auth_state","logged_in":false}"#;
        assert_eq!(
            parse_auth_state(not_logged_in),
            Some(SoloistAuthState {
                logged_in: false,
                is_active: false,
            })
        );

        assert_eq!(
            parse_auth_state(r#"{"type":"playback_state","status":"playing"}"#),
            None
        );
        assert_eq!(parse_auth_state("not json"), None);
    }

    #[test]
    fn parse_soloist_event_handles_position_state_errors_and_invalid_json() {
        let pos_sync = r#"{"type":"position_sync","position":{"position_ms":45000}}"#;
        assert_eq!(
            parse_soloist_event(pos_sync, None),
            Some(AppEvent::PlaybackPositionUpdated(Duration::from_millis(
                45_000
            )))
        );

        let playing_item_state = r#"{"type":"playback_state","status":"playing","item":{"uri":"spotify:track:1"},"position":{"position_ms":1234},"is_active":true}"#;
        assert_eq!(
            parse_soloist_event(playing_item_state, Some("spotify:track:1")),
            Some(AppEvent::PlaybackPositionUpdated(Duration::from_millis(
                1234
            )))
        );

        let idle_with_item = r#"{"type":"playback_state","status":"idle","item":{"uri":"spotify:track:1"},"position":{"position_ms":500}}"#;
        assert_eq!(
            parse_soloist_event(idle_with_item, Some("spotify:track:1")),
            Some(AppEvent::PlaybackPositionUpdated(Duration::from_millis(
                500
            )))
        );

        let stopped_state = r#"{"type":"playback_state","status":"stopped","item":null}"#;
        assert_eq!(
            parse_soloist_event(stopped_state, Some("spotify:track:1")),
            Some(AppEvent::TrackEnded)
        );

        let idle_after_track = r#"{"type":"playback_state","status":"idle","item":null}"#;
        assert_eq!(
            parse_soloist_event(idle_after_track, Some("spotify:track:1")),
            Some(AppEvent::TrackEnded)
        );
        assert_eq!(parse_soloist_event(idle_after_track, None), None);

        let idle_legacy_track = r#"{"type":"playback_state","status":"idle","track":null}"#;
        assert_eq!(
            parse_soloist_event(idle_legacy_track, Some("spotify:track:1")),
            Some(AppEvent::TrackEnded)
        );

        let failed_cmd =
            r#"{"type":"command_result","command":"play","success":false,"error":"not paired"}"#;
        assert_eq!(
            parse_soloist_event(failed_cmd, None),
            Some(AppEvent::PlaybackFailed("not paired".to_string()))
        );

        let ok_cmd = r#"{"type":"command_result","command":"play","success":true}"#;
        assert_eq!(parse_soloist_event(ok_cmd, None), None);

        let volume_changed = r#"{"type":"volume_changed","volume":75}"#;
        assert_eq!(
            parse_soloist_event(volume_changed, None),
            Some(AppEvent::VolumeChanged(75))
        );
        let volume_clamped = r#"{"type":"volume_changed","volume":150}"#;
        assert_eq!(
            parse_soloist_event(volume_clamped, None),
            Some(AppEvent::VolumeChanged(100))
        );

        assert_eq!(parse_soloist_event(r#"{"type":"unknown"}"#, None), None);
        assert_eq!(parse_soloist_event("not json", None), None);
    }

    #[test]
    fn parse_soloist_event_syncs_track_on_uri_change() {
        let state = r#"{
            "type": "playback_state",
            "status": "paused",
            "position": {"position_ms": 15000},
            "item": {
                "uri": "spotify:track:222",
                "decorations": {
                    "identity": {"name": "Next Track"},
                    "creators": [
                        {"entity": {"decorations": {"identity": {"name": "Artist One"}}}},
                        {"entity": {"decorations": {"identity": {"name": "Artist Two"}}}}
                    ],
                    "parent": {
                        "entity": {"decorations": {"identity": {"name": "Album Name"}}}
                    },
                    "playback": {"duration_ms": 210000}
                }
            }
        }"#;

        assert_eq!(
            parse_soloist_event(state, Some("spotify:track:111")),
            Some(AppEvent::TrackSynced {
                track: Track {
                    id: TrackId::new("222").unwrap(),
                    title: "Next Track".to_string(),
                    artists: vec!["Artist One".to_string(), "Artist Two".to_string()],
                    album_name: "Album Name".to_string(),
                    duration: Duration::from_millis(210_000),
                },
                position: Duration::from_millis(15_000),
                status: PlaybackStatus::Paused,
            })
        );

        assert_eq!(
            parse_soloist_event(state, Some("spotify:track:222")),
            Some(AppEvent::PlaybackPositionUpdated(Duration::from_millis(
                15_000
            )))
        );
    }

    #[test]
    fn needs_binary_refresh_uses_installed_at_marker_not_binary_mtime() {
        let dir = missing_dir_path("refresh-marker");
        create_dir_all(&dir).unwrap();
        let binary_path = dir.join(SOLOIST_BIN_NAME);

        assert!(needs_binary_refresh(&dir, &binary_path).unwrap());

        write_private_marker(&binary_path).unwrap();
        assert!(needs_binary_refresh(&dir, &binary_path).unwrap());

        write_private_marker(&dir.join(INSTALLED_AT_MARKER_NAME)).unwrap();
        assert!(!needs_binary_refresh(&dir, &binary_path).unwrap());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn run_player_worker_exits_cleanly_and_ignores_idle_controls() {
        let soloist_dir = missing_dir_path("idle-exit");
        let (cmd_tx, cmd_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
        let (event_tx, mut event_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);

        let handle = tokio::spawn(run_player_worker(soloist_dir, cmd_rx, event_tx));

        cmd_tx.send(Command::PausePlayback).await.unwrap();
        cmd_tx.send(Command::StopPlayback).await.unwrap();
        drop(cmd_tx);

        assert!(event_rx.recv().await.is_none());
        handle.await.unwrap();
    }

    #[test]
    fn parse_soloist_event_parses_queue_changed_and_queue_skipping_placeholders() {
        let queue_changed = r#"{
            "type": "queue_changed",
            "upcoming": [
                {
                    "uid": "ph1",
                    "source": "context",
                    "item": {
                        "uri": "spotify:meta:node_rules_placeholder"
                    }
                },
                {
                    "uid": "u1",
                    "source": "context",
                    "item": {
                        "uri": "spotify:track:q1",
                        "decorations": {
                            "identity": {"name": "Air on the G String"},
                            "creators": [
                                {"entity": {"decorations": {"identity": {"name": "Bach"}}}}
                            ],
                            "parent": {
                                "entity": {"decorations": {"identity": {"name": "Orchestral Suites"}}}
                            },
                            "playback": {"duration_ms": 200000}
                        }
                    }
                }
            ]
        }"#;

        assert_eq!(
            parse_soloist_event(queue_changed, None),
            Some(AppEvent::QueueUpdated(vec![Track {
                id: TrackId::new("q1").unwrap(),
                title: "Air on the G String".to_string(),
                artists: vec!["Bach".to_string()],
                album_name: "Orchestral Suites".to_string(),
                duration: Duration::from_millis(200_000),
            }]))
        );

        let queue_fallback = r#"{
            "type": "queue",
            "tracks": [
                {
                    "uri": "spotify:track:q2",
                    "name": "Chaconne",
                    "artists": [{"name": "Bach"}],
                    "album": {"name": "Partita No. 2"}
                }
            ]
        }"#;
        assert_eq!(
            parse_soloist_event(queue_fallback, None),
            Some(AppEvent::QueueUpdated(vec![Track {
                id: TrackId::new("q2").unwrap(),
                title: "Chaconne".to_string(),
                artists: vec!["Bach".to_string()],
                album_name: "Partita No. 2".to_string(),
                duration: Duration::ZERO,
            }]))
        );
    }

    #[test]
    fn bundled_loader_invocation_returns_none_when_absent_and_builds_args_for_x86_64_and_aarch64() {
        let soloist_dir = missing_dir_path("bundled-loader");
        let binary_path = soloist_dir.join(SOLOIST_BIN_NAME);
        create_dir_all(&soloist_dir).unwrap();

        assert_eq!(
            bundled_loader_invocation(&soloist_dir, &binary_path, "x86_64"),
            None
        );

        let lib_dir = soloist_dir.join("lib");
        create_dir_all(&lib_dir).unwrap();
        let x86_loader = lib_dir.join("ld-linux-x86-64.so.2");
        write_private_marker(&x86_loader).unwrap();

        let (loader, args) =
            bundled_loader_invocation(&soloist_dir, &binary_path, "x86_64").unwrap();
        assert_eq!(loader, x86_loader);
        assert_eq!(
            args,
            vec![
                OsString::from("--library-path"),
                OsString::from(format!(
                    "{}:/usr/lib/x86_64-linux-gnu:/lib/x86_64-linux-gnu:/usr/lib64:/usr/lib",
                    lib_dir.display()
                )),
                binary_path.as_os_str().to_os_string(),
            ]
        );

        let arm_loader = lib_dir.join("ld-linux-aarch64.so.1");
        write_private_marker(&arm_loader).unwrap();
        let (loader, args) =
            bundled_loader_invocation(&soloist_dir, &binary_path, "aarch64").unwrap();
        assert_eq!(loader, arm_loader);
        assert_eq!(
            args,
            vec![
                OsString::from("--library-path"),
                OsString::from(format!(
                    "{}:/usr/lib/aarch64-linux-gnu:/lib/aarch64-linux-gnu:/usr/lib64:/usr/lib",
                    lib_dir.display()
                )),
                binary_path.as_os_str().to_os_string(),
            ]
        );

        assert_eq!(
            bundled_loader_invocation(&soloist_dir, &binary_path, "riscv64"),
            None
        );

        fs::remove_dir_all(&soloist_dir).unwrap();
    }

    #[test]
    fn should_suppress_stale_sync_respects_grace_window_and_matching_uri() {
        let now = Instant::now();
        let pending = Some(("spotify:track:expected".to_string(), now));

        assert!(should_suppress_stale_sync(
            &pending,
            now + Duration::from_secs(1),
            "spotify:track:stale"
        ));
        assert!(!should_suppress_stale_sync(
            &pending,
            now + Duration::from_secs(1),
            "spotify:track:expected"
        ));
        assert!(!should_suppress_stale_sync(
            &pending,
            now + Duration::from_secs(5),
            "spotify:track:stale"
        ));
        assert!(!should_suppress_stale_sync(
            &None,
            now + Duration::from_secs(1),
            "spotify:track:stale"
        ));
    }
}
