use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use thiserror::Error;

use crate::app::{Action, InputMode, View};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyBinding {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum KeymapError {
    #[error("unknown keybinding action '{name}'")]
    UnknownAction { name: String },
    #[error("keybinding for '{action}' cannot be empty")]
    EmptyBinding { action: String },
    #[error("invalid key '{key}' for action '{action}': {reason}")]
    InvalidKeySpec {
        action: String,
        key: String,
        reason: String,
    },
    #[error("duplicate key '{key}' bound to both '{first_action}' and '{second_action}'")]
    DuplicateBinding {
        key: String,
        first_action: String,
        second_action: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    normal_bindings: HashMap<KeyBinding, Action>,
}

const DEFAULT_BINDINGS: &[(&str, Action, &[&str])] = &[
    ("quit", Action::Quit, &["q"]),
    ("search", Action::EnterSearchInput, &["ctrl+l"]),
    ("toggle_view", Action::ToggleView, &["tab"]),
    ("previous_view", Action::PreviousView, &["shift+tab"]),
    ("view_search", Action::SwitchView(View::Search), &["1"]),
    ("view_albums", Action::SwitchView(View::MyAlbums), &["2"]),
    ("view_queue", Action::SwitchView(View::Queue), &["3"]),
    ("view_jam", Action::SwitchView(View::Jam), &["4"]),
    ("toggle_search_section", Action::ToggleSearchSection, &["s"]),
    ("move_down", Action::MoveDown, &["j"]),
    ("move_up", Action::MoveUp, &["k"]),
    ("select", Action::Select, &["enter"]),
    ("play_selected", Action::PlaySelected, &["shift+enter"]),
    ("back", Action::Back, &["esc"]),
    ("play_pause", Action::TogglePlayPause, &["space"]),
    ("next_track", Action::NextTrack, &["l"]),
    ("previous_track", Action::PreviousTrack, &["h"]),
    ("seek_backward", Action::SeekBackward, &["left"]),
    ("seek_forward", Action::SeekForward, &["right"]),
    ("volume_up", Action::VolumeUp, &["up"]),
    ("volume_down", Action::VolumeDown, &["down"]),
    ("toggle_shuffle", Action::ToggleShuffle, &["z"]),
    ("toggle_like", Action::ToggleLike, &["f"]),
    ("add_to_playlist", Action::OpenAddToPlaylist, &["a"]),
    ("create_playlist", Action::OpenCreatePlaylist, &["c"]),
    ("remove", Action::RequestRemove, &["d"]),
    ("refresh_jam", Action::RefreshJam, &["r"]),
    ("start_jam", Action::StartJam, &["n"]),
    ("join_jam", Action::OpenJamJoin, &["i"]),
    ("leave_jam", Action::LeaveJam, &["x"]),
];

/// Parses a key specification string (e.g. `"ctrl+f"`, `"space"`, `"j"`) into a normalized `KeyBinding`.
pub fn parse_key_spec(spec: &str) -> Result<KeyBinding, String> {
    if spec.is_empty() {
        return Err("key spec cannot be empty".to_string());
    }
    if spec == " " {
        return Ok(KeyBinding {
            code: KeyCode::Char(' '),
            modifiers: KeyModifiers::NONE,
        });
    }

    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Err("key spec cannot be whitespace".to_string());
    }

    let mut rest = trimmed;
    let mut modifiers = KeyModifiers::NONE;
    loop {
        if rest == "+" {
            break;
        }
        let Some((prefix, after)) = rest.split_once('+') else {
            break;
        };
        if after.is_empty() {
            return Err(format!("missing key after modifier '{prefix}'"));
        }
        match prefix.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => modifiers |= KeyModifiers::CONTROL,
            "alt" => modifiers |= KeyModifiers::ALT,
            "shift" => modifiers |= KeyModifiers::SHIFT,
            _ => return Err(format!("unknown modifier '{prefix}'")),
        }
        rest = after;
    }

    let code = parse_key_code(rest)?;
    if matches!(code, KeyCode::Char(_)) {
        // Strip SHIFT so shifted punctuation and uppercase chars match consistently across terminals.
        modifiers.remove(KeyModifiers::SHIFT);
    }
    let (code, modifiers) = if code == KeyCode::Tab && modifiers == KeyModifiers::SHIFT {
        (KeyCode::BackTab, KeyModifiers::NONE)
    } else if code == KeyCode::BackTab {
        let mut normalized = modifiers;
        normalized.remove(KeyModifiers::SHIFT);
        (code, normalized)
    } else {
        (code, modifiers)
    };

    Ok(KeyBinding { code, modifiers })
}

fn parse_key_code(key: &str) -> Result<KeyCode, String> {
    let lower = key.to_ascii_lowercase();
    let named = match lower.as_str() {
        "space" => Some(KeyCode::Char(' ')),
        "enter" => Some(KeyCode::Enter),
        "esc" | "escape" => Some(KeyCode::Esc),
        "tab" => Some(KeyCode::Tab),
        "backtab" => Some(KeyCode::BackTab),
        "backspace" => Some(KeyCode::Backspace),
        "up" => Some(KeyCode::Up),
        "down" => Some(KeyCode::Down),
        "left" => Some(KeyCode::Left),
        "right" => Some(KeyCode::Right),
        "home" => Some(KeyCode::Home),
        "end" => Some(KeyCode::End),
        "pageup" => Some(KeyCode::PageUp),
        "pagedown" => Some(KeyCode::PageDown),
        "delete" => Some(KeyCode::Delete),
        _ => None,
    };
    if let Some(code) = named {
        return Ok(code);
    }

    if let Some(num_str) = lower.strip_prefix('f')
        && let Ok(n) = num_str.parse::<u8>()
        && (1..=12).contains(&n)
    {
        return Ok(KeyCode::F(n));
    }

    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Ok(KeyCode::Char(c));
    }

    Err(format!("unrecognized key '{key}'"))
}

impl Default for Keymap {
    fn default() -> Self {
        Self::from_overrides(&HashMap::new()).expect("default keybindings must be valid")
    }
}

type ActionBindingEntry = (Action, Vec<(String, KeyBinding)>);

impl Keymap {
    /// Builds a `Keymap` by applying user-configured action overrides on top of defaults.
    pub fn from_overrides(overrides: &HashMap<String, Vec<String>>) -> Result<Self, KeymapError> {
        let mut action_bindings: HashMap<&str, ActionBindingEntry> =
            HashMap::with_capacity(DEFAULT_BINDINGS.len());

        for &(name, ref action, default_keys) in DEFAULT_BINDINGS {
            let parsed = default_keys
                .iter()
                .map(|&k| {
                    let binding = parse_key_spec(k).expect("default key spec must be valid");
                    (k.to_string(), binding)
                })
                .collect();
            action_bindings.insert(name, (action.clone(), parsed));
        }

        let mut sorted_overrides: Vec<(&String, &Vec<String>)> = overrides.iter().collect();
        sorted_overrides.sort_by_key(|(name, _)| *name);

        for (action_name, keys) in sorted_overrides {
            let Some((action, _)) = action_bindings.get(action_name.as_str()).cloned() else {
                return Err(KeymapError::UnknownAction {
                    name: action_name.clone(),
                });
            };
            if keys.is_empty() {
                return Err(KeymapError::EmptyBinding {
                    action: action_name.clone(),
                });
            }

            let mut parsed = Vec::with_capacity(keys.len());
            for key_spec in keys {
                let binding =
                    parse_key_spec(key_spec).map_err(|reason| KeymapError::InvalidKeySpec {
                        action: action_name.clone(),
                        key: key_spec.clone(),
                        reason,
                    })?;
                parsed.push((key_spec.clone(), binding));
            }
            action_bindings.insert(action_name.as_str(), (action, parsed));
        }

        let mut normal_bindings = HashMap::new();
        let mut seen: HashMap<KeyBinding, &str> = HashMap::new();

        for &(action_name, _, _) in DEFAULT_BINDINGS {
            let (action, bindings) = &action_bindings[action_name];
            for (key_spec, binding) in bindings {
                if let Some(&first_action) = seen.get(binding) {
                    return Err(KeymapError::DuplicateBinding {
                        key: key_spec.clone(),
                        first_action: first_action.to_string(),
                        second_action: action_name.to_string(),
                    });
                }
                seen.insert(*binding, action_name);
                normal_bindings.insert(*binding, action.clone());
            }
        }

        Ok(Self { normal_bindings })
    }

    /// Translates a terminal `KeyEvent` into an application `Action` for the active `InputMode`.
    pub fn map_key(&self, mode: InputMode, key: KeyEvent) -> Option<Action> {
        if key.kind == KeyEventKind::Release {
            return None;
        }

        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Some(Action::Quit);
        }

        match mode {
            InputMode::SearchInput => map_search_input(key),
            InputMode::CreatePlaylistInput | InputMode::JamJoinInput => map_modal_text_input(key),
            InputMode::AddToPlaylistPicker => map_modal_picker(key),
            InputMode::ConfirmDelete => map_modal_confirm(key),
            InputMode::Normal => {
                let mut modifiers = key.modifiers;
                if matches!(key.code, KeyCode::Char(_) | KeyCode::BackTab) {
                    modifiers.remove(KeyModifiers::SHIFT);
                }
                let code = if key.code == KeyCode::Tab && modifiers == KeyModifiers::SHIFT {
                    modifiers = KeyModifiers::NONE;
                    KeyCode::BackTab
                } else {
                    key.code
                };
                let binding = KeyBinding { code, modifiers };
                self.normal_bindings.get(&binding).cloned()
            }
        }
    }
}

/// Translates a terminal `KeyEvent` into an application `Action` using default keybindings.
pub fn map_key(mode: InputMode, key: KeyEvent) -> Option<Action> {
    Keymap::default().map_key(mode, key)
}

fn map_search_input(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Esc => Some(Action::ExitSearchInput),
        KeyCode::Enter => Some(Action::SubmitSearch),
        KeyCode::Backspace => Some(Action::SearchInputBackspace),
        KeyCode::Char(c)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            Some(Action::SearchInputChar(c))
        }
        _ => None,
    }
}

fn map_modal_text_input(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Esc => Some(Action::ModalCancel),
        KeyCode::Enter => Some(Action::ModalSubmit),
        KeyCode::Backspace => Some(Action::ModalInputBackspace),
        KeyCode::Char(c)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            Some(Action::ModalInputChar(c))
        }
        _ => None,
    }
}

fn map_modal_picker(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Esc => Some(Action::ModalCancel),
        KeyCode::Enter => Some(Action::ModalSubmit),
        KeyCode::Char('j') | KeyCode::Down => Some(Action::ModalMoveDown),
        KeyCode::Char('k') | KeyCode::Up => Some(Action::ModalMoveUp),
        _ => None,
    }
}

fn map_modal_confirm(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Esc => Some(Action::ModalCancel),
        KeyCode::Enter => Some(Action::ModalSubmit),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn press_with(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn normal_mode_maps_navigation_views_and_playback() {
        assert_eq!(
            map_key(InputMode::Normal, press(KeyCode::Char('q'))),
            Some(Action::Quit)
        );
        assert_eq!(map_key(InputMode::Normal, press(KeyCode::Char('/'))), None);
        assert_eq!(
            map_key(
                InputMode::Normal,
                press_with(KeyCode::Char('l'), KeyModifiers::CONTROL)
            ),
            Some(Action::EnterSearchInput)
        );
        assert_eq!(
            map_key(InputMode::Normal, press(KeyCode::Char('z'))),
            Some(Action::ToggleShuffle)
        );
        assert_eq!(
            map_key(InputMode::Normal, press(KeyCode::Tab)),
            Some(Action::ToggleView)
        );
        assert_eq!(
            map_key(InputMode::Normal, press(KeyCode::BackTab)),
            Some(Action::PreviousView)
        );
        assert_eq!(
            map_key(
                InputMode::Normal,
                press_with(KeyCode::BackTab, KeyModifiers::SHIFT)
            ),
            Some(Action::PreviousView)
        );
        assert_eq!(
            map_key(
                InputMode::Normal,
                press_with(KeyCode::Tab, KeyModifiers::SHIFT)
            ),
            Some(Action::PreviousView)
        );
        assert_eq!(
            parse_key_spec("shift+tab"),
            Ok(KeyBinding {
                code: KeyCode::BackTab,
                modifiers: KeyModifiers::NONE,
            })
        );
        assert_eq!(
            parse_key_spec("backtab"),
            Ok(KeyBinding {
                code: KeyCode::BackTab,
                modifiers: KeyModifiers::NONE,
            })
        );
        assert_eq!(
            map_key(InputMode::Normal, press(KeyCode::Char('1'))),
            Some(Action::SwitchView(View::Search))
        );
        assert_eq!(
            map_key(InputMode::Normal, press(KeyCode::Char('2'))),
            Some(Action::SwitchView(View::MyAlbums))
        );
        assert_eq!(
            map_key(InputMode::Normal, press(KeyCode::Char('3'))),
            Some(Action::SwitchView(View::Queue))
        );
        assert_eq!(
            map_key(InputMode::Normal, press(KeyCode::Char('4'))),
            Some(Action::SwitchView(View::Jam))
        );
        assert_eq!(
            map_key(
                InputMode::Normal,
                press_with(KeyCode::Enter, KeyModifiers::SHIFT)
            ),
            Some(Action::PlaySelected)
        );
        assert_eq!(
            map_key(InputMode::Normal, press(KeyCode::Char('f'))),
            Some(Action::ToggleLike)
        );
        assert_eq!(
            map_key(InputMode::Normal, press(KeyCode::Char('a'))),
            Some(Action::OpenAddToPlaylist)
        );
        assert_eq!(
            map_key(InputMode::Normal, press(KeyCode::Char('c'))),
            Some(Action::OpenCreatePlaylist)
        );
        assert_eq!(
            map_key(InputMode::Normal, press(KeyCode::Char('r'))),
            Some(Action::RefreshJam)
        );
        assert_eq!(
            map_key(InputMode::Normal, press(KeyCode::Char('n'))),
            Some(Action::StartJam)
        );
        assert_eq!(
            map_key(InputMode::Normal, press(KeyCode::Char('i'))),
            Some(Action::OpenJamJoin)
        );
        assert_eq!(
            map_key(InputMode::Normal, press(KeyCode::Char('x'))),
            Some(Action::LeaveJam)
        );
        assert_eq!(
            map_key(InputMode::Normal, press(KeyCode::Char('s'))),
            Some(Action::ToggleSearchSection)
        );
        assert_eq!(
            map_key(InputMode::Normal, press(KeyCode::Char('j'))),
            Some(Action::MoveDown)
        );
        assert_eq!(
            map_key(InputMode::Normal, press(KeyCode::Down)),
            Some(Action::VolumeDown)
        );
        assert_eq!(
            map_key(InputMode::Normal, press(KeyCode::Char('k'))),
            Some(Action::MoveUp)
        );
        assert_eq!(
            map_key(InputMode::Normal, press(KeyCode::Up)),
            Some(Action::VolumeUp)
        );
        assert_eq!(
            map_key(InputMode::Normal, press(KeyCode::Enter)),
            Some(Action::Select)
        );
        assert_eq!(
            map_key(InputMode::Normal, press(KeyCode::Esc)),
            Some(Action::Back)
        );
        assert_eq!(map_key(InputMode::Normal, press(KeyCode::Backspace)), None);
        assert_eq!(
            map_key(InputMode::Normal, press(KeyCode::Char(' '))),
            Some(Action::TogglePlayPause)
        );
        assert_eq!(
            map_key(InputMode::Normal, press(KeyCode::Char('l'))),
            Some(Action::NextTrack)
        );
        assert_eq!(
            map_key(InputMode::Normal, press(KeyCode::Char('h'))),
            Some(Action::PreviousTrack)
        );
        assert_eq!(
            map_key(InputMode::Normal, press(KeyCode::Left)),
            Some(Action::SeekBackward)
        );
        assert_eq!(
            map_key(InputMode::Normal, press(KeyCode::Right)),
            Some(Action::SeekForward)
        );
        assert_eq!(
            map_key(
                InputMode::Normal,
                press_with(KeyCode::Char('+'), KeyModifiers::SHIFT)
            ),
            None
        );
        assert_eq!(map_key(InputMode::Normal, press(KeyCode::Char('-'))), None);
        assert_eq!(map_key(InputMode::Normal, press(KeyCode::Char('b'))), None);
        assert_eq!(map_key(InputMode::Normal, press(KeyCode::Char('p'))), None);
        assert_eq!(
            map_key(
                InputMode::Normal,
                press_with(KeyCode::Char('c'), KeyModifiers::CONTROL)
            ),
            Some(Action::Quit)
        );
    }

    #[test]
    fn search_input_mode_maps_typing_controls_and_ignores_ctrl_alt_combos() {
        assert_eq!(
            map_key(InputMode::SearchInput, press(KeyCode::Char('a'))),
            Some(Action::SearchInputChar('a'))
        );
        assert_eq!(
            map_key(InputMode::SearchInput, press(KeyCode::Backspace)),
            Some(Action::SearchInputBackspace)
        );
        assert_eq!(
            map_key(InputMode::SearchInput, press(KeyCode::Enter)),
            Some(Action::SubmitSearch)
        );
        assert_eq!(
            map_key(InputMode::SearchInput, press(KeyCode::Esc)),
            Some(Action::ExitSearchInput)
        );
        assert_eq!(
            map_key(
                InputMode::SearchInput,
                press_with(KeyCode::Char('c'), KeyModifiers::CONTROL)
            ),
            Some(Action::Quit)
        );
        assert_eq!(
            map_key(
                InputMode::SearchInput,
                press_with(KeyCode::Char('a'), KeyModifiers::CONTROL)
            ),
            None
        );
        assert_eq!(
            map_key(
                InputMode::SearchInput,
                press_with(KeyCode::Char('a'), KeyModifiers::ALT)
            ),
            None
        );
    }

    #[test]
    fn ignores_key_release_events_in_both_modes() {
        let mut release = press(KeyCode::Char('q'));
        release.kind = KeyEventKind::Release;
        assert_eq!(map_key(InputMode::Normal, release), None);

        let mut search_release = press(KeyCode::Char('a'));
        search_release.kind = KeyEventKind::Release;
        assert_eq!(map_key(InputMode::SearchInput, search_release), None);
    }

    #[test]
    fn parse_key_spec_handles_named_keys_modifiers_and_punctuation() {
        assert_eq!(
            parse_key_spec(" ").unwrap(),
            KeyBinding {
                code: KeyCode::Char(' '),
                modifiers: KeyModifiers::NONE,
            }
        );
        assert_eq!(
            parse_key_spec("ctrl+f").unwrap(),
            KeyBinding {
                code: KeyCode::Char('f'),
                modifiers: KeyModifiers::CONTROL,
            }
        );
        assert_eq!(
            parse_key_spec("Alt+Enter").unwrap(),
            KeyBinding {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::ALT,
            }
        );
        assert_eq!(
            parse_key_spec("+").unwrap(),
            KeyBinding {
                code: KeyCode::Char('+'),
                modifiers: KeyModifiers::NONE,
            }
        );
        assert_eq!(
            parse_key_spec("ctrl++").unwrap(),
            KeyBinding {
                code: KeyCode::Char('+'),
                modifiers: KeyModifiers::CONTROL,
            }
        );
        assert_eq!(
            parse_key_spec("F5").unwrap(),
            KeyBinding {
                code: KeyCode::F(5),
                modifiers: KeyModifiers::NONE,
            }
        );
        assert_eq!(
            parse_key_spec("shift+N").unwrap(),
            KeyBinding {
                code: KeyCode::Char('N'),
                modifiers: KeyModifiers::NONE,
            }
        );
        assert!(parse_key_spec("").is_err());
        assert!(parse_key_spec("ctrl+").is_err());
        assert!(parse_key_spec("super+a").is_err());
        assert!(parse_key_spec("unknownkey").is_err());
    }

    #[test]
    fn from_overrides_replaces_defaults_and_matches_shifted_chars() {
        let mut overrides = HashMap::new();
        overrides.insert("search".to_string(), vec!["ctrl+f".to_string()]);
        overrides.insert(
            "move_down".to_string(),
            vec!["s".to_string(), "pagedown".to_string()],
        );
        overrides.insert("toggle_search_section".to_string(), vec!["tab".to_string()]);
        overrides.insert("toggle_view".to_string(), vec!["f2".to_string()]);
        overrides.insert("next_track".to_string(), vec![">".to_string()]);

        let keymap = Keymap::from_overrides(&overrides).unwrap();

        assert_eq!(
            keymap.map_key(
                InputMode::Normal,
                press_with(KeyCode::Char('f'), KeyModifiers::CONTROL)
            ),
            Some(Action::EnterSearchInput)
        );
        // Default 'ctrl+l' was replaced for search.
        assert_eq!(
            keymap.map_key(
                InputMode::Normal,
                press_with(KeyCode::Char('l'), KeyModifiers::CONTROL)
            ),
            None
        );
        assert_eq!(
            keymap.map_key(InputMode::Normal, press(KeyCode::Char('s'))),
            Some(Action::MoveDown)
        );
        assert_eq!(
            keymap.map_key(InputMode::Normal, press(KeyCode::PageDown)),
            Some(Action::MoveDown)
        );
        assert_eq!(
            keymap.map_key(
                InputMode::Normal,
                press_with(KeyCode::Char('>'), KeyModifiers::SHIFT)
            ),
            Some(Action::NextTrack)
        );
    }

    #[test]
    fn from_overrides_rejects_unknown_empty_invalid_and_duplicate_bindings() {
        let mut unknown = HashMap::new();
        unknown.insert("nonexistent".to_string(), vec!["b".to_string()]);
        assert_eq!(
            Keymap::from_overrides(&unknown),
            Err(KeymapError::UnknownAction {
                name: "nonexistent".to_string()
            })
        );

        let mut empty = HashMap::new();
        empty.insert("quit".to_string(), Vec::new());
        assert_eq!(
            Keymap::from_overrides(&empty),
            Err(KeymapError::EmptyBinding {
                action: "quit".to_string()
            })
        );

        let mut invalid = HashMap::new();
        invalid.insert("quit".to_string(), vec!["bogus_key".to_string()]);
        assert!(matches!(
            Keymap::from_overrides(&invalid),
            Err(KeymapError::InvalidKeySpec { action, key, .. })
            if action == "quit" && key == "bogus_key"
        ));

        let mut duplicate = HashMap::new();
        duplicate.insert("search".to_string(), vec!["q".to_string()]);
        assert_eq!(
            Keymap::from_overrides(&duplicate),
            Err(KeymapError::DuplicateBinding {
                key: "q".to_string(),
                first_action: "quit".to_string(),
                second_action: "search".to_string(),
            })
        );
    }

    #[test]
    fn modal_input_modes_map_text_picker_and_cancel_keys() {
        for mode in [InputMode::CreatePlaylistInput, InputMode::JamJoinInput] {
            assert_eq!(
                map_key(mode, press(KeyCode::Char('x'))),
                Some(Action::ModalInputChar('x'))
            );
            assert_eq!(
                map_key(mode, press(KeyCode::Backspace)),
                Some(Action::ModalInputBackspace)
            );
            assert_eq!(
                map_key(mode, press(KeyCode::Enter)),
                Some(Action::ModalSubmit)
            );
            assert_eq!(
                map_key(mode, press(KeyCode::Esc)),
                Some(Action::ModalCancel)
            );
        }

        assert_eq!(
            map_key(InputMode::AddToPlaylistPicker, press(KeyCode::Char('j'))),
            Some(Action::ModalMoveDown)
        );
        assert_eq!(
            map_key(InputMode::AddToPlaylistPicker, press(KeyCode::Down)),
            Some(Action::ModalMoveDown)
        );
        assert_eq!(
            map_key(InputMode::AddToPlaylistPicker, press(KeyCode::Char('k'))),
            Some(Action::ModalMoveUp)
        );
        assert_eq!(
            map_key(InputMode::AddToPlaylistPicker, press(KeyCode::Up)),
            Some(Action::ModalMoveUp)
        );
        assert_eq!(
            map_key(InputMode::AddToPlaylistPicker, press(KeyCode::Enter)),
            Some(Action::ModalSubmit)
        );
        assert_eq!(
            map_key(InputMode::AddToPlaylistPicker, press(KeyCode::Esc)),
            Some(Action::ModalCancel)
        );

        assert_eq!(
            map_key(InputMode::Normal, press(KeyCode::Char('d'))),
            Some(Action::RequestRemove)
        );
        assert_eq!(
            map_key(InputMode::ConfirmDelete, press(KeyCode::Enter)),
            Some(Action::ModalSubmit)
        );
        assert_eq!(
            map_key(InputMode::ConfirmDelete, press(KeyCode::Esc)),
            Some(Action::ModalCancel)
        );
        assert_eq!(
            map_key(InputMode::ConfirmDelete, press(KeyCode::Char('d'))),
            None
        );
        assert_eq!(
            map_key(InputMode::ConfirmDelete, press(KeyCode::Backspace)),
            None
        );
    }
}
