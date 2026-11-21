use std::{
    error::Error,
    io,
    process::ExitCode,
    sync::Arc,
    time::{Duration, Instant},
};

use crossterm::event::{self, Event};
use tokio::sync::mpsc;

use violin::{
    app::{AppEvent, AppState, Command},
    auth::{AuthManager, TokenStore},
    config::{AppPaths, CLIENT_ID_ENV_VAR, Config},
    keymap::Keymap,
    player,
    spotify::SpotifyClient,
    terminal::TerminalGuard,
    ui,
    worker::{self, COMMAND_CHANNEL_CAPACITY, EVENT_CHANNEL_CAPACITY},
};

const TICK_INTERVAL: Duration = Duration::from_millis(100);
const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(50);

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
        Some("--version") | Some("-V") => {
            println!("violin {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--help") | Some("-h") => {
            println!(
                "violin {}\n\nTerminal Spotify client.\n\nUsage:\n  violin [COMMAND | OPTIONS]\n\nCommands:\n  login            Log in to Spotify via browser link\n  logout           Remove cached Spotify tokens\n\nOptions:\n  -h, --help       Print help\n  -V, --version    Print version\n\nEnvironment:\n  {CLIENT_ID_ENV_VAR}  Optional custom Spotify client ID (overrides config.toml)",
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
) -> io::Result<()> {
    let mut terminal = TerminalGuard::init()?;
    let mut state = AppState::new();
    let mut last_tick = Instant::now();

    while !state.should_quit {
        while let Ok(worker_event) = event_rx.try_recv() {
            let commands = state.handle_event(worker_event);
            forward_commands(spotify_cmd_tx, player_cmd_tx, commands);
        }

        terminal.draw(|frame| ui::render(frame, &state))?;

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
            | Command::FetchJamSession
            | Command::CreateJamSession
            | Command::JoinJamSession(_)
            | Command::LeaveJamSession { .. } => {
                let _ = spotify_cmd_tx.try_send(cmd);
            }
            Command::PlayTrack(_)
            | Command::PausePlayback
            | Command::ResumePlayback
            | Command::StopPlayback
            | Command::SkipNext
            | Command::SkipPrevious
            | Command::Seek(_)
            | Command::SetShuffle(_)
            | Command::SetVolume(_) => {
                let _ = player_cmd_tx.try_send(cmd);
            }
        }
    }
}
