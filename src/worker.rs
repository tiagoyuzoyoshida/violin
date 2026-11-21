use std::sync::Arc;

use librespot_core::Session;
use librespot_core::authentication::Credentials;
use librespot_core::config::SessionConfig;
use tokio::sync::mpsc;

use crate::app::{AppEvent, Command};
use crate::auth::AuthManager;
use crate::spotify::SpotifyClient;

pub const COMMAND_CHANNEL_CAPACITY: usize = 32;
pub const EVENT_CHANNEL_CAPACITY: usize = 64;

/// Runs the background Spotify desktop API worker until `cmd_rx` closes.
pub async fn run_spotify_worker(
    auth: Arc<AuthManager>,
    client: SpotifyClient,
    mut cmd_rx: mpsc::Receiver<Command>,
    event_tx: mpsc::Sender<AppEvent>,
) {
    let mut session: Option<Session> = None;

    while let Some(cmd) = cmd_rx.recv().await {
        let event = match cmd {
            Command::Search(query) => match ensure_session(&auth, &mut session).await {
                Ok(sess) => match client.search(&sess, &query).await {
                    Ok(results) => AppEvent::SearchCompleted { query, results },
                    Err(err) => AppEvent::Error(err.to_string()),
                },
                Err(msg) => AppEvent::Error(msg),
            },
            Command::FetchSavedAlbums => match ensure_session(&auth, &mut session).await {
                Ok(sess) => match client.load_library(&sess).await {
                    Ok((items, liked_track_ids)) => AppEvent::SavedAlbumsLoaded {
                        items,
                        liked_track_ids,
                    },
                    Err(err) => AppEvent::Error(err.to_string()),
                },
                Err(msg) => AppEvent::Error(msg),
            },
            Command::FetchAlbumTracks { album, auto_play } => {
                match ensure_session(&auth, &mut session).await {
                    Ok(sess) => match client.album_tracks(&sess, &album).await {
                        Ok(tracks) => AppEvent::AlbumTracksLoaded {
                            album,
                            tracks,
                            auto_play,
                        },
                        Err(err) => AppEvent::Error(err.to_string()),
                    },
                    Err(msg) => AppEvent::Error(msg),
                }
            }
            Command::FetchTrackRadio(track) => match ensure_session(&auth, &mut session).await {
                Ok(sess) => match client.track_radio(&sess, &track).await {
                    Ok(tracks) => AppEvent::RadioTracksLoaded(tracks),
                    Err(_) => continue,
                },
                Err(_) => continue,
            },
            Command::SetTrackLiked { track, liked } => {
                match ensure_session(&auth, &mut session).await {
                    Ok(sess) => {
                        match client
                            .set_track_liked(&sess, track.id.as_str(), liked)
                            .await
                        {
                            Ok(()) => continue,
                            Err(err) => AppEvent::TrackLikeFailed {
                                track_id: track.id.as_str().to_string(),
                                revert_to_liked: !liked,
                                message: err.to_string(),
                            },
                        }
                    }
                    Err(msg) => AppEvent::TrackLikeFailed {
                        track_id: track.id.as_str().to_string(),
                        revert_to_liked: !liked,
                        message: msg,
                    },
                }
            }
            Command::CreatePlaylist(name) => match ensure_session(&auth, &mut session).await {
                Ok(sess) => match client.create_playlist(&sess, &name).await {
                    Ok(album) => AppEvent::PlaylistCreated(album),
                    Err(err) => AppEvent::Error(err.to_string()),
                },
                Err(msg) => AppEvent::Error(msg),
            },
            Command::AddTrackToPlaylist {
                playlist_id,
                playlist_name,
                track,
            } => match ensure_session(&auth, &mut session).await {
                Ok(sess) => match client
                    .add_track_to_playlist(&sess, &playlist_id, track.id.as_str())
                    .await
                {
                    Ok(()) => AppEvent::TrackAddedToPlaylist {
                        playlist_name,
                        track_title: track.title,
                    },
                    Err(err) => AppEvent::Error(err.to_string()),
                },
                Err(msg) => AppEvent::Error(msg),
            },
            Command::FetchJamSession => match ensure_session(&auth, &mut session).await {
                Ok(sess) => match client.current_jam(&sess).await {
                    Ok(jam) => AppEvent::JamSessionLoaded(jam),
                    Err(err) => AppEvent::Error(err.to_string()),
                },
                Err(msg) => AppEvent::Error(msg),
            },
            Command::CreateJamSession => match ensure_session(&auth, &mut session).await {
                Ok(sess) => match client.create_jam(&sess).await {
                    Ok(jam) => {
                        let copied_to_clipboard = try_copy_to_clipboard(&jam.join_url).await;
                        AppEvent::JamSessionCreated {
                            session: jam,
                            copied_to_clipboard,
                        }
                    }
                    Err(err) => AppEvent::Error(err.to_string()),
                },
                Err(msg) => AppEvent::Error(msg),
            },
            Command::JoinJamSession(input) => match ensure_session(&auth, &mut session).await {
                Ok(sess) => match client.join_jam(&sess, &input).await {
                    Ok(jam) => AppEvent::JamSessionLoaded(Some(jam)),
                    Err(err) => AppEvent::Error(err.to_string()),
                },
                Err(msg) => AppEvent::Error(msg),
            },
            Command::LeaveJamSession {
                session_id,
                is_owner,
            } => match ensure_session(&auth, &mut session).await {
                Ok(sess) => match client.leave_jam(&sess, &session_id, is_owner).await {
                    Ok(()) => AppEvent::JamLeft,
                    Err(err) => AppEvent::Error(err.to_string()),
                },
                Err(msg) => AppEvent::Error(msg),
            },
            Command::PlayTrack(_)
            | Command::PausePlayback
            | Command::ResumePlayback
            | Command::StopPlayback
            | Command::SkipNext
            | Command::SkipPrevious
            | Command::SetShuffle(_)
            | Command::SetVolume(_) => continue,
        };

        if event_tx.send(event).await.is_err() {
            break;
        }
    }
}

async fn ensure_session(auth: &AuthManager, slot: &mut Option<Session>) -> Result<Session, String> {
    if let Some(existing) = slot
        && !existing.is_invalid()
    {
        return Ok(existing.clone());
    }

    let token = auth
        .valid_token_non_interactive()
        .await
        .map_err(|err| err.to_string())?;
    let session = Session::new(SessionConfig::default(), None);
    session
        .connect(Credentials::with_access_token(token), false)
        .await
        .map_err(|err| err.to_string())?;
    *slot = Some(session.clone());
    Ok(session)
}

async fn try_copy_to_clipboard(text: &str) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    let candidates: &[(&str, &[&str])] = &[
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
    ];
    for &(bin, args) in candidates {
        let Ok(mut child) = tokio::process::Command::new(bin)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        else {
            continue;
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = tokio::io::AsyncWriteExt::write_all(&mut stdin, text.as_bytes()).await;
        }
        if let Ok(status) = child.wait().await
            && status.success()
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::TokenStore;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn missing_tokens_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "violin-worker-test-{name}-{}-{nanos}/tokens.json",
            std::process::id()
        ))
    }

    #[tokio::test]
    async fn search_without_tokens_emits_error_event() {
        let auth = Arc::new(
            AuthManager::new(
                "client_id".to_string(),
                TokenStore::new(missing_tokens_path("missing")),
            )
            .unwrap(),
        );
        let client = SpotifyClient::with_pathfinder_url("http://127.0.0.1:1").unwrap();
        let (cmd_tx, cmd_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
        let (event_tx, mut event_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);

        let handle = tokio::spawn(run_spotify_worker(auth, client, cmd_rx, event_tx));

        cmd_tx
            .send(Command::Search("bach".to_string()))
            .await
            .unwrap();
        drop(cmd_tx);

        let event = event_rx.recv().await.unwrap();
        assert!(matches!(event, AppEvent::Error(_)));
        assert!(event_rx.recv().await.is_none());

        handle.await.unwrap();
    }

    #[tokio::test]
    async fn playback_commands_are_ignored_and_worker_exits_when_channel_closes() {
        let auth = Arc::new(
            AuthManager::new(
                "client_id".to_string(),
                TokenStore::new(missing_tokens_path("playback-ignore")),
            )
            .unwrap(),
        );
        let client = SpotifyClient::with_pathfinder_url("http://127.0.0.1:1").unwrap();
        let (cmd_tx, cmd_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
        let (event_tx, mut event_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);

        let handle = tokio::spawn(run_spotify_worker(auth, client, cmd_rx, event_tx));

        cmd_tx.send(Command::PausePlayback).await.unwrap();
        cmd_tx.send(Command::StopPlayback).await.unwrap();
        drop(cmd_tx);

        assert!(event_rx.recv().await.is_none());
        handle.await.unwrap();
    }
}
