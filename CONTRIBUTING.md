# Contributing to violin

Thanks for your interest in contributing to `violin`! Bug reports, feature ideas, and pull requests are all welcome.

## Project culture

`violin` is intentionally **opinionated, lightweight, and keyboard-first**. Every design decision is guided by user value — speed, low RAM, reliability, and simplicity:

- **Smallest clean change:** Keep pull requests focused on one problem. Avoid drive-by refactors, speculative abstractions, or unrelated reformatting.
- **No unnecessary dependencies:** Prefer `std` and existing crates over adding new dependencies. Never introduce `openssl-sys` or system C-library build requirements (`rustls` only).
- **No UI clutter:** Keep the TUI minimal and clean. Keybinding hints belong in `README.md`, not inside the terminal UI.
- **Docs move with code:** If a PR changes a keybinding, CLI option, config key, or dependency, update `README.md` in the same PR.

## Development setup

Requires Rust `1.93+` (2024 edition). No system OpenSSL or ALSA development headers are needed.

```bash
git clone https://github.com/tiagoyuzoyoshida/violin.git
cd violin
cargo test --all-targets
cargo run
```

## Architecture at a glance

`violin` uses a unidirectional, pure-core architecture so core behavior is fast and deterministic to test offline:

- `src/domain.rs` — Typed domain models (`Track`, `Album`, `LibraryItem`, `PlaybackQueue`, `JamSession`, `CoverArt`). Pure data; zero I/O.
- `src/app.rs` — Pure state machine (`AppState`, `Action`, `AppEvent`, `Command`). Reduces inputs and events into updated state and side-effect commands. Zero I/O.
- `src/keymap.rs` — Translates terminal key events into `Action`, with user overrides from `config.toml`.
- `src/ui.rs` — Pure `ratatui` rendering of `&AppState` (read-only; zero I/O).
- `src/art.rs` — Cover-art decoding and Kitty / 2×2 quadrant-block renderers.
- `src/auth.rs`, `src/spotify.rs`, `src/player.rs`, `src/worker.rs` — Async background workers for OAuth2 PKCE, Spotify desktop endpoints (`spclient`/Pathfinder), and headless Spotify `soloist` playback.
- `src/update.rs` — Self-updater (`violin update`).
- `src/main.rs` — Thin composition root and terminal event loop.

## Engineering standards

- **Pure core, effects at the edges:** Never import `reqwest`, `tokio`, `crossterm`, `ratatui`, `librespot`, or `image` into `src/domain.rs` or `src/app.rs`.
- **All input flows through `Action`:** Never mutate `AppState` directly from raw key events. Map keys in `src/keymap.rs` into an `Action` so user-configured `[keybindings]` work everywhere.
- **Non-blocking UI loop & clean terminal restore:** Network calls and audio control must run in background workers over bounded async channels, never inside the render/input loop. Terminal raw mode must always restore cleanly on exit or panic.
- **Fail loudly & use strong types:** Validate inputs at module boundaries, use typed errors (`thiserror`) and enums/newtypes (`TrackId`, `AlbumId`), and never swallow errors with silent fallbacks.
- **No secrets:** Never commit or log OAuth tokens, refresh tokens, API keys, or machine-specific paths.
- **Fix causes, not symptoms:** Every bug fix or behavior change should include a fast, deterministic, offline unit test (using `ratatui::backend::TestBackend` for UI changes) that fails without the fix.

## Before opening a pull request

Run the full CI check suite locally and make sure everything passes with zero warnings:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
bash -n install.sh
! grep -q 'name = "openssl-sys"' Cargo.lock
```

## Good places to help

- Artist profile view (top tracks & albums)
- Playlist sorting (e.g. most recently added, matching standard Spotify)
- Packaging for Linux distributions (AUR, Nix, Homebrew Linux, `.deb`/`.rpm`)
- Bug reports with reproduction steps and distro/terminal details
