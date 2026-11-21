# violin

[![CI](https://github.com/tiagoyuzoyoshida/violin/actions/workflows/ci.yml/badge.svg)](https://github.com/tiagoyuzoyoshida/violin/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/tiagoyuzoyoshida/violin)](https://github.com/tiagoyuzoyoshida/violin/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

`violin` is a lightweight, keyboard-driven Spotify terminal client for Linux. Built in Rust for Spotify Premium users, it ships as a single static binary with standalone headless audio playback—no official desktop or mobile Spotify client required.

<!-- Demo video / GIF placeholder: add link or media here -->

## Features

- **Standalone Linux audio:** Plays audio directly via Spotify's headless `soloist` engine over PipeWire or PulseAudio.
- **Fast search:** Search Spotify tracks and albums and toggle between result sections.
- **Library & playlists:** Browse Liked Songs, playlists, folders, and saved albums; like/unlike tracks, create playlists, and add tracks to playlists.
- **Live queue:** Inspect the current playback queue and jump between tracks.
- **Spotify Jam:** Start a new Jam (auto-copying the invite link to the clipboard), join by link or token, refresh, and leave or end sessions.
- **Custom keybindings:** Remap any Normal-mode action in `~/.config/violin/config.toml`.

## Quick install

Install the latest static Linux release (`x86_64` or `aarch64`) into `~/.local/bin/violin`:

```bash
curl -fsSL https://raw.githubusercontent.com/tiagoyuzoyoshida/violin/main/install.sh | bash
```

Optional environment overrides: `VIOLIN_VERSION` (e.g. `v0.1.0`) and `VIOLIN_INSTALL_DIR`.

## Build from source

Requires Rust `1.93+` (2024 edition); no ALSA development headers or `pkg-config` needed.

```bash
cargo install --git https://github.com/tiagoyuzoyoshida/violin
```

Or clone and build locally:

```bash
git clone https://github.com/tiagoyuzoyoshida/violin.git
cd violin
cargo build --release
cargo test --all-targets
```

## Login & setup

No Spotify Developer App is required.

1. Run `violin` (or `violin login`).
2. Open the printed Spotify authorization URL in your browser and log in with your Spotify Premium account. If the local redirect cannot reach `violin` (for example over SSH), paste the redirected URL from your browser's address bar back into the terminal.
3. On first run, `violin` downloads Spotify's headless `soloist` audio engine into `~/.local/share/violin/soloist/`, prompts you to generate and paste a one-time Soloist API key, and asks you to select `"violin"` once under Devices in any Spotify app to complete Connect pairing.
4. Every subsequent launch runs `soloist` silently in the background with no other Spotify app needed.

Tokens are cached in `~/.local/share/violin/tokens.json` with `0600` permissions. Run `violin logout` to remove cached tokens and clear Soloist device pairing.

### Optional custom `client_id`

To use your own Spotify Developer App instead of the built-in public client ID, register `http://127.0.0.1:8888/callback` as a Redirect URI and set `client_id` in `~/.config/violin/config.toml` or via `VIOLIN_CLIENT_ID`:

```toml
# ~/.config/violin/config.toml
client_id = "your_spotify_client_id"
```

```bash
export VIOLIN_CLIENT_ID="your_spotify_client_id"
```

## Keybindings

### Normal mode

| Key | Action |
|---|---|
| `Ctrl-l` | Enter search input mode |
| `Tab` | Cycle between Search, Library, Queue, and Jam |
| `Shift-Tab` | Cycle backwards between Search, Library, Queue, and Jam |
| `1` / `2` / `3` / `4` | Switch to Search / Library / Queue / Jam |
| `s` | Toggle Search results between Tracks and Albums |
| `j` | Move selection down |
| `k` | Move selection up |
| `Enter` | Play selected track or open selected album / playlist / folder |
| `Ctrl-Enter` | Immediately play selected album or playlist |
| `Esc` | Close detail view or leave current library folder |
| `Space` | Play / pause |
| `l` | Next track (autoplay/radio queue, or album/playlist order) |
| `h` | Previous track (autoplay/radio queue, or album/playlist order) |
| `Left` / `Right` | Seek backward / forward by 5s |
| `+` / `-` | Increase / decrease volume |
| `z` | Toggle shuffle |
| `f` | Like / unlike focused or playing track |
| `c` | Create a new playlist |
| `a` | Add focused or playing track to a playlist |
| `r` | Refresh active Jam session (`[4] Jam` tab) |
| `n` | Start a new Jam session (auto-copies invite link; `[4] Jam` tab) |
| `i` | Join a Jam session by link or token (`[4] Jam` tab) |
| `x` | Leave (or end, if host) active Jam session (`[4] Jam` tab) |
| `q` / `Ctrl-c` | Quit |

> **Note (`[4] Jam`):** `violin` supports 1-key Jam creation (`n`) with automatic invite link copying to the Linux clipboard (`wl-copy` / `xclip` / `xsel`), plus joining (`i`), refreshing (`r`), syncing, and leaving (`x`) active Spotify Jams when on the `[4] Jam` tab.

### Modal & input modes

| Key | Action |
|---|---|
| `Enter` | Submit search, create playlist, add to selected playlist, or join Jam |
| `Esc` | Cancel input / modal and return to Normal mode |
| `j` / `k` / `Up` / `Down` | Move selection in the Add-to-Playlist picker |
| `Backspace` | Delete last character |
| `Ctrl-c` | Quit |

### Custom keybindings

Override any Normal-mode action under `[keybindings]` in `~/.config/violin/config.toml`. Each action accepts a single key string or an array of keys, including `ctrl+`, `alt+`, and `shift+` modifiers:

```toml
# ~/.config/violin/config.toml
[keybindings]
search = "ctrl+f"
move_down = ["j", "down", "pagedown"]
quit = ["q", "ctrl+q"]
```

Configurable action names: `quit`, `search`, `toggle_view`, `previous_view`, `view_search`, `view_albums`, `view_queue`, `view_jam`, `toggle_search_section`, `move_down`, `move_up`, `select`, `play_selected`, `back`, `play_pause`, `next_track`, `previous_track`, `seek_backward`, `seek_forward`, `volume_up`, `volume_down`, `toggle_shuffle`, `toggle_like`, `add_to_playlist`, `create_playlist`, `refresh_jam`, `start_jam`, `join_jam`, `leave_jam`.

Invalid action names, unrecognized keys, empty binding lists, or duplicate key assignments fail startup with an explicit error.

## Disclaimer

`violin` is an unofficial, independent Spotify client and is not affiliated with, endorsed by, or supported by Spotify AB.

## License

MIT — see [LICENSE](LICENSE).
