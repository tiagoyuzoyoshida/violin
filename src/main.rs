use std::{
    error::Error,
    io,
    process::ExitCode,
    sync::Arc,
    time::{Duration, Instant},
};

use crossterm::event::{self, Event};
use ratatui::layout::Rect;
use tokio::sync::mpsc;

use violin::{
    app::{AppEvent, AppState, Command},
    art::{ArtProtocol, BAR_CELL_W, KITTY_BAR_ALT_IMAGE_ID, KITTY_BAR_IMAGE_ID},
    auth::{AuthManager, TokenStore},
    config::{AppPaths, ArtStyle, CLIENT_ID_ENV_VAR, Config},
    domain::{PlaybackStatus, TrackId},
    keymap::Keymap,
    player,
    spotify::SpotifyClient,
    terminal::TerminalGuard,
    ui::{self, OverlayRects},
    worker::{self, COMMAND_CHANNEL_CAPACITY, EVENT_CHANNEL_CAPACITY},
};

const TICK_INTERVAL: Duration = Duration::from_millis(100);
const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(50);

type KittyBarOverlay = (u32, Rect, u32, [u8; 3], [u8; 3]);

#[tokio::main]
async fn main() -> ExitCode {
    let _ = rustls::crypto::ring::default_provider().install_default();
    match std::env::args().nth(1).as_deref() {
        None => {
            if let Err(err) = run_app().await {
                eprintln!("violin: {err}");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Some("login") => {
            if let Err(err) = run_login().await {
                eprintln!("violin: {err}");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Some("logout") => {
            if let Err(err) = run_logout() {
                eprintln!("violin: {err}");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Some("update") => {
            if let Err(err) = run_update_cmd().await {
                eprintln!("violin: {err}");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Some("--version") | Some("-V") => {
            println!("violin {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--help") | Some("-h") => {
            println!(
                "violin {}\n\nTerminal Spotify client.\n\nUsage:\n  violin [COMMAND | OPTIONS]\n\nCommands:\n  login            Log in to Spotify via browser link\n  logout           Remove cached Spotify tokens\n  update           Update violin to the latest release\n\nOptions:\n  -h, --help       Print help\n  -V, --version    Print version\n\nEnvironment:\n  {CLIENT_ID_ENV_VAR}  Optional custom Spotify client ID (overrides config.toml)",
                env!("CARGO_PKG_VERSION")
            );
            ExitCode::SUCCESS
        }
        Some(unknown) => {
            eprintln!(
                "violin: unknown command or option '{unknown}'\nTry 'violin --help' for usage."
            );
            ExitCode::from(2)
        }
    }
}

async fn run_login() -> Result<(), Box<dyn Error>> {
    let config = Config::load()?;
    let token_store = TokenStore::new(config.paths.tokens_file);
    let auth = AuthManager::new(config.client_id, token_store)?;
    auth.login().await?;
    player::ensure_soloist_ready(&config.paths.soloist_dir).await?;
    Ok(())
}

fn run_logout() -> Result<(), Box<dyn Error>> {
    let paths = AppPaths::resolve()?;
    if TokenStore::new(paths.tokens_file).clear()? {
        println!("Logged out (removed cached Spotify tokens).");
    } else {
        println!("Already logged out (no cached Spotify tokens found).");
    }
    player::clear_soloist_pairing(&paths.soloist_dir)?;
    Ok(())
}

async fn run_update_cmd() -> Result<(), Box<dyn Error>> {
    match violin::update::run_update().await? {
        violin::update::UpdateOutcome::UpToDate { current, .. } => {
            println!("violin {current} is already up to date.");
        }
        violin::update::UpdateOutcome::Updated {
            from,
            to,
            binary_path,
        } => {
            println!(
                "Updated violin v{from} -> v{to} ({})",
                binary_path.display()
            );
        }
    }
    Ok(())
}

async fn run_app() -> Result<(), Box<dyn Error>> {
    let config = Config::load()?;
    let token_store = TokenStore::new(config.paths.tokens_file);
    let auth = Arc::new(AuthManager::new(config.client_id, token_store)?);
    auth.ensure_valid_token().await?;
    player::ensure_soloist_ready(&config.paths.soloist_dir).await?;

    let spotify_client = SpotifyClient::new()?;
    let (spotify_cmd_tx, spotify_cmd_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
    let (player_cmd_tx, player_cmd_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
    let (event_tx, mut event_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);

    let spotify_worker_handle = tokio::spawn(worker::run_spotify_worker(
        Arc::clone(&auth),
        spotify_client,
        spotify_cmd_rx,
        event_tx.clone(),
    ));
    let player_worker_handle = tokio::spawn(player::run_player_worker(
        config.paths.soloist_dir,
        player_cmd_rx,
        event_tx,
    ));

    let _ = spotify_cmd_tx.try_send(Command::FetchSavedAlbums);
    let _ = spotify_cmd_tx.try_send(Command::FetchJamSession);
    let tui_result = run_tui(
        &spotify_cmd_tx,
        &player_cmd_tx,
        &mut event_rx,
        &config.keymap,
        config.art_style,
    );

    drop(spotify_cmd_tx);
    drop(player_cmd_tx);
    spotify_worker_handle.abort();
    player_worker_handle.abort();
    tui_result?;

    Ok(())
}

fn run_tui(
    spotify_cmd_tx: &mpsc::Sender<Command>,
    player_cmd_tx: &mpsc::Sender<Command>,
    event_rx: &mut mpsc::Receiver<AppEvent>,
    keymap: &Keymap,
    art_style: ArtStyle,
) -> io::Result<()> {
    let mut terminal = TerminalGuard::init()?;
    let mut state = AppState::new();
    state.retro_art = art_style == ArtStyle::Retro;
    let mut last_tick = Instant::now();
    let detected_protocol = violin::art::detect_art_protocol();
    let mut kitty_overlay: Option<(TrackId, Rect)> = None;
    let mut kitty_bar_overlay: Option<KittyBarOverlay> = None;

    while !state.should_quit {
        while let Ok(worker_event) = event_rx.try_recv() {
            let commands = state.handle_event(worker_event);
            forward_commands(spotify_cmd_tx, player_cmd_tx, commands);
        }

        let effective_art_protocol = if state.retro_art {
            ArtProtocol::HalfBlock
        } else {
            detected_protocol
        };
        let mut overlays = OverlayRects::default();
        terminal.draw(|frame| {
            overlays =
                ui::render_with_protocol(frame, &state, effective_art_protocol, detected_protocol);
        })?;

        if effective_art_protocol == ArtProtocol::Kitty {
            if state.focus_mode
                && let Some(art) = &state.cover_art
                && let Some(rect) = overlays.art_rect
            {
                let next_overlay = (art.track_id.clone(), rect);
                if kitty_overlay.as_ref() != Some(&next_overlay) {
                    if let Some((_, prev_rect)) = kitty_overlay
                        && prev_rect != rect
                    {
                        terminal.clear_kitty_image()?;
                    }
                    terminal
                        .write_kitty_image(&violin::art::kitty_transmit_and_place(art, rect))?;
                    kitty_overlay = Some(next_overlay);
                }
            } else if kitty_overlay.is_some() {
                terminal.clear_kitty_image()?;
                kitty_overlay = None;
            }
        } else if kitty_overlay.is_some() {
            terminal.clear_kitty_image()?;
            kitty_overlay = None;
        }

        if detected_protocol == ArtProtocol::Kitty
            && let Some(rect) = overlays.bar_rect
        {
            let ratio = match &state.now_playing {
                Some(np) if np.track.duration.as_millis() > 0 => {
                    let pos = np.position.min(np.track.duration);
                    (pos.as_secs_f64() / np.track.duration.as_secs_f64()).clamp(0.0, 1.0)
                }
                _ => 0.0,
            };
            let is_playing = state
                .now_playing
                .as_ref()
                .is_some_and(|np| np.status == PlaybackStatus::Playing);
            let (filled, empty) = if state.focus_mode && !is_playing {
                ([110, 110, 110], [45, 45, 45])
            } else {
                ([125, 207, 255], [86, 95, 137])
            };
            let fill_qpx =
                (ratio * f64::from(rect.width) * f64::from(BAR_CELL_W) * 4.0).round() as u32;
            let next_key = (rect, fill_qpx, filled, empty);
            if kitty_bar_overlay.map(|(_, r, q, f, e)| (r, q, f, e)) != Some(next_key) {
                let prev_id = kitty_bar_overlay.map(|(id, ..)| id);
                let next_id = if prev_id == Some(KITTY_BAR_IMAGE_ID) {
                    KITTY_BAR_ALT_IMAGE_ID
                } else {
                    KITTY_BAR_IMAGE_ID
                };
                let png = violin::art::render_progress_bar_png(rect.width, ratio, filled, empty);
                terminal.write_kitty_image(&violin::art::kitty_swap_image_png(
                    &png, rect, next_id, prev_id,
                ))?;
                kitty_bar_overlay = Some((next_id, rect, fill_qpx, filled, empty));
            }
        } else if let Some((active_id, ..)) = kitty_bar_overlay {
            terminal.clear_kitty_image_id(active_id)?;
            kitty_bar_overlay = None;
        }

        let poll_timeout = TICK_INTERVAL
            .saturating_sub(last_tick.elapsed())
            .min(EVENT_POLL_INTERVAL);
        if event::poll(poll_timeout)?
            && let Event::Key(key) = event::read()?
            && let Some(action) = keymap.map_key(state.input_mode, key)
        {
            let commands = state.handle_action(action);
            forward_commands(spotify_cmd_tx, player_cmd_tx, commands);
        }

        if last_tick.elapsed() >= TICK_INTERVAL {
            let elapsed = last_tick.elapsed();
            last_tick = Instant::now();
            let commands = state.handle_event(AppEvent::PlaybackTick(elapsed));
            forward_commands(spotify_cmd_tx, player_cmd_tx, commands);
        }
    }

    Ok(())
}

fn forward_commands(
    spotify_cmd_tx: &mpsc::Sender<Command>,
    player_cmd_tx: &mpsc::Sender<Command>,
    commands: Vec<Command>,
) {
    for cmd in commands {
        match cmd {
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
            | Command::LeaveJamSession { .. }
            | Command::FetchCoverArt { .. } => {
                let _ = spotify_cmd_tx.try_send(cmd);
            }
            Command::PlayTrack(_)
            | Command::PausePlayback
            | Command::ResumePlayback
            | Command::StopPlayback
            | Command::SkipNext
            | Command::SkipPrevious
            | Command::Seek(_)
            | Command::SetVolume(_) => {
                let _ = player_cmd_tx.try_send(cmd);
            }
        }
    }
}
