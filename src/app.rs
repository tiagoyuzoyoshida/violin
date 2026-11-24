use std::collections::HashSet;
use std::time::Duration;

use rand::seq::SliceRandom;

use crate::domain::{
    Album, CoverArt, JamSession, LibraryItem, PlaybackQueue, PlaybackStatus, SearchResults, Track,
    TrackId,
};
use crate::spotify::{LIKED_SONGS_ID, PLAYLIST_ID_PREFIX};

const PREVIOUS_TRACK_RESTART_THRESHOLD: Duration = Duration::from_secs(3);
const POSITION_SYNC_DRIFT_THRESHOLD: Duration = Duration::from_millis(1500);
const DEFAULT_VOLUME: u8 = 80;
const VOLUME_STEP: u8 = 5;
const SEEK_STEP: Duration = Duration::from_secs(5);
/// Upcoming tracks shown (and kept filled) outside a collection: now playing + 19 = 20 rows, matching `spotify::SEARCH_LIMIT`.
const QUEUE_UPCOMING_LIMIT: usize = 19;
const QUEUE_HISTORY_LIMIT: usize = 100;

/// Where the local playback queue came from; decides display length, shuffle, and sync handling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueContext {
    /// No violin-started queue (startup, external/Jam sync).
    None,
    /// An album, playlist, or Liked Songs; `tracks` is the original order.
    Collection { label: String, tracks: Vec<Track> },
    /// Radio seeded from a Search track.
    Radio,
}

fn collection_label(album: &Album) -> String {
    let id = album.id.as_str();
    if id == LIKED_SONGS_ID {
        "Liked Songs".to_string()
    } else if id.starts_with(PLAYLIST_ID_PREFIX) {
        format!("Playlist: {}", album.name)
    } else {
        format!("Album: {}", album.name)
    }
}

/// Top-level navigation views in the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Search,
    MyAlbums,
    Queue,
    Jam,
}

/// Active result list within the search view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchSection {
    Tracks,
    Albums,
}

/// Keyboard input handling mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    SearchInput,
    CreatePlaylistInput,
    AddToPlaylistPicker,
    JamJoinInput,
    ConfirmDelete,
}

/// Target awaiting confirmation in `InputMode::ConfirmDelete`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingDelete {
    Playlist {
        playlist_id: String,
        playlist_name: String,
    },
    TrackFromPlaylist {
        playlist_id: String,
        playlist_name: String,
        track_id: String,
        track_title: String,
    },
}

/// Track listing state for a drilled-down album.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumDetail {
    pub origin_view: View,
    pub album: Album,
    pub tracks: Vec<Track>,
    pub selected_track: usize,
}

/// Active track and playback progress shown in the player bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NowPlaying {
    pub track: Track,
    pub status: PlaybackStatus,
    pub position: Duration,
}

/// User-triggered intentions produced by input mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Quit,
    SwitchView(View),
    ToggleView,
    PreviousView,
    ToggleFocus,
    ToggleRetroArt,
    ToggleSearchSection,
    EnterSearchInput,
    ExitSearchInput,
    SearchInputChar(char),
    SearchInputBackspace,
    SubmitSearch,
    MoveUp,
    MoveDown,
    Select,
    PlaySelected,
    Back,
    TogglePlayPause,
    NextTrack,
    PreviousTrack,
    SeekBackward,
    SeekForward,
    VolumeUp,
    VolumeDown,
    ToggleShuffle,
    ToggleLike,
    OpenCreatePlaylist,
    OpenAddToPlaylist,
    RequestRemove,
    RefreshJam,
    StartJam,
    OpenJamJoin,
    LeaveJam,
    ModalCancel,
    ModalSubmit,
    ModalInputChar(char),
    ModalInputBackspace,
    ModalMoveUp,
    ModalMoveDown,
}

/// Asynchronous notifications from backend, timer, or playback adapters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppEvent {
    SearchCompleted {
        query: String,
        results: SearchResults,
    },
    SavedAlbumsLoaded {
        items: Vec<LibraryItem>,
        liked_track_ids: HashSet<String>,
    },
    AlbumTracksLoaded {
        album: Album,
        tracks: Vec<Track>,
        auto_play: bool,
    },
    PlaybackTick(Duration),
    PlaybackPositionUpdated(Duration),
    TrackSynced {
        track: Track,
        position: Duration,
        status: PlaybackStatus,
    },
    TrackEnded,
    QueueUpdated(Vec<Track>),
    RadioTracksLoaded(Vec<Track>),
    VolumeChanged(u8),
    TrackLikeFailed {
        track_id: String,
        revert_to_liked: bool,
        message: String,
    },
    PlaylistCreated(Album),
    TrackAddedToPlaylist {
        playlist_name: String,
        track_title: String,
    },
    PlaylistRemoved {
        playlist_id: String,
        playlist_name: String,
    },
    TrackRemovedFromPlaylist {
        playlist_id: String,
        playlist_name: String,
        track_id: String,
        track_title: String,
    },
    JamSessionLoaded(Option<JamSession>),
    JamSessionCreated {
        session: JamSession,
        copied_to_clipboard: bool,
    },
    JamLeft,
    CoverArtLoaded(CoverArt),
    CoverArtFailed {
        track_id: TrackId,
    },
    PlaybackFailed(String),
    Error(String),
}

/// Side-effect requests emitted by `AppState` for the runtime to execute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Search(String),
    FetchSavedAlbums,
    FetchAlbumTracks {
        album: Album,
        auto_play: bool,
    },
    FetchTrackRadio(Track),
    PlayTrack(Track),
    PausePlayback,
    ResumePlayback,
    StopPlayback,
    SkipNext,
    SkipPrevious,
    Seek(Duration),
    SetVolume(u8),
    SetTrackLiked {
        track: Track,
        liked: bool,
    },
    CreatePlaylist(String),
    AddTrackToPlaylist {
        playlist_id: String,
        playlist_name: String,
        track: Track,
    },
    RemovePlaylist {
        playlist_id: String,
        playlist_name: String,
    },
    RemoveTrackFromPlaylist {
        playlist_id: String,
        playlist_name: String,
        track_id: String,
        track_title: String,
    },
    FetchJamSession,
    CreateJamSession,
    JoinJamSession(String),
    LeaveJamSession {
        session_id: String,
        is_owner: bool,
    },
    FetchCoverArt {
        track_id: TrackId,
        cover_url: Option<String>,
    },
}

/// Pure application state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppState {
    pub should_quit: bool,
    pub active_view: View,
    pub input_mode: InputMode,
    pub search_query: String,
    pub search_section: SearchSection,
    pub search_results: SearchResults,
    pub selected_search_track: usize,
    pub selected_search_album: usize,
    pub saved_albums: Vec<LibraryItem>,
    pub library_folder_path: Vec<usize>,
    pub selected_saved_album: usize,
    pub album_detail: Option<AlbumDetail>,
    pub upcoming_queue: Vec<Track>,
    pub selected_queue_track: usize,
    pub queue: PlaybackQueue,
    pub now_playing: Option<NowPlaying>,
    pub queue_context: QueueContext,
    radio_fetch_in_flight: bool,
    pub shuffle: bool,
    pub volume: u8,
    pub liked_track_ids: HashSet<String>,
    pub status_message: Option<String>,
    pub notice_message: Option<String>,
    pub jam_session: Option<JamSession>,
    pub create_playlist_input: String,
    pub add_to_playlist_track: Option<Track>,
    pub picker_folder_path: Vec<usize>,
    pub selected_playlist_picker: usize,
    pub jam_join_input: String,
    pub pending_delete: Option<PendingDelete>,
    pub focus_mode: bool,
    pub retro_art: bool,
    pub cover_art: Option<CoverArt>,
    cover_art_requested_track_id: Option<TrackId>,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    /// Creates the initial application state on the Search view in Normal mode.
    pub fn new() -> Self {
        Self {
            should_quit: false,
            active_view: View::Search,
            input_mode: InputMode::Normal,
            search_query: String::new(),
            search_section: SearchSection::Tracks,
            search_results: SearchResults::default(),
            selected_search_track: 0,
            selected_search_album: 0,
            saved_albums: Vec::new(),
            library_folder_path: Vec::new(),
            selected_saved_album: 0,
            album_detail: None,
            upcoming_queue: Vec::new(),
            selected_queue_track: 0,
            queue: PlaybackQueue::default(),
            now_playing: None,
            queue_context: QueueContext::None,
            radio_fetch_in_flight: false,
            shuffle: false,
            volume: DEFAULT_VOLUME,
            liked_track_ids: HashSet::new(),
            status_message: None,
            notice_message: None,
            jam_session: None,
            create_playlist_input: String::new(),
            add_to_playlist_track: None,
            picker_folder_path: Vec::new(),
            selected_playlist_picker: 0,
            jam_join_input: String::new(),
            pending_delete: None,
            focus_mode: false,
            retro_art: false,
            cover_art: None,
            cover_art_requested_track_id: None,
        }
    }

    fn clear_messages(&mut self) {
        self.status_message = None;
        self.notice_message = None;
    }

    /// Applies a user `Action` and returns any resulting `Command`s.
    pub fn handle_action(&mut self, action: Action) -> Vec<Command> {
        match action {
            Action::Quit => self.handle_quit(),
            Action::ToggleFocus => {
                self.focus_mode = !self.focus_mode;
                self.maybe_fetch_cover_art()
            }
            Action::ToggleRetroArt => {
                self.retro_art = !self.retro_art;
                Vec::new()
            }
            Action::SwitchView(view) => {
                self.active_view = view;
                self.input_mode = InputMode::Normal;
                Vec::new()
            }
            Action::ToggleView => {
                self.active_view = match self.active_view {
                    View::Search => View::MyAlbums,
                    View::MyAlbums => View::Queue,
                    View::Queue => View::Jam,
                    View::Jam => View::Search,
                };
                self.input_mode = InputMode::Normal;
                Vec::new()
            }
            Action::PreviousView => {
                self.active_view = match self.active_view {
                    View::Search => View::Jam,
                    View::Jam => View::Queue,
                    View::Queue => View::MyAlbums,
                    View::MyAlbums => View::Search,
                };
                self.input_mode = InputMode::Normal;
                if self.active_view == View::Jam {
                    vec![Command::FetchJamSession]
                } else {
                    Vec::new()
                }
            }
            Action::ToggleSearchSection => {
                if self.active_view == View::Search && self.active_album_detail().is_none() {
                    self.search_section = match self.search_section {
                        SearchSection::Tracks => SearchSection::Albums,
                        SearchSection::Albums => SearchSection::Tracks,
                    };
                }
                Vec::new()
            }
            Action::EnterSearchInput => {
                self.active_view = View::Search;
                self.album_detail = None;
                self.input_mode = InputMode::SearchInput;
                self.clear_messages();
                Vec::new()
            }
            Action::ExitSearchInput => {
                self.input_mode = InputMode::Normal;
                Vec::new()
            }
            Action::SearchInputChar(c) => {
                if self.input_mode == InputMode::SearchInput {
                    self.search_query.push(c);
                }
                Vec::new()
            }
            Action::SearchInputBackspace => {
                if self.input_mode == InputMode::SearchInput {
                    self.search_query.pop();
                }
                Vec::new()
            }
            Action::SubmitSearch => self.handle_submit_search(),
            Action::MoveUp => {
                self.move_selection(-1);
                Vec::new()
            }
            Action::MoveDown => {
                self.move_selection(1);
                Vec::new()
            }
            Action::Select => self.handle_select(false),
            Action::PlaySelected => self.handle_select(true),
            Action::Back => {
                if self.focus_mode {
                    self.focus_mode = false;
                    return Vec::new();
                }
                if self.active_album_detail().is_some() {
                    self.album_detail = None;
                } else if self.active_view == View::MyAlbums
                    && let Some(parent_idx) = self.library_folder_path.pop()
                {
                    self.selected_saved_album = parent_idx;
                }
                Vec::new()
            }
            Action::TogglePlayPause => self.handle_toggle_play_pause(),
            Action::NextTrack => self.handle_next_track(),
            Action::PreviousTrack => self.handle_previous_track(),
            Action::SeekBackward => self.handle_seek(false),
            Action::SeekForward => self.handle_seek(true),
            Action::VolumeUp => {
                self.volume = self.volume.saturating_add(VOLUME_STEP).min(100);
                vec![Command::SetVolume(self.volume)]
            }
            Action::VolumeDown => {
                self.volume = self.volume.saturating_sub(VOLUME_STEP);
                vec![Command::SetVolume(self.volume)]
            }
            Action::ToggleShuffle => self.toggle_shuffle(),
            Action::ToggleLike => self.handle_toggle_like(),
            Action::OpenCreatePlaylist => {
                self.create_playlist_input.clear();
                self.input_mode = InputMode::CreatePlaylistInput;
                self.clear_messages();
                Vec::new()
            }
            Action::OpenAddToPlaylist => self.handle_open_add_to_playlist(),
            Action::RequestRemove => self.handle_request_remove(),
            Action::RefreshJam => {
                if self.active_view != View::Jam || self.active_album_detail().is_some() {
                    return Vec::new();
                }
                self.clear_messages();
                vec![Command::FetchJamSession]
            }
            Action::StartJam => {
                if self.active_view != View::Jam || self.active_album_detail().is_some() {
                    return Vec::new();
                }
                self.input_mode = InputMode::Normal;
                self.clear_messages();
                vec![Command::CreateJamSession]
            }
            Action::OpenJamJoin => {
                if self.active_view != View::Jam || self.active_album_detail().is_some() {
                    return Vec::new();
                }
                self.jam_join_input.clear();
                self.input_mode = InputMode::JamJoinInput;
                self.clear_messages();
                Vec::new()
            }
            Action::LeaveJam => {
                if self.active_view != View::Jam || self.active_album_detail().is_some() {
                    return Vec::new();
                }
                self.clear_messages();
                if let Some(jam) = &self.jam_session {
                    vec![Command::LeaveJamSession {
                        session_id: jam.session_id.clone(),
                        is_owner: jam.is_owner,
                    }]
                } else {
                    Vec::new()
                }
            }
            Action::ModalCancel => {
                if self.input_mode == InputMode::AddToPlaylistPicker
                    && let Some(parent) = self.picker_folder_path.pop()
                {
                    self.selected_playlist_picker = parent;
                    return Vec::new();
                }
                self.input_mode = InputMode::Normal;
                self.add_to_playlist_track = None;
                self.picker_folder_path.clear();
                self.pending_delete = None;
                Vec::new()
            }
            Action::ModalSubmit => self.handle_modal_submit(),
            Action::ModalInputChar(c) => {
                match self.input_mode {
                    InputMode::CreatePlaylistInput => self.create_playlist_input.push(c),
                    InputMode::JamJoinInput => self.jam_join_input.push(c),
                    _ => {}
                }
                Vec::new()
            }
            Action::ModalInputBackspace => {
                match self.input_mode {
                    InputMode::CreatePlaylistInput => {
                        self.create_playlist_input.pop();
                    }
                    InputMode::JamJoinInput => {
                        self.jam_join_input.pop();
                    }
                    _ => {}
                }
                Vec::new()
            }
            Action::ModalMoveUp => {
                if self.input_mode == InputMode::AddToPlaylistPicker {
                    self.selected_playlist_picker = self.selected_playlist_picker.saturating_sub(1);
                }
                Vec::new()
            }
            Action::ModalMoveDown => {
                if self.input_mode == InputMode::AddToPlaylistPicker {
                    let max_idx = self.current_picker_items().len().saturating_sub(1);
                    self.selected_playlist_picker =
                        self.selected_playlist_picker.saturating_add(1).min(max_idx);
                }
                Vec::new()
            }
        }
    }

    /// Applies an external `AppEvent` and returns any resulting `Command`s.
    pub fn handle_event(&mut self, event: AppEvent) -> Vec<Command> {
        match event {
            AppEvent::SearchCompleted { query: _, results } => {
                self.status_message = None;
                self.search_results = results;
                self.selected_search_track = 0;
                self.selected_search_album = 0;
                Vec::new()
            }
            AppEvent::SavedAlbumsLoaded {
                items,
                liked_track_ids,
            } => {
                self.status_message = None;
                self.saved_albums = items;
                self.liked_track_ids = liked_track_ids;
                self.library_folder_path.clear();
                self.selected_saved_album = 0;
                Vec::new()
            }
            AppEvent::AlbumTracksLoaded {
                album,
                tracks,
                auto_play,
            } => {
                if auto_play {
                    if tracks.is_empty() {
                        self.status_message =
                            Some(format!("No playable tracks in \"{}\"", album.name));
                        return Vec::new();
                    }
                    self.album_detail = None;
                    self.active_view = View::Queue;
                    self.status_message = None;
                    return self.start_collection(&album, tracks, 0);
                }
                self.album_detail = Some(AlbumDetail {
                    origin_view: self.active_view,
                    album,
                    tracks,
                    selected_track: 0,
                });
                self.status_message = None;
                Vec::new()
            }
            AppEvent::PlaybackTick(delta) => {
                if let Some(np) = self.now_playing.as_mut()
                    && np.status == PlaybackStatus::Playing
                {
                    np.position = np.position.saturating_add(delta).min(np.track.duration);
                }
                Vec::new()
            }
            AppEvent::PlaybackPositionUpdated(pos) => {
                if let Some(np) = self.now_playing.as_mut() {
                    let clamped = pos.min(np.track.duration);
                    let should_apply = np.status != PlaybackStatus::Playing
                        || np.position.is_zero()
                        || clamped.is_zero()
                        || clamped.abs_diff(np.position) >= POSITION_SYNC_DRIFT_THRESHOLD;
                    if should_apply {
                        np.position = clamped;
                    }
                }
                Vec::new()
            }
            AppEvent::TrackSynced {
                track,
                position,
                status,
            } => {
                let clamped = position.min(track.duration);
                if let Some(idx) = self.queue.tracks().iter().position(|t| t.id == track.id) {
                    let tracks = self.queue.tracks().to_vec();
                    self.queue.set_tracks(tracks, idx);
                    self.sync_upcoming_from_queue();
                } else if self.queue_context != QueueContext::None && self.jam_session.is_none() {
                    return self.handle_foreign_track_sync(&track, clamped, status);
                } else {
                    let had_local_queue = self.queue.len() > 1;
                    self.queue.set_tracks(vec![track.clone()], 0);
                    self.queue_context = QueueContext::None;
                    self.shuffle = false;
                    if had_local_queue {
                        self.upcoming_queue.clear();
                        self.selected_queue_track = 0;
                        self.radio_fetch_in_flight = false;
                    }
                }
                self.clear_cover_art_if_different(&track.id);
                self.now_playing = Some(NowPlaying {
                    track,
                    status,
                    position: clamped,
                });
                self.status_message = None;
                self.maybe_fetch_cover_art()
            }
            AppEvent::TrackEnded => self.advance_after_track_end(),
            AppEvent::QueueUpdated(mut tracks) => {
                if !tracks.is_empty()
                    && self.queue_context == QueueContext::None
                    && self.queue.len() <= 1
                {
                    tracks.truncate(QUEUE_UPCOMING_LIMIT);
                    self.upcoming_queue = tracks;
                    let display_len = self.queue_display_tracks().len();
                    self.selected_queue_track =
                        self.selected_queue_track.min(display_len.saturating_sub(1));
                }
                Vec::new()
            }
            AppEvent::RadioTracksLoaded(tracks) => {
                self.radio_fetch_in_flight = false;
                if self.queue_context != QueueContext::Radio || tracks.is_empty() {
                    return Vec::new();
                }
                let mut full = self.queue.tracks().to_vec();
                if full.is_empty()
                    && let Some(np) = &self.now_playing
                {
                    full.push(np.track.clone());
                }
                let mut seen: HashSet<String> =
                    full.iter().map(|t| t.id.as_str().to_string()).collect();
                for t in tracks {
                    if seen.insert(t.id.as_str().to_string()) {
                        full.push(t);
                    }
                }
                let cur = self.queue.current_index().unwrap_or(0);
                self.queue.set_tracks(full, cur);
                let prev_selected = self.selected_queue_track;
                self.sync_upcoming_from_queue();
                let display_len = self.queue_display_tracks().len();
                self.selected_queue_track = prev_selected.min(display_len.saturating_sub(1));
                Vec::new()
            }
            AppEvent::VolumeChanged(volume) => {
                self.volume = volume.min(100);
                Vec::new()
            }
            AppEvent::TrackLikeFailed {
                track_id,
                revert_to_liked,
                message,
            } => {
                if revert_to_liked {
                    self.liked_track_ids.insert(track_id);
                } else {
                    self.liked_track_ids.remove(&track_id);
                }
                self.status_message = Some(message);
                Vec::new()
            }
            AppEvent::PlaylistCreated(album) => {
                let insert_idx = usize::from(matches!(
                    self.saved_albums.first(),
                    Some(LibraryItem::Collection(first)) if first.id.as_str() == LIKED_SONGS_ID
                ));
                self.saved_albums
                    .insert(insert_idx, LibraryItem::Collection(album.clone()));
                self.notice_message = Some(format!("Created playlist \"{}\"", album.name));
                Vec::new()
            }
            AppEvent::TrackAddedToPlaylist {
                playlist_name,
                track_title,
            } => {
                self.notice_message =
                    Some(format!("Added \"{track_title}\" to \"{playlist_name}\""));
                Vec::new()
            }
            AppEvent::PlaylistRemoved {
                playlist_id,
                playlist_name,
            } => {
                let full_id = format!("{PLAYLIST_ID_PREFIX}{playlist_id}");
                remove_playlist_from_tree(&mut self.saved_albums, &full_id);
                let max_idx = self.current_library_items().len().saturating_sub(1);
                self.selected_saved_album = self.selected_saved_album.min(max_idx);
                self.notice_message = Some(format!("Removed playlist \"{playlist_name}\""));
                Vec::new()
            }
            AppEvent::TrackRemovedFromPlaylist {
                playlist_id,
                playlist_name,
                track_id,
                track_title,
            } => {
                let full_id = format!("{PLAYLIST_ID_PREFIX}{playlist_id}");
                if let Some(detail) = &mut self.album_detail
                    && detail.album.id.as_str() == full_id
                {
                    if let Some(pos) = detail
                        .tracks
                        .iter()
                        .position(|track| track.id.as_str() == track_id)
                    {
                        detail.tracks.remove(pos);
                    }
                    let max_idx = detail.tracks.len().saturating_sub(1);
                    detail.selected_track = detail.selected_track.min(max_idx);
                }
                self.notice_message = Some(format!(
                    "Removed \"{track_title}\" from \"{playlist_name}\""
                ));
                Vec::new()
            }
            AppEvent::JamSessionLoaded(session) => {
                self.jam_session = session;
                self.status_message = None;
                Vec::new()
            }
            AppEvent::JamSessionCreated {
                session,
                copied_to_clipboard,
            } => {
                self.jam_session = Some(session);
                self.status_message = None;
                self.notice_message = Some(if copied_to_clipboard {
                    "Jam started — invite link copied to clipboard".to_string()
                } else {
                    "Jam started".to_string()
                });
                Vec::new()
            }
            AppEvent::JamLeft => {
                self.jam_session = None;
                self.notice_message = Some("Left Jam session".to_string());
                Vec::new()
            }
            AppEvent::CoverArtLoaded(art) => {
                if self.cover_art_requested_track_id.as_ref() == Some(&art.track_id) {
                    self.cover_art_requested_track_id = None;
                }
                if self
                    .now_playing
                    .as_ref()
                    .is_some_and(|np| np.track.id == art.track_id)
                {
                    self.cover_art = Some(art);
                }
                Vec::new()
            }
            AppEvent::CoverArtFailed { track_id } => {
                if self.cover_art_requested_track_id.as_ref() == Some(&track_id) {
                    self.cover_art_requested_track_id = None;
                }
                Vec::new()
            }
            AppEvent::PlaybackFailed(message) => {
                if let Some(np) = self.now_playing.as_mut()
                    && np.status == PlaybackStatus::Playing
                {
                    np.status = PlaybackStatus::Paused;
                }
                self.status_message = Some(message);
                Vec::new()
            }
            AppEvent::Error(message) => {
                self.status_message = Some(message);
                Vec::new()
            }
        }
    }

    fn handle_quit(&mut self) -> Vec<Command> {
        self.should_quit = true;
        if let Some(np) = &self.now_playing
            && np.status != PlaybackStatus::Stopped
        {
            return vec![Command::StopPlayback];
        }
        Vec::new()
    }

    fn handle_submit_search(&mut self) -> Vec<Command> {
        self.input_mode = InputMode::Normal;
        let query = self.search_query.trim().to_string();
        if query.is_empty() {
            return Vec::new();
        }
        self.clear_messages();
        vec![Command::Search(query)]
    }

    /// Returns the currently playing track (if any), then `upcoming_queue` for `[3] Queue`.
    /// Played history stays in `queue` for `PreviousTrack` but is not shown.
    pub fn queue_display_tracks(&self) -> Vec<Track> {
        let shown = match self.queue_context {
            QueueContext::Collection { .. } => self.upcoming_queue.len(),
            QueueContext::None | QueueContext::Radio => {
                self.upcoming_queue.len().min(QUEUE_UPCOMING_LIMIT)
            }
        };
        let mut tracks = Vec::with_capacity(1 + shown);
        if let Some(np) = &self.now_playing {
            tracks.push(np.track.clone());
        }
        tracks.extend(self.upcoming_queue[..shown].iter().cloned());
        tracks
    }

    /// Returns the drilled-down album detail if it belongs to `active_view`.
    pub fn active_album_detail(&self) -> Option<&AlbumDetail> {
        self.album_detail
            .as_ref()
            .filter(|detail| detail.origin_view == self.active_view)
    }

    fn active_album_detail_mut(&mut self) -> Option<&mut AlbumDetail> {
        let active = self.active_view;
        self.album_detail
            .as_mut()
            .filter(|detail| detail.origin_view == active)
    }

    /// Returns the currently focused track across views, falling back to `now_playing`.
    pub fn focused_track(&self) -> Option<Track> {
        if let Some(detail) = self.active_album_detail() {
            return detail.tracks.get(detail.selected_track).cloned();
        }
        if self.active_view == View::Search && self.search_section == SearchSection::Tracks {
            return self
                .search_results
                .tracks
                .get(self.selected_search_track)
                .cloned();
        }
        if self.active_view == View::Queue {
            return self
                .queue_display_tracks()
                .get(self.selected_queue_track)
                .cloned()
                .or_else(|| self.now_playing.as_ref().map(|np| np.track.clone()));
        }
        self.now_playing.as_ref().map(|np| np.track.clone())
    }

    /// Recursively collects all user playlists from `saved_albums` (including inside folders).
    pub fn user_playlists(&self) -> Vec<Album> {
        fn collect(items: &[LibraryItem], out: &mut Vec<Album>) {
            for item in items {
                match item {
                    LibraryItem::Collection(album)
                        if album.id.as_str().starts_with(PLAYLIST_ID_PREFIX) =>
                    {
                        out.push(album.clone());
                    }
                    LibraryItem::Collection(_) => {}
                    LibraryItem::Folder { items, .. } => collect(items, out),
                }
            }
        }
        let mut playlists = Vec::new();
        collect(&self.saved_albums, &mut playlists);
        playlists
    }

    /// Returns the filtered playlist/folder items at `picker_folder_path` for the Add-to-Playlist modal.
    pub fn current_picker_items(&self) -> Vec<LibraryItem> {
        let mut current = filter_playlist_tree(&self.saved_albums);
        for &idx in &self.picker_folder_path {
            if let Some(LibraryItem::Folder { items, .. }) = current.get(idx).cloned() {
                current = items;
            } else {
                break;
            }
        }
        current
    }

    /// Returns the folder names along `picker_folder_path` for the Add-to-Playlist modal breadcrumb.
    pub fn current_picker_breadcrumb(&self) -> Vec<String> {
        let mut names = Vec::new();
        let mut current = filter_playlist_tree(&self.saved_albums);
        for &idx in &self.picker_folder_path {
            if let Some(LibraryItem::Folder { name, items }) = current.get(idx).cloned() {
                names.push(name);
                current = items;
            } else {
                break;
            }
        }
        names
    }

    fn handle_toggle_like(&mut self) -> Vec<Command> {
        let Some(track) = self.focused_track() else {
            return Vec::new();
        };
        let track_id = track.id.as_str().to_string();
        let liked = if self.liked_track_ids.contains(&track_id) {
            self.liked_track_ids.remove(&track_id);
            false
        } else {
            self.liked_track_ids.insert(track_id);
            true
        };
        vec![Command::SetTrackLiked { track, liked }]
    }

    fn handle_open_add_to_playlist(&mut self) -> Vec<Command> {
        self.clear_messages();
        let Some(track) = self.focused_track() else {
            self.status_message = Some("No track selected".to_string());
            return Vec::new();
        };
        self.picker_folder_path.clear();
        self.selected_playlist_picker = 0;
        if self.current_picker_items().is_empty() {
            self.status_message = Some("No playlists available".to_string());
            return Vec::new();
        }
        self.add_to_playlist_track = Some(track);
        self.input_mode = InputMode::AddToPlaylistPicker;
        Vec::new()
    }

    fn handle_request_remove(&mut self) -> Vec<Command> {
        if let Some(detail) = self.active_album_detail() {
            if let Some(playlist_id) = detail.album.id.as_str().strip_prefix(PLAYLIST_ID_PREFIX)
                && let Some(track) = detail.tracks.get(detail.selected_track)
            {
                let playlist_id = playlist_id.to_string();
                let playlist_name = detail.album.name.clone();
                let track_id = track.id.as_str().to_string();
                let track_title = track.title.clone();
                self.clear_messages();
                self.pending_delete = Some(PendingDelete::TrackFromPlaylist {
                    playlist_id,
                    playlist_name,
                    track_id,
                    track_title,
                });
                self.input_mode = InputMode::ConfirmDelete;
            }
            return Vec::new();
        }

        if self.active_view == View::MyAlbums
            && let Some(LibraryItem::Collection(playlist)) =
                self.current_library_items().get(self.selected_saved_album)
            && let Some(playlist_id) = playlist.id.as_str().strip_prefix(PLAYLIST_ID_PREFIX)
        {
            let playlist_id = playlist_id.to_string();
            let playlist_name = playlist.name.clone();
            self.clear_messages();
            self.pending_delete = Some(PendingDelete::Playlist {
                playlist_id,
                playlist_name,
            });
            self.input_mode = InputMode::ConfirmDelete;
        }
        Vec::new()
    }

    fn handle_modal_submit(&mut self) -> Vec<Command> {
        self.clear_messages();
        let mode = self.input_mode;
        match mode {
            InputMode::CreatePlaylistInput => {
                self.input_mode = InputMode::Normal;
                let trimmed = self.create_playlist_input.trim();
                if trimmed.is_empty() {
                    return Vec::new();
                }
                vec![Command::CreatePlaylist(trimmed.to_string())]
            }
            InputMode::JamJoinInput => {
                self.input_mode = InputMode::Normal;
                let trimmed = self.jam_join_input.trim();
                if trimmed.is_empty() {
                    return Vec::new();
                }
                vec![Command::JoinJamSession(trimmed.to_string())]
            }
            InputMode::AddToPlaylistPicker => {
                let items = self.current_picker_items();
                match items.get(self.selected_playlist_picker).cloned() {
                    Some(LibraryItem::Folder { .. }) => {
                        self.picker_folder_path.push(self.selected_playlist_picker);
                        self.selected_playlist_picker = 0;
                        Vec::new()
                    }
                    Some(LibraryItem::Collection(playlist)) => {
                        self.input_mode = InputMode::Normal;
                        self.picker_folder_path.clear();
                        let Some(track) = self.add_to_playlist_track.take() else {
                            return Vec::new();
                        };
                        let playlist_id = playlist
                            .id
                            .as_str()
                            .strip_prefix(PLAYLIST_ID_PREFIX)
                            .unwrap_or(playlist.id.as_str())
                            .to_string();
                        vec![Command::AddTrackToPlaylist {
                            playlist_id,
                            playlist_name: playlist.name,
                            track,
                        }]
                    }
                    None => {
                        self.input_mode = InputMode::Normal;
                        self.add_to_playlist_track = None;
                        self.picker_folder_path.clear();
                        Vec::new()
                    }
                }
            }
            InputMode::ConfirmDelete => {
                self.input_mode = InputMode::Normal;
                match self.pending_delete.take() {
                    Some(PendingDelete::Playlist {
                        playlist_id,
                        playlist_name,
                    }) => vec![Command::RemovePlaylist {
                        playlist_id,
                        playlist_name,
                    }],
                    Some(PendingDelete::TrackFromPlaylist {
                        playlist_id,
                        playlist_name,
                        track_id,
                        track_title,
                    }) => vec![Command::RemoveTrackFromPlaylist {
                        playlist_id,
                        playlist_name,
                        track_id,
                        track_title,
                    }],
                    None => Vec::new(),
                }
            }
            InputMode::Normal | InputMode::SearchInput => Vec::new(),
        }
    }

    /// Returns the slice of `LibraryItem`s visible at the current `library_folder_path`.
    pub fn current_library_items(&self) -> &[LibraryItem] {
        let mut current = self.saved_albums.as_slice();
        for &idx in &self.library_folder_path {
            if let Some(LibraryItem::Folder { items, .. }) = current.get(idx) {
                current = items.as_slice();
            } else {
                break;
            }
        }
        current
    }

    /// Returns the folder names along `library_folder_path` for breadcrumb display.
    pub fn current_library_breadcrumb(&self) -> Vec<&str> {
        let mut names = Vec::new();
        let mut current = self.saved_albums.as_slice();
        for &idx in &self.library_folder_path {
            if let Some(LibraryItem::Folder { name, items }) = current.get(idx) {
                names.push(name.as_str());
                current = items.as_slice();
            } else {
                break;
            }
        }
        names
    }

    fn move_selection(&mut self, delta: isize) {
        let library_len = self.current_library_items().len();
        let queue_len = self.queue_display_tracks().len();
        let (index, len) = if let Some(detail) = self.active_album_detail_mut() {
            (&mut detail.selected_track, detail.tracks.len())
        } else if self.active_view == View::MyAlbums {
            (&mut self.selected_saved_album, library_len)
        } else if self.active_view == View::Queue {
            (&mut self.selected_queue_track, queue_len)
        } else {
            match self.search_section {
                SearchSection::Tracks => (
                    &mut self.selected_search_track,
                    self.search_results.tracks.len(),
                ),
                SearchSection::Albums => (
                    &mut self.selected_search_album,
                    self.search_results.albums.len(),
                ),
            }
        };

        let max_index = len.saturating_sub(1);
        *index = if delta < 0 {
            index.saturating_sub(delta.unsigned_abs())
        } else {
            index.saturating_add(delta as usize).min(max_index)
        };
    }

    fn handle_select(&mut self, auto_play: bool) -> Vec<Command> {
        self.clear_messages();
        if let Some(detail) = self.active_album_detail() {
            if !detail.tracks.is_empty() {
                let tracks = detail.tracks.clone();
                let album = detail.album.clone();
                let start = if auto_play { 0 } else { detail.selected_track };
                self.active_view = View::Queue;
                return self.start_collection(&album, tracks, start);
            }
            return Vec::new();
        }

        if self.active_view == View::MyAlbums {
            return match self
                .current_library_items()
                .get(self.selected_saved_album)
                .cloned()
            {
                Some(LibraryItem::Folder { .. }) => {
                    self.library_folder_path.push(self.selected_saved_album);
                    self.selected_saved_album = 0;
                    Vec::new()
                }
                Some(LibraryItem::Collection(album)) => {
                    vec![Command::FetchAlbumTracks { album, auto_play }]
                }
                None => Vec::new(),
            };
        }

        if self.active_view == View::Queue {
            let selected = self.selected_queue_track;
            let up_idx = if let Some(np) = &self.now_playing {
                if selected == 0 {
                    return self.start_playing_track(np.track.clone());
                }
                selected - 1
            } else {
                selected
            };

            if let Some(track) = self.upcoming_queue.get(up_idx).cloned() {
                if let Some(cur) = self.queue.current_index() {
                    let target_idx = cur + 1 + up_idx;
                    if self.queue.tracks().get(target_idx) == Some(&track) {
                        let tracks = self.queue.tracks().to_vec();
                        let mut cmds = self.play_queue(tracks, target_idx);
                        self.sync_upcoming_from_queue();
                        self.maybe_refill_radio_queue(&mut cmds);
                        return cmds;
                    }
                }
                let remaining = self.upcoming_queue[up_idx..].to_vec();
                let mut cmds = self.play_queue(remaining, 0);
                self.sync_upcoming_from_queue();
                self.maybe_refill_radio_queue(&mut cmds);
                return cmds;
            }
            return Vec::new();
        }

        if self.active_view == View::Jam {
            return Vec::new();
        }

        match self.search_section {
            SearchSection::Tracks => {
                if let Some(track) = self
                    .search_results
                    .tracks
                    .get(self.selected_search_track)
                    .cloned()
                {
                    self.queue.set_tracks(vec![track.clone()], 0);
                    self.upcoming_queue.clear();
                    self.selected_queue_track = 0;
                    self.active_view = View::Queue;
                    self.queue_context = QueueContext::Radio;
                    self.shuffle = false;
                    self.radio_fetch_in_flight = true;
                    let mut cmds = self.start_playing_track(track.clone());
                    cmds.push(Command::FetchTrackRadio(track));
                    return cmds;
                }
                Vec::new()
            }
            SearchSection::Albums => {
                if let Some(album) = self
                    .search_results
                    .albums
                    .get(self.selected_search_album)
                    .cloned()
                {
                    return vec![Command::FetchAlbumTracks { album, auto_play }];
                }
                Vec::new()
            }
        }
    }

    fn handle_toggle_play_pause(&mut self) -> Vec<Command> {
        let Some(np) = self.now_playing.as_mut() else {
            return Vec::new();
        };
        match np.status {
            PlaybackStatus::Playing => {
                np.status = PlaybackStatus::Paused;
                vec![Command::PausePlayback]
            }
            PlaybackStatus::Paused => {
                np.status = PlaybackStatus::Playing;
                vec![Command::ResumePlayback]
            }
            PlaybackStatus::Stopped => {
                np.status = PlaybackStatus::Playing;
                np.position = Duration::ZERO;
                vec![Command::PlayTrack(np.track.clone())]
            }
        }
    }

    fn handle_next_track(&mut self) -> Vec<Command> {
        if let Some(track) = self.queue.next_track().cloned() {
            self.sync_upcoming_from_queue();
            let mut cmds = self.start_playing_track(track);
            self.maybe_refill_radio_queue(&mut cmds);
            return cmds;
        }
        if self.now_playing.is_some() {
            return vec![Command::SkipNext];
        }
        Vec::new()
    }

    fn advance_after_track_end(&mut self) -> Vec<Command> {
        if let Some(track) = self.queue.next_track().cloned() {
            self.sync_upcoming_from_queue();
            let mut cmds = self.start_playing_track(track);
            self.maybe_refill_radio_queue(&mut cmds);
            return cmds;
        }
        if let Some(np) = self.now_playing.as_mut() {
            np.status = PlaybackStatus::Stopped;
            np.position = Duration::ZERO;
        }
        Vec::new()
    }

    /// Handles Soloist reporting a track outside the local queue (its own autoplay pick) while
    /// the local queue is authoritative: the local queue decides what plays next.
    fn handle_foreign_track_sync(
        &mut self,
        track: &Track,
        position: Duration,
        status: PlaybackStatus,
    ) -> Vec<Command> {
        self.status_message = None;
        if let Some(np) = self.now_playing.as_mut()
            && np.track.title == track.title
            && np.track.artists == track.artists
        {
            np.status = status;
            np.position = position.min(np.track.duration);
            return Vec::new();
        }
        let mut cmds = self.advance_after_track_end();
        if cmds.is_empty() && status == PlaybackStatus::Playing {
            cmds.push(Command::StopPlayback);
        }
        cmds
    }

    fn start_collection(
        &mut self,
        album: &Album,
        tracks: Vec<Track>,
        start_index: usize,
    ) -> Vec<Command> {
        self.queue_context = QueueContext::Collection {
            label: collection_label(album),
            tracks: tracks.clone(),
        };
        self.shuffle = false;
        self.radio_fetch_in_flight = false;
        let cmds = self.play_queue(tracks, start_index);
        self.sync_upcoming_from_queue();
        cmds
    }

    fn toggle_shuffle(&mut self) -> Vec<Command> {
        let QueueContext::Collection {
            tracks: original, ..
        } = &self.queue_context
        else {
            return Vec::new();
        };
        self.shuffle = !self.shuffle;
        if self.shuffle {
            let mut tracks = self.queue.tracks().to_vec();
            let cur = self.queue.current_index().unwrap_or(0);
            if let Some(upcoming) = tracks.get_mut(cur + 1..) {
                upcoming.shuffle(&mut rand::rng());
            }
            self.queue.set_tracks(tracks, cur);
        } else {
            let index = self
                .now_playing
                .as_ref()
                .and_then(|np| original.iter().position(|t| t.id == np.track.id))
                .unwrap_or(0);
            self.queue.set_tracks(original.clone(), index);
        }
        self.sync_upcoming_from_queue();
        Vec::new()
    }

    fn maybe_refill_radio_queue(&mut self, cmds: &mut Vec<Command>) {
        if self.queue_context == QueueContext::Radio
            && !self.radio_fetch_in_flight
            && self.upcoming_queue.len() < QUEUE_UPCOMING_LIMIT
            && let Some(seed) = self
                .queue
                .tracks()
                .last()
                .cloned()
                .or_else(|| self.now_playing.as_ref().map(|np| np.track.clone()))
        {
            self.radio_fetch_in_flight = true;
            cmds.push(Command::FetchTrackRadio(seed));
        }
    }

    fn handle_previous_track(&mut self) -> Vec<Command> {
        if let Some(np) = self.now_playing.as_mut()
            && np.position >= PREVIOUS_TRACK_RESTART_THRESHOLD
        {
            np.position = Duration::ZERO;
            np.status = PlaybackStatus::Playing;
            return vec![Command::PlayTrack(np.track.clone())];
        }
        if let Some(track) = self.queue.previous_track().cloned() {
            self.sync_upcoming_from_queue();
            return self.start_playing_track(track);
        }
        if self.now_playing.is_some() {
            return vec![Command::SkipPrevious];
        }
        Vec::new()
    }

    fn handle_seek(&mut self, forward: bool) -> Vec<Command> {
        let Some(np) = self.now_playing.as_mut() else {
            return Vec::new();
        };
        np.position = if forward {
            np.position.saturating_add(SEEK_STEP).min(np.track.duration)
        } else {
            np.position.saturating_sub(SEEK_STEP)
        };
        vec![Command::Seek(np.position)]
    }

    fn sync_upcoming_from_queue(&mut self) {
        if self.queue_context == QueueContext::Radio
            && let Some(cur) = self.queue.current_index()
            && cur > QUEUE_HISTORY_LIMIT
        {
            let kept = self.queue.tracks()[cur - QUEUE_HISTORY_LIMIT..].to_vec();
            self.queue.set_tracks(kept, QUEUE_HISTORY_LIMIT);
        }
        self.upcoming_queue = match self.queue.current_index() {
            Some(idx) => self.queue.tracks().get(idx + 1..).unwrap_or(&[]).to_vec(),
            None => Vec::new(),
        };
        self.selected_queue_track = 0;
    }

    fn play_queue(&mut self, tracks: Vec<Track>, start_index: usize) -> Vec<Command> {
        if let Some(track) = self.queue.set_tracks(tracks, start_index).cloned() {
            return self.start_playing_track(track);
        }
        Vec::new()
    }

    fn clear_cover_art_if_different(&mut self, track_id: &TrackId) {
        if self
            .cover_art
            .as_ref()
            .is_some_and(|art| &art.track_id != track_id)
        {
            self.cover_art = None;
        }
        if self
            .cover_art_requested_track_id
            .as_ref()
            .is_some_and(|id| id != track_id)
        {
            self.cover_art_requested_track_id = None;
        }
    }

    fn maybe_fetch_cover_art(&mut self) -> Vec<Command> {
        if !self.focus_mode {
            return Vec::new();
        }
        let Some(np) = &self.now_playing else {
            return Vec::new();
        };
        let track_id = &np.track.id;
        if self
            .cover_art
            .as_ref()
            .is_some_and(|art| &art.track_id == track_id)
            || self.cover_art_requested_track_id.as_ref() == Some(track_id)
        {
            return Vec::new();
        }
        self.cover_art_requested_track_id = Some(track_id.clone());
        vec![Command::FetchCoverArt {
            track_id: track_id.clone(),
            cover_url: np.track.cover_url.clone(),
        }]
    }

    fn start_playing_track(&mut self, track: Track) -> Vec<Command> {
        self.clear_cover_art_if_different(&track.id);
        self.now_playing = Some(NowPlaying {
            track: track.clone(),
            status: PlaybackStatus::Playing,
            position: Duration::ZERO,
        });
        let mut cmds = vec![Command::PlayTrack(track)];
        cmds.extend(self.maybe_fetch_cover_art());
        cmds
    }
}

fn remove_playlist_from_tree(items: &mut Vec<LibraryItem>, full_album_id: &str) {
    items.retain_mut(|item| match item {
        LibraryItem::Collection(album) => album.id.as_str() != full_album_id,
        LibraryItem::Folder {
            items: children, ..
        } => {
            remove_playlist_from_tree(children, full_album_id);
            true
        }
    });
}

fn filter_playlist_tree(items: &[LibraryItem]) -> Vec<LibraryItem> {
    items
        .iter()
        .filter_map(|item| match item {
            LibraryItem::Collection(album) if album.id.as_str().starts_with(PLAYLIST_ID_PREFIX) => {
                Some(LibraryItem::Collection(album.clone()))
            }
            LibraryItem::Collection(_) => None,
            LibraryItem::Folder { name, items } => {
                let filtered = filter_playlist_tree(items);
                if filtered.is_empty() {
                    None
                } else {
                    Some(LibraryItem::Folder {
                        name: name.clone(),
                        items: filtered,
                    })
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{AlbumId, TrackId};

    fn make_track(id: &str, title: &str, secs: u64) -> Track {
        Track {
            id: TrackId::new(id).unwrap(),
            title: title.to_string(),
            artists: vec!["Artist".to_string()],
            album_name: "Album".to_string(),
            duration: Duration::from_secs(secs),
            cover_url: None,
        }
    }

    fn make_album(id: &str, name: &str) -> Album {
        Album {
            id: AlbumId::new(id).unwrap(),
            name: name.to_string(),
            artists: vec!["Artist".to_string()],
            release_year: Some(2020),
            total_tracks: 10,
        }
    }

    #[test]
    fn view_switching_and_search_section_toggle() {
        let mut state = AppState::new();
        assert_eq!(state.active_view, View::Search);
        assert_eq!(state.search_section, SearchSection::Tracks);

        state.handle_action(Action::ToggleSearchSection);
        assert_eq!(state.search_section, SearchSection::Albums);

        state.handle_action(Action::ToggleView);
        assert_eq!(state.active_view, View::MyAlbums);

        state.handle_action(Action::ToggleSearchSection);
        assert_eq!(state.search_section, SearchSection::Albums);

        state.handle_action(Action::ToggleView);
        assert_eq!(state.active_view, View::Queue);

        state.handle_action(Action::ToggleView);
        assert_eq!(state.active_view, View::Jam);

        state.handle_action(Action::ToggleView);
        assert_eq!(state.active_view, View::Search);

        state.handle_action(Action::SwitchView(View::Queue));
        assert_eq!(state.active_view, View::Queue);

        state.handle_action(Action::SwitchView(View::Search));
        assert_eq!(state.active_view, View::Search);
    }

    #[test]
    fn search_input_typing_and_submit_empty_vs_non_empty() {
        let mut state = AppState::new();
        state.handle_action(Action::SearchInputChar('x'));
        assert!(state.search_query.is_empty());

        state.handle_action(Action::EnterSearchInput);
        assert_eq!(state.input_mode, InputMode::SearchInput);

        state.handle_action(Action::SearchInputChar(' '));
        state.handle_action(Action::SearchInputChar('a'));
        state.handle_action(Action::SearchInputBackspace);
        assert_eq!(state.search_query, " ");

        let cmds = state.handle_action(Action::SubmitSearch);
        assert!(cmds.is_empty());
        assert_eq!(state.input_mode, InputMode::Normal);
        assert_eq!(state.status_message, None);

        state.handle_event(AppEvent::Error("stale error".to_string()));
        state.handle_action(Action::EnterSearchInput);
        assert_eq!(state.status_message, None);
        state.handle_action(Action::SearchInputChar('b'));
        state.handle_action(Action::SearchInputChar('a'));
        state.handle_action(Action::SearchInputChar('c'));
        state.handle_action(Action::SearchInputChar('h'));
        let cmds = state.handle_action(Action::SubmitSearch);
        assert_eq!(cmds, vec![Command::Search("bach".to_string())]);
        assert_eq!(state.status_message, None);
    }

    #[test]
    fn navigation_clamps_on_empty_and_populated_lists() {
        let mut state = AppState::new();
        state.handle_action(Action::MoveDown);
        state.handle_action(Action::MoveUp);
        assert_eq!(state.selected_search_track, 0);

        state.handle_event(AppEvent::SearchCompleted {
            query: "test".to_string(),
            results: SearchResults {
                tracks: vec![make_track("t1", "One", 100), make_track("t2", "Two", 100)],
                albums: vec![make_album("a1", "Album 1")],
            },
        });

        state.handle_action(Action::MoveDown);
        assert_eq!(state.selected_search_track, 1);
        state.handle_action(Action::MoveDown);
        assert_eq!(state.selected_search_track, 1);
        state.handle_action(Action::MoveUp);
        assert_eq!(state.selected_search_track, 0);
        state.handle_action(Action::MoveUp);
        assert_eq!(state.selected_search_track, 0);
    }

    #[test]
    fn selecting_tracks_albums_and_album_detail_tracks() {
        let mut state = AppState::new();
        let t1 = make_track("t1", "Track 1", 120);
        let t2 = make_track("t2", "Track 2", 150);
        let album = make_album("a1", "Album 1");

        state.handle_event(AppEvent::SearchCompleted {
            query: "q".to_string(),
            results: SearchResults {
                tracks: vec![t1.clone(), t2.clone()],
                albums: vec![album.clone()],
            },
        });

        state.handle_action(Action::MoveDown);
        let cmds = state.handle_action(Action::Select);
        assert_eq!(
            cmds,
            vec![
                Command::PlayTrack(t2.clone()),
                Command::FetchTrackRadio(t2.clone())
            ]
        );
        assert_eq!(state.now_playing.as_ref().unwrap().track, t2);
        assert_eq!(state.active_view, View::Queue);

        state.handle_action(Action::SwitchView(View::Search));
        state.handle_action(Action::ToggleSearchSection);
        let cmds = state.handle_action(Action::Select);
        assert_eq!(
            cmds,
            vec![Command::FetchAlbumTracks {
                album: album.clone(),
                auto_play: false,
            }]
        );

        state.handle_event(AppEvent::AlbumTracksLoaded {
            album: album.clone(),
            tracks: vec![t1.clone()],
            auto_play: false,
        });
        assert!(state.active_album_detail().is_some());
        let cmds = state.handle_action(Action::Select);
        assert_eq!(cmds, vec![Command::PlayTrack(t1)]);
        assert!(state.album_detail.is_some());
        assert!(state.active_album_detail().is_none());
        assert_eq!(state.active_view, View::Queue);

        state.handle_action(Action::Back);
        assert!(state.active_album_detail().is_none());
        assert!(state.album_detail.is_some());

        state.handle_action(Action::SwitchView(View::MyAlbums));
        state.handle_event(AppEvent::SavedAlbumsLoaded {
            items: vec![LibraryItem::Collection(album.clone())],
            liked_track_ids: HashSet::new(),
        });
        let cmds = state.handle_action(Action::Select);
        assert_eq!(
            cmds,
            vec![Command::FetchAlbumTracks {
                album,
                auto_play: false,
            }]
        );
    }

    #[test]
    fn toggle_play_pause_and_stopped_replay() {
        let mut state = AppState::new();
        assert!(state.handle_action(Action::TogglePlayPause).is_empty());

        let t1 = make_track("t1", "One", 100);
        state.handle_event(AppEvent::SearchCompleted {
            query: "q".to_string(),
            results: SearchResults {
                tracks: vec![t1.clone()],
                albums: vec![],
            },
        });
        state.handle_action(Action::Select);

        assert_eq!(
            state.handle_action(Action::TogglePlayPause),
            vec![Command::PausePlayback]
        );
        assert_eq!(
            state.now_playing.as_ref().unwrap().status,
            PlaybackStatus::Paused
        );

        assert_eq!(
            state.handle_action(Action::TogglePlayPause),
            vec![Command::ResumePlayback]
        );
        assert_eq!(
            state.now_playing.as_ref().unwrap().status,
            PlaybackStatus::Playing
        );

        state.handle_event(AppEvent::PlaybackTick(Duration::from_secs(10)));
        state.handle_event(AppEvent::TrackEnded);
        assert_eq!(
            state.now_playing.as_ref().unwrap().status,
            PlaybackStatus::Stopped
        );

        assert_eq!(
            state.handle_action(Action::TogglePlayPause),
            vec![Command::PlayTrack(t1)]
        );
        assert_eq!(
            state.now_playing.as_ref().unwrap().status,
            PlaybackStatus::Playing
        );
        assert_eq!(state.now_playing.as_ref().unwrap().position, Duration::ZERO);
    }

    #[test]
    fn next_and_previous_track_with_three_second_restart_rule() {
        let mut state = AppState::new();
        assert!(state.handle_action(Action::NextTrack).is_empty());
        assert!(state.handle_action(Action::PreviousTrack).is_empty());

        let t1 = make_track("t1", "One", 100);
        let t2 = make_track("t2", "Two", 100);
        state.handle_event(AppEvent::AlbumTracksLoaded {
            album: make_album("a1", "Album One"),
            tracks: vec![t1.clone(), t2.clone()],
            auto_play: false,
        });
        assert_eq!(
            state.handle_action(Action::Select),
            vec![Command::PlayTrack(t1.clone())]
        );

        assert_eq!(
            state.handle_action(Action::NextTrack),
            vec![Command::PlayTrack(t2.clone())]
        );
        assert_eq!(
            state.handle_action(Action::NextTrack),
            vec![Command::SkipNext]
        );

        state.handle_event(AppEvent::PlaybackTick(Duration::from_secs(4)));
        assert_eq!(
            state.handle_action(Action::PreviousTrack),
            vec![Command::PlayTrack(t2.clone())]
        );
        assert_eq!(state.now_playing.as_ref().unwrap().position, Duration::ZERO);

        state.handle_event(AppEvent::PlaybackTick(Duration::from_secs(2)));
        assert_eq!(
            state.handle_action(Action::PreviousTrack),
            vec![Command::PlayTrack(t1)]
        );
        assert_eq!(
            state.handle_action(Action::PreviousTrack),
            vec![Command::SkipPrevious]
        );
    }

    #[test]
    fn search_track_select_clears_local_queue_and_delegates_skip_to_soloist() {
        let mut state = AppState::new();
        let album_track = make_track("a1", "Album Track", 100);
        state.handle_event(AppEvent::AlbumTracksLoaded {
            album: make_album("alb1", "Album"),
            tracks: vec![album_track.clone(), make_track("a2", "Album Two", 100)],
            auto_play: false,
        });
        state.handle_action(Action::Select);
        assert_eq!(state.active_view, View::Queue);
        assert_eq!(state.upcoming_queue.len(), 1);
        state.handle_action(Action::SwitchView(View::Search));
        state.handle_action(Action::Back);

        let s1 = make_track("s1", "Search One", 100);
        let s2 = make_track("s2", "Search Two", 100);
        state.handle_event(AppEvent::SearchCompleted {
            query: "q".to_string(),
            results: SearchResults {
                tracks: vec![s1.clone(), s2],
                albums: vec![],
            },
        });

        assert_eq!(
            state.handle_action(Action::Select),
            vec![Command::PlayTrack(s1.clone()), Command::FetchTrackRadio(s1)]
        );
        assert_eq!(state.active_view, View::Queue);
        assert!(state.upcoming_queue.is_empty());
        assert_eq!(
            state.handle_action(Action::NextTrack),
            vec![Command::SkipNext]
        );
        assert_eq!(
            state.handle_action(Action::PreviousTrack),
            vec![Command::SkipPrevious]
        );
    }

    #[test]
    fn playback_tick_clamps_and_track_ended_advances_or_stops() {
        let mut state = AppState::new();
        let t1 = make_track("t1", "One", 10);
        let t2 = make_track("t2", "Two", 20);
        state.handle_event(AppEvent::AlbumTracksLoaded {
            album: make_album("a1", "Album One"),
            tracks: vec![t1, t2.clone()],
            auto_play: false,
        });
        state.handle_action(Action::Select);

        state.handle_event(AppEvent::PlaybackTick(Duration::from_secs(50)));
        assert_eq!(
            state.now_playing.as_ref().unwrap().position,
            Duration::from_secs(10)
        );

        let cmds = state.handle_event(AppEvent::TrackEnded);
        assert_eq!(cmds, vec![Command::PlayTrack(t2)]);
        assert_eq!(state.now_playing.as_ref().unwrap().position, Duration::ZERO);

        let cmds = state.handle_event(AppEvent::TrackEnded);
        assert!(cmds.is_empty());
        assert_eq!(
            state.now_playing.as_ref().unwrap().status,
            PlaybackStatus::Stopped
        );
    }

    #[test]
    fn quit_stops_active_playback_and_error_sets_status_message() {
        let mut state = AppState::new();
        state.handle_event(AppEvent::Error("network timeout".to_string()));
        assert_eq!(state.status_message.as_deref(), Some("network timeout"));

        let cmds = state.handle_action(Action::Quit);
        assert!(state.should_quit);
        assert!(cmds.is_empty());

        let mut playing_state = AppState::new();
        playing_state.handle_event(AppEvent::SearchCompleted {
            query: "q".to_string(),
            results: SearchResults {
                tracks: vec![make_track("t1", "One", 10)],
                albums: vec![],
            },
        });
        playing_state.handle_action(Action::Select);
        let cmds = playing_state.handle_action(Action::Quit);
        assert!(playing_state.should_quit);
        assert_eq!(cmds, vec![Command::StopPlayback]);
    }

    #[test]
    fn playback_failed_pauses_active_playback_while_error_does_not() {
        let mut state = AppState::new();
        state.handle_event(AppEvent::SearchCompleted {
            query: "q".to_string(),
            results: SearchResults {
                tracks: vec![make_track("t1", "One", 30)],
                albums: vec![],
            },
        });
        state.handle_action(Action::Select);
        assert_eq!(
            state.now_playing.as_ref().unwrap().status,
            PlaybackStatus::Playing
        );

        state.handle_event(AppEvent::Error("search hiccup".to_string()));
        assert_eq!(state.status_message.as_deref(), Some("search hiccup"));
        assert_eq!(
            state.now_playing.as_ref().unwrap().status,
            PlaybackStatus::Playing
        );

        state.handle_event(AppEvent::PlaybackFailed("soloist crashed".to_string()));
        assert_eq!(state.status_message.as_deref(), Some("soloist crashed"));
        assert_eq!(
            state.now_playing.as_ref().unwrap().status,
            PlaybackStatus::Paused
        );

        state.handle_event(AppEvent::PlaybackTick(Duration::from_secs(5)));
        assert_eq!(state.now_playing.as_ref().unwrap().position, Duration::ZERO);
    }

    #[test]
    fn track_synced_updates_now_playing_clamps_position_and_clears_status() {
        let mut state = AppState::new();
        state.handle_event(AppEvent::Error("previous error".to_string()));

        let synced_track = make_track("t2", "Synced Track", 180);
        let cmds = state.handle_event(AppEvent::TrackSynced {
            track: synced_track.clone(),
            position: Duration::from_secs(240),
            status: PlaybackStatus::Paused,
        });

        assert!(cmds.is_empty());
        assert_eq!(state.status_message, None);
        let np = state.now_playing.as_ref().unwrap();
        assert_eq!(np.track, synced_track);
        assert_eq!(np.status, PlaybackStatus::Paused);
        assert_eq!(np.position, Duration::from_secs(180));
    }

    #[test]
    fn queue_updated_navigation_clamping_and_selection_play() {
        let mut state = AppState::new();
        let t1 = make_track("q1", "Queue One", 100);
        let t2 = make_track("q2", "Queue Two", 120);
        let t3 = make_track("q3", "Queue Three", 140);

        state.handle_action(Action::SwitchView(View::Queue));
        state.handle_event(AppEvent::QueueUpdated(vec![
            t1.clone(),
            t2.clone(),
            t3.clone(),
        ]));
        assert_eq!(state.upcoming_queue.len(), 3);
        assert_eq!(state.selected_queue_track, 0);

        state.handle_action(Action::MoveDown);
        state.handle_action(Action::MoveDown);
        assert_eq!(state.selected_queue_track, 2);

        // Shrinking queue clamps selection index.
        state.handle_event(AppEvent::QueueUpdated(vec![t1.clone(), t2.clone()]));
        assert_eq!(state.selected_queue_track, 1);

        let cmds = state.handle_action(Action::Select);
        assert_eq!(cmds, vec![Command::PlayTrack(t2.clone())]);
        assert_eq!(state.now_playing.as_ref().unwrap().track, t2);
        assert_eq!(
            state.handle_action(Action::NextTrack),
            vec![Command::SkipNext]
        );
    }

    #[test]
    fn radio_tracks_loaded_populates_queue_and_syncs_upcoming_across_navigation() {
        let mut state = AppState::new();
        let seed = make_track("seed", "Seed Track", 120);
        let r1 = make_track("r1", "Radio One", 130);
        let r2 = make_track("r2", "Radio Two", 140);

        state.handle_event(AppEvent::SearchCompleted {
            query: "seed".to_string(),
            results: SearchResults {
                tracks: vec![seed.clone()],
                albums: vec![],
            },
        });
        let cmds = state.handle_action(Action::Select);
        assert_eq!(
            cmds,
            vec![
                Command::PlayTrack(seed.clone()),
                Command::FetchTrackRadio(seed.clone()),
            ]
        );
        assert_eq!(state.active_view, View::Queue);
        assert!(state.upcoming_queue.is_empty());

        // Empty QueueUpdated from Soloist must not wipe upcoming_queue once populated.
        state.handle_event(AppEvent::RadioTracksLoaded(vec![r1.clone(), r2.clone()]));
        assert_eq!(state.upcoming_queue, vec![r1.clone(), r2.clone()]);

        state.handle_event(AppEvent::QueueUpdated(Vec::new()));
        assert_eq!(state.upcoming_queue, vec![r1.clone(), r2.clone()]);

        // NextTrack / TrackEnded advance and update upcoming_queue live,
        // triggering a single background refill while below threshold.
        assert_eq!(
            state.handle_action(Action::NextTrack),
            vec![
                Command::PlayTrack(r1.clone()),
                Command::FetchTrackRadio(r2.clone()),
            ]
        );
        assert_eq!(state.now_playing.as_ref().unwrap().track, r1);
        assert_eq!(state.upcoming_queue, vec![r2.clone()]);

        // While refill is in flight, TrackEnded does not emit a duplicate FetchTrackRadio.
        assert_eq!(
            state.handle_event(AppEvent::TrackEnded),
            vec![Command::PlayTrack(r2.clone())]
        );
        assert_eq!(state.now_playing.as_ref().unwrap().track, r2);
        assert!(state.upcoming_queue.is_empty());

        // PreviousTrack retreats and restores upcoming_queue.
        assert_eq!(
            state.handle_action(Action::PreviousTrack),
            vec![Command::PlayTrack(r1)]
        );
        assert_eq!(state.upcoming_queue, vec![r2]);
    }

    #[test]
    fn radio_tracks_loaded_does_not_clobber_active_album_queue() {
        let mut state = AppState::new();
        let t1 = make_track("t1", "Album One", 100);
        let t2 = make_track("t2", "Album Two", 100);
        state.handle_event(AppEvent::AlbumTracksLoaded {
            album: make_album("a1", "Album"),
            tracks: vec![t1, t2.clone()],
            auto_play: false,
        });
        state.handle_action(Action::Select);
        assert_eq!(state.upcoming_queue, vec![t2.clone()]);

        let late_radio = make_track("r1", "Late Radio", 100);
        state.handle_event(AppEvent::RadioTracksLoaded(vec![late_radio]));
        assert_eq!(state.upcoming_queue, vec![t2]);
        assert!(matches!(
            state.queue_context,
            QueueContext::Collection { .. }
        ));
    }

    #[test]
    fn radio_refill_appends_deduplicated_tracks_and_preserves_index_and_selection() {
        let mut state = AppState::new();
        let seed = make_track("seed", "Seed Track", 120);
        let r1 = make_track("r1", "Radio One", 130);
        let r2 = make_track("r2", "Radio Two", 140);
        let r3 = make_track("r3", "Radio Three", 150);

        state.handle_event(AppEvent::SearchCompleted {
            query: "seed".to_string(),
            results: SearchResults {
                tracks: vec![seed.clone()],
                albums: vec![],
            },
        });
        state.handle_action(Action::Select);
        state.handle_event(AppEvent::RadioTracksLoaded(vec![r1.clone(), r2.clone()]));

        // Advance to r1 -> emits FetchTrackRadio(r2).
        let cmds = state.handle_action(Action::NextTrack);
        assert_eq!(
            cmds,
            vec![
                Command::PlayTrack(r1.clone()),
                Command::FetchTrackRadio(r2.clone()),
            ]
        );
        // Move cursor to row 1 (r2, after the now-playing r1) before refill arrives.
        state.selected_queue_track = 1;

        // Refill includes duplicates (seed, r1, r2) plus new track r3.
        state.handle_event(AppEvent::RadioTracksLoaded(vec![
            seed,
            r1.clone(),
            r2.clone(),
            r3.clone(),
        ]));
        assert_eq!(state.now_playing.as_ref().unwrap().track, r1);
        assert_eq!(state.queue.current_index(), Some(1));
        assert_eq!(state.upcoming_queue, vec![r2.clone(), r3.clone()]);
        assert_eq!(state.selected_queue_track, 1);

        // Selecting r2 in Queue advances and triggers another refill seeded by r3.
        let queue_cmds = state.handle_action(Action::Select);
        assert_eq!(
            queue_cmds,
            vec![
                Command::PlayTrack(r2.clone()),
                Command::FetchTrackRadio(r3.clone()),
            ]
        );

        // Failed radio fetch (empty vec) resets in-flight flag so TrackEnded can retry.
        state.handle_event(AppEvent::RadioTracksLoaded(Vec::new()));
        assert_eq!(
            state.handle_event(AppEvent::TrackEnded),
            vec![Command::PlayTrack(r3.clone()), Command::FetchTrackRadio(r3)]
        );
    }

    #[test]
    fn library_nested_folder_navigation_and_back_restore_cursor() {
        let mut state = AppState::new();
        let top_album = make_album("top", "Top Album");
        let outer_album = make_album("outer", "Outer Playlist");
        let inner_album = make_album("inner", "Inner Playlist");

        let library = vec![
            LibraryItem::Collection(top_album),
            LibraryItem::Folder {
                name: "Albums".to_string(),
                items: vec![
                    LibraryItem::Collection(outer_album),
                    LibraryItem::Folder {
                        name: "Jazz".to_string(),
                        items: vec![LibraryItem::Collection(inner_album.clone())],
                    },
                ],
            },
        ];

        state.handle_action(Action::SwitchView(View::MyAlbums));
        state.handle_event(AppEvent::SavedAlbumsLoaded {
            items: library,
            liked_track_ids: HashSet::new(),
        });
        assert_eq!(state.current_library_items().len(), 2);
        assert!(state.current_library_breadcrumb().is_empty());

        // Move to folder at index 1 and enter it.
        state.handle_action(Action::MoveDown);
        assert_eq!(state.selected_saved_album, 1);
        assert!(state.handle_action(Action::Select).is_empty());
        assert_eq!(state.library_folder_path, vec![1]);
        assert_eq!(state.selected_saved_album, 0);
        assert_eq!(state.current_library_breadcrumb(), vec!["Albums"]);
        assert_eq!(state.current_library_items().len(), 2);

        // Move down past end clamps to index 1 ("Jazz" folder), then enter it.
        state.handle_action(Action::MoveDown);
        state.handle_action(Action::MoveDown);
        assert_eq!(state.selected_saved_album, 1);
        assert!(state.handle_action(Action::Select).is_empty());
        assert_eq!(state.library_folder_path, vec![1, 1]);
        assert_eq!(state.selected_saved_album, 0);
        assert_eq!(state.current_library_breadcrumb(), vec!["Albums", "Jazz"]);

        // Select playlist inside nested folder.
        let cmds = state.handle_action(Action::Select);
        assert_eq!(
            cmds,
            vec![Command::FetchAlbumTracks {
                album: inner_album.clone(),
                auto_play: false,
            }]
        );

        // Open album_detail, then press Back: closes detail first without leaving folder.
        state.handle_event(AppEvent::AlbumTracksLoaded {
            album: inner_album,
            tracks: vec![make_track("t1", "Track 1", 120)],
            auto_play: false,
        });
        assert!(state.album_detail.is_some());
        state.handle_action(Action::Back);
        assert!(state.album_detail.is_none());
        assert_eq!(state.library_folder_path, vec![1, 1]);

        // Back pops one folder level at a time and restores cursor.
        state.handle_action(Action::Back);
        assert_eq!(state.library_folder_path, vec![1]);
        assert_eq!(state.selected_saved_album, 1);

        state.handle_action(Action::Back);
        assert!(state.library_folder_path.is_empty());
        assert_eq!(state.selected_saved_album, 1);

        // Back at root is a no-op.
        state.handle_action(Action::Back);
        assert!(state.library_folder_path.is_empty());
        assert_eq!(state.selected_saved_album, 1);
    }

    #[test]
    fn play_selected_auto_plays_album_and_switches_to_queue() {
        let mut state = AppState::new();
        let album = make_album("alb_fast", "Fast Play");
        let t1 = make_track("t1", "First", 120);
        let t2 = make_track("t2", "Second", 150);

        state.handle_event(AppEvent::SearchCompleted {
            query: "fast".to_string(),
            results: SearchResults {
                tracks: vec![],
                albums: vec![album.clone()],
            },
        });
        state.handle_action(Action::ToggleSearchSection);
        let cmds = state.handle_action(Action::PlaySelected);
        assert_eq!(
            cmds,
            vec![Command::FetchAlbumTracks {
                album: album.clone(),
                auto_play: true,
            }]
        );

        let play_cmds = state.handle_event(AppEvent::AlbumTracksLoaded {
            album,
            tracks: vec![t1.clone(), t2.clone()],
            auto_play: true,
        });
        assert_eq!(play_cmds, vec![Command::PlayTrack(t1.clone())]);
        assert!(state.album_detail.is_none());
        assert_eq!(state.active_view, View::Queue);
        assert_eq!(state.now_playing.as_ref().unwrap().track, t1);
        assert_eq!(state.upcoming_queue, vec![t2]);
    }

    #[test]
    fn toggle_like_updates_optimistically_and_reverts_on_failure() {
        let mut state = AppState::new();
        let t1 = make_track("trk_like", "Likeable", 180);
        state.handle_event(AppEvent::SearchCompleted {
            query: "q".to_string(),
            results: SearchResults {
                tracks: vec![t1.clone()],
                albums: vec![],
            },
        });

        let cmds = state.handle_action(Action::ToggleLike);
        assert_eq!(
            cmds,
            vec![Command::SetTrackLiked {
                track: t1.clone(),
                liked: true,
            }]
        );
        assert!(state.liked_track_ids.contains("trk_like"));

        state.handle_event(AppEvent::TrackLikeFailed {
            track_id: "trk_like".to_string(),
            revert_to_liked: false,
            message: "network error".to_string(),
        });
        assert!(!state.liked_track_ids.contains("trk_like"));
        assert_eq!(state.status_message.as_deref(), Some("network error"));

        // Re-like, then unlike.
        state.liked_track_ids.insert("trk_like".to_string());
        let cmds = state.handle_action(Action::ToggleLike);
        assert_eq!(
            cmds,
            vec![Command::SetTrackLiked {
                track: t1,
                liked: false,
            }]
        );
        assert!(!state.liked_track_ids.contains("trk_like"));
    }

    #[test]
    fn create_playlist_and_add_to_playlist_modals_work() {
        let mut state = AppState::new();
        let liked = make_album(LIKED_SONGS_ID, "Liked Songs");
        let existing_pl = make_album("playlist:pl_old", "Old Mix");
        let folder_pl = make_album("playlist:pl_fold", "Folder Mix");
        state.handle_event(AppEvent::SavedAlbumsLoaded {
            items: vec![
                LibraryItem::Collection(liked),
                LibraryItem::Folder {
                    name: "Classical".to_string(),
                    items: vec![LibraryItem::Collection(folder_pl)],
                },
                LibraryItem::Collection(existing_pl),
            ],
            liked_track_ids: HashSet::new(),
        });

        // Create playlist modal.
        state.handle_action(Action::OpenCreatePlaylist);
        assert_eq!(state.input_mode, InputMode::CreatePlaylistInput);
        for ch in "New Mix!".chars() {
            state.handle_action(Action::ModalInputChar(ch));
        }
        state.handle_action(Action::ModalInputBackspace);
        let cmds = state.handle_action(Action::ModalSubmit);
        assert_eq!(state.input_mode, InputMode::Normal);
        assert_eq!(cmds, vec![Command::CreatePlaylist("New Mix".to_string())]);

        let created = make_album("playlist:pl_new", "New Mix");
        state.handle_event(AppEvent::PlaylistCreated(created));
        assert_eq!(
            state.notice_message.as_deref(),
            Some("Created playlist \"New Mix\"")
        );
        // Inserted right after Liked Songs.
        assert_eq!(state.user_playlists().len(), 3);
        assert_eq!(state.user_playlists()[0].name, "New Mix");

        // Add focused track to playlist inside folder (index 1 -> Classical -> Folder Mix).
        let t1 = make_track("trk_add", "Sonata", 200);
        state.handle_event(AppEvent::SearchCompleted {
            query: "sonata".to_string(),
            results: SearchResults {
                tracks: vec![t1.clone()],
                albums: vec![],
            },
        });
        state.handle_action(Action::OpenAddToPlaylist);
        assert_eq!(state.input_mode, InputMode::AddToPlaylistPicker);
        state.handle_action(Action::ModalMoveDown);
        assert_eq!(state.selected_playlist_picker, 1);
        // Submit on folder drills in instead of closing.
        let cmds = state.handle_action(Action::ModalSubmit);
        assert!(cmds.is_empty());
        assert_eq!(state.input_mode, InputMode::AddToPlaylistPicker);
        assert_eq!(state.picker_folder_path, vec![1]);
        assert_eq!(state.current_picker_breadcrumb(), vec!["Classical"]);
        assert_eq!(state.selected_playlist_picker, 0);

        let cmds = state.handle_action(Action::ModalSubmit);
        assert_eq!(state.input_mode, InputMode::Normal);
        assert!(state.picker_folder_path.is_empty());
        assert_eq!(
            cmds,
            vec![Command::AddTrackToPlaylist {
                playlist_id: "pl_fold".to_string(),
                playlist_name: "Folder Mix".to_string(),
                track: t1,
            }]
        );

        state.handle_event(AppEvent::TrackAddedToPlaylist {
            playlist_name: "Folder Mix".to_string(),
            track_title: "Sonata".to_string(),
        });
        assert_eq!(
            state.notice_message.as_deref(),
            Some("Added \"Sonata\" to \"Folder Mix\"")
        );
    }

    #[test]
    fn jam_view_and_modal_actions_emit_expected_commands() {
        let mut state = AppState::new();
        state.handle_action(Action::SwitchView(View::Jam));
        assert_eq!(state.active_view, View::Jam);

        assert_eq!(
            state.handle_action(Action::RefreshJam),
            vec![Command::FetchJamSession]
        );

        state.handle_action(Action::OpenJamJoin);
        assert_eq!(state.input_mode, InputMode::JamJoinInput);
        for ch in "tok_123".chars() {
            state.handle_action(Action::ModalInputChar(ch));
        }
        assert_eq!(
            state.handle_action(Action::ModalSubmit),
            vec![Command::JoinJamSession("tok_123".to_string())]
        );

        let jam = JamSession {
            session_id: "sess_1".to_string(),
            join_token: "tok_123".to_string(),
            join_url: "https://spotify.link/tok_123".to_string(),
            owner_id: "host".to_string(),
            is_owner: false,
            is_listening: true,
            is_controlling: false,
            members: vec![],
        };
        state.handle_event(AppEvent::JamSessionLoaded(Some(jam)));
        assert!(state.jam_session.is_some());

        assert_eq!(
            state.handle_action(Action::LeaveJam),
            vec![Command::LeaveJamSession {
                session_id: "sess_1".to_string(),
                is_owner: false,
            }]
        );
        state.handle_event(AppEvent::JamLeft);
        assert!(state.jam_session.is_none());
        assert_eq!(state.notice_message.as_deref(), Some("Left Jam session"));
    }

    #[test]
    fn start_jam_in_jam_view_handles_created_event() {
        let mut state = AppState::new();
        state.active_view = View::Jam;
        state.status_message = Some("old error".to_string());

        assert_eq!(
            state.handle_action(Action::StartJam),
            vec![Command::CreateJamSession]
        );
        assert_eq!(state.active_view, View::Jam);
        assert_eq!(state.input_mode, InputMode::Normal);
        assert!(state.album_detail.is_none());
        assert!(state.status_message.is_none());

        let jam = JamSession {
            session_id: "sess_new".to_string(),
            join_token: "tok_new".to_string(),
            join_url: "https://open.spotify.com/socialsession/tok_new".to_string(),
            owner_id: "me".to_string(),
            is_owner: true,
            is_listening: true,
            is_controlling: true,
            members: vec![],
        };

        state.handle_event(AppEvent::JamSessionCreated {
            session: jam.clone(),
            copied_to_clipboard: true,
        });
        assert_eq!(state.jam_session, Some(jam.clone()));
        assert_eq!(
            state.notice_message.as_deref(),
            Some("Jam started — invite link copied to clipboard")
        );

        state.handle_event(AppEvent::JamSessionCreated {
            session: jam.clone(),
            copied_to_clipboard: false,
        });
        assert_eq!(state.jam_session, Some(jam));
        assert_eq!(state.notice_message.as_deref(), Some("Jam started"));
    }

    #[test]
    fn jam_actions_are_ignored_outside_jam_view() {
        let mut state = AppState::new();
        state.jam_session = Some(JamSession {
            session_id: "sess_1".to_string(),
            join_token: "tok_123".to_string(),
            join_url: "https://spotify.link/tok_123".to_string(),
            owner_id: "host".to_string(),
            is_owner: true,
            is_listening: true,
            is_controlling: true,
            members: vec![],
        });

        for view in [View::Search, View::MyAlbums, View::Queue] {
            state.active_view = view;
            assert!(state.handle_action(Action::RefreshJam).is_empty());
            assert!(state.handle_action(Action::StartJam).is_empty());
            assert!(state.handle_action(Action::OpenJamJoin).is_empty());
            assert_eq!(state.input_mode, InputMode::Normal);
            assert!(state.handle_action(Action::LeaveJam).is_empty());
        }

        state.active_view = View::Jam;
        state.album_detail = Some(AlbumDetail {
            origin_view: View::Jam,
            album: make_album("alb_1", "Album"),
            tracks: vec![],
            selected_track: 0,
        });
        assert!(state.handle_action(Action::RefreshJam).is_empty());
        assert!(state.handle_action(Action::StartJam).is_empty());
        assert!(state.handle_action(Action::OpenJamJoin).is_empty());
        assert_eq!(state.input_mode, InputMode::Normal);
        assert!(state.handle_action(Action::LeaveJam).is_empty());
    }

    #[test]
    fn album_detail_persists_on_origin_view_across_queue_switch() {
        let mut state = AppState::new();
        let album = make_album("a1", "Sonatas");
        let t1 = make_track("t1", "Op. 24 No. 1", 180);
        let t2 = make_track("t2", "Op. 24 No. 2", 210);

        state.handle_event(AppEvent::AlbumTracksLoaded {
            album,
            tracks: vec![t1, t2.clone()],
            auto_play: false,
        });
        assert_eq!(state.active_view, View::Search);
        assert!(state.active_album_detail().is_some());

        state.handle_action(Action::MoveDown);
        let cmds = state.handle_action(Action::Select);
        assert_eq!(cmds, vec![Command::PlayTrack(t2)]);
        assert_eq!(state.active_view, View::Queue);
        assert!(state.active_album_detail().is_none());
        assert!(state.album_detail.is_some());

        state.handle_action(Action::SwitchView(View::Search));
        assert_eq!(state.active_view, View::Search);
        assert_eq!(
            state.active_album_detail().map(|d| d.selected_track),
            Some(1)
        );

        state.handle_action(Action::Back);
        assert!(state.album_detail.is_none());
        assert!(state.active_album_detail().is_none());
    }

    #[test]
    fn add_to_playlist_picker_filters_non_playlists_and_supports_folder_back_out() {
        let mut state = AppState::new();
        let saved_album = make_album("alb_1", "Saved Album");
        let empty_album_folder = LibraryItem::Folder {
            name: "Albums Only".to_string(),
            items: vec![LibraryItem::Collection(saved_album.clone())],
        };
        let nested_pl = make_album("playlist:pl_nested", "Deep Mix");
        let mixed_folder = LibraryItem::Folder {
            name: "Genres".to_string(),
            items: vec![
                LibraryItem::Collection(saved_album.clone()),
                LibraryItem::Folder {
                    name: "Baroque".to_string(),
                    items: vec![LibraryItem::Collection(nested_pl)],
                },
            ],
        };
        state.handle_event(AppEvent::SavedAlbumsLoaded {
            items: vec![
                LibraryItem::Collection(saved_album),
                empty_album_folder,
                mixed_folder,
            ],
            liked_track_ids: HashSet::new(),
        });

        // Only "Genres" folder remains (Albums Only & Saved Album are filtered out).
        assert_eq!(state.current_picker_items().len(), 1);
        assert!(matches!(
            &state.current_picker_items()[0],
            LibraryItem::Folder { name, .. } if name == "Genres"
        ));

        let t1 = make_track("trk_1", "Partita", 180);
        state.handle_event(AppEvent::SearchCompleted {
            query: "partita".to_string(),
            results: SearchResults {
                tracks: vec![t1.clone()],
                albums: vec![],
            },
        });

        state.handle_action(Action::OpenAddToPlaylist);
        assert_eq!(state.input_mode, InputMode::AddToPlaylistPicker);
        // Drill into Genres -> Baroque.
        state.handle_action(Action::ModalSubmit);
        assert_eq!(state.current_picker_breadcrumb(), vec!["Genres"]);
        state.handle_action(Action::ModalSubmit);
        assert_eq!(state.current_picker_breadcrumb(), vec!["Genres", "Baroque"]);

        // ModalCancel pops one level and restores cursor, staying in picker.
        state.handle_action(Action::ModalCancel);
        assert_eq!(state.input_mode, InputMode::AddToPlaylistPicker);
        assert_eq!(state.current_picker_breadcrumb(), vec!["Genres"]);
        assert_eq!(state.selected_playlist_picker, 0);

        state.handle_action(Action::ModalCancel);
        assert_eq!(state.input_mode, InputMode::AddToPlaylistPicker);
        assert!(state.current_picker_breadcrumb().is_empty());

        // ModalCancel at root closes modal.
        state.handle_action(Action::ModalCancel);
        assert_eq!(state.input_mode, InputMode::Normal);
        assert!(state.add_to_playlist_track.is_none());
    }

    #[test]
    fn volume_controls_clamp() {
        let mut state = AppState::new();
        assert_eq!(state.volume, 80);

        assert_eq!(
            state.handle_action(Action::VolumeUp),
            vec![Command::SetVolume(85)]
        );
        assert_eq!(state.volume, 85);

        for _ in 0..5 {
            state.handle_action(Action::VolumeUp);
        }
        assert_eq!(state.volume, 100);
        assert_eq!(
            state.handle_action(Action::VolumeUp),
            vec![Command::SetVolume(100)]
        );

        for _ in 0..25 {
            state.handle_action(Action::VolumeDown);
        }
        assert_eq!(state.volume, 0);
        assert_eq!(
            state.handle_action(Action::VolumeDown),
            vec![Command::SetVolume(0)]
        );

        state.handle_event(AppEvent::VolumeChanged(120));
        assert_eq!(state.volume, 100);
        state.handle_event(AppEvent::VolumeChanged(42));
        assert_eq!(state.volume, 42);
    }

    #[test]
    fn previous_view_cycles_reverse_and_fetches_jam_session() {
        let mut state = AppState::new();
        assert_eq!(state.active_view, View::Search);

        assert_eq!(
            state.handle_action(Action::PreviousView),
            vec![Command::FetchJamSession]
        );
        assert_eq!(state.active_view, View::Jam);

        assert!(state.handle_action(Action::PreviousView).is_empty());
        assert_eq!(state.active_view, View::Queue);

        assert!(state.handle_action(Action::PreviousView).is_empty());
        assert_eq!(state.active_view, View::MyAlbums);

        assert!(state.handle_action(Action::PreviousView).is_empty());
        assert_eq!(state.active_view, View::Search);
    }

    #[test]
    fn queue_display_includes_now_playing_at_index_zero_and_handles_selection() {
        let mut state = AppState::new();
        let t1 = make_track("t1", "Track One", 120);
        let t2 = make_track("t2", "Track Two", 130);
        let t3 = make_track("t3", "Track Three", 140);

        state.handle_event(AppEvent::AlbumTracksLoaded {
            album: make_album("a1", "Album"),
            tracks: vec![t1.clone(), t2.clone(), t3.clone()],
            auto_play: true,
        });
        assert_eq!(state.active_view, View::Queue);
        assert_eq!(state.selected_queue_track, 0);
        assert_eq!(
            state.queue_display_tracks(),
            vec![t1.clone(), t2.clone(), t3.clone()]
        );

        // Selecting row 0 replays the active track without losing upcoming_queue.
        let replay_cmds = state.handle_action(Action::Select);
        assert_eq!(replay_cmds, vec![Command::PlayTrack(t1.clone())]);
        assert_eq!(state.upcoming_queue, vec![t2.clone(), t3.clone()]);
        assert_eq!(state.selected_queue_track, 0);

        // Moving down clamps to queue_display_tracks().len() - 1 = 2.
        state.handle_action(Action::MoveDown);
        state.handle_action(Action::MoveDown);
        state.handle_action(Action::MoveDown);
        assert_eq!(state.selected_queue_track, 2);

        // Selecting row 2 plays t3; t1 and t2 become hidden history and the cursor rests on row 0.
        let play_t3 = state.handle_action(Action::Select);
        assert_eq!(play_t3, vec![Command::PlayTrack(t3.clone())]);
        assert_eq!(state.now_playing.as_ref().unwrap().track, t3);
        assert!(state.upcoming_queue.is_empty());
        assert_eq!(state.selected_queue_track, 0);
        assert_eq!(state.queue_display_tracks(), vec![t3]);
    }

    #[test]
    fn playback_position_updated_ignores_small_drift_while_playing() {
        let mut state = AppState::new();
        state.now_playing = Some(NowPlaying {
            track: make_track("t1", "Track One", 120),
            status: PlaybackStatus::Playing,
            position: Duration::from_secs(30),
        });

        state.handle_event(AppEvent::PlaybackPositionUpdated(Duration::from_millis(
            30_900,
        )));
        assert_eq!(
            state.now_playing.as_ref().unwrap().position,
            Duration::from_secs(30)
        );

        state.handle_event(AppEvent::PlaybackPositionUpdated(Duration::from_millis(
            28_600,
        )));
        assert_eq!(
            state.now_playing.as_ref().unwrap().position,
            Duration::from_secs(30)
        );
    }

    #[test]
    fn playback_position_updated_snaps_on_large_drift_or_zero_while_playing() {
        let mut state = AppState::new();
        state.now_playing = Some(NowPlaying {
            track: make_track("t1", "Track One", 120),
            status: PlaybackStatus::Playing,
            position: Duration::ZERO,
        });

        // Snaps from ZERO even when update is below drift threshold.
        state.handle_event(AppEvent::PlaybackPositionUpdated(Duration::from_millis(
            500,
        )));
        assert_eq!(
            state.now_playing.as_ref().unwrap().position,
            Duration::from_millis(500)
        );

        // Snaps on >= 1500ms drift and clamps to track duration.
        state.handle_event(AppEvent::PlaybackPositionUpdated(Duration::from_secs(150)));
        assert_eq!(
            state.now_playing.as_ref().unwrap().position,
            Duration::from_secs(120)
        );

        // Snaps back to ZERO when synced position is ZERO.
        state.handle_event(AppEvent::PlaybackPositionUpdated(Duration::ZERO));
        assert_eq!(state.now_playing.as_ref().unwrap().position, Duration::ZERO);
    }

    #[test]
    fn playback_position_updated_always_snaps_when_paused_or_stopped() {
        let mut state = AppState::new();
        state.now_playing = Some(NowPlaying {
            track: make_track("t1", "Track One", 120),
            status: PlaybackStatus::Paused,
            position: Duration::from_secs(30),
        });

        state.handle_event(AppEvent::PlaybackPositionUpdated(Duration::from_millis(
            30_400,
        )));
        assert_eq!(
            state.now_playing.as_ref().unwrap().position,
            Duration::from_millis(30_400)
        );

        state.now_playing.as_mut().unwrap().status = PlaybackStatus::Stopped;
        state.handle_event(AppEvent::PlaybackPositionUpdated(Duration::from_millis(
            30_600,
        )));
        assert_eq!(
            state.now_playing.as_ref().unwrap().position,
            Duration::from_millis(30_600)
        );
    }

    #[test]
    fn queue_updated_does_not_clobber_active_multi_track_queue() {
        let mut state = AppState::new();
        let t1 = make_track("t1", "Album One", 100);
        let t2 = make_track("t2", "Album Two", 100);
        let t3 = make_track("t3", "Album Three", 100);
        state.handle_event(AppEvent::AlbumTracksLoaded {
            album: make_album("a1", "Album"),
            tracks: vec![t1, t2.clone(), t3.clone()],
            auto_play: false,
        });
        state.handle_action(Action::Select);
        assert_eq!(state.upcoming_queue, vec![t2.clone(), t3.clone()]);

        let soloist_partial = vec![make_track("x1", "Soloist Snippet", 100)];
        state.handle_event(AppEvent::QueueUpdated(soloist_partial));

        assert_eq!(state.upcoming_queue, vec![t2.clone(), t3]);
        assert_eq!(
            state.handle_action(Action::NextTrack),
            vec![Command::PlayTrack(t2)]
        );
    }

    #[test]
    fn queue_updated_populates_upcoming_when_queue_has_at_most_one_track() {
        let mut state = AppState::new();
        let current = make_track("c1", "Current", 100);
        let next1 = make_track("n1", "Next One", 100);
        let next2 = make_track("n2", "Next Two", 100);

        state.handle_event(AppEvent::TrackSynced {
            track: current,
            position: Duration::ZERO,
            status: PlaybackStatus::Playing,
        });
        assert_eq!(state.queue.len(), 1);

        state.handle_event(AppEvent::QueueUpdated(vec![next1.clone(), next2.clone()]));
        assert_eq!(state.upcoming_queue, vec![next1, next2]);
    }

    #[test]
    fn track_synced_for_track_in_queue_advances_cursor_and_updates_upcoming() {
        let mut state = AppState::new();
        let t1 = make_track("t1", "Album One", 100);
        let t2 = make_track("t2", "Album Two", 120);
        let t3 = make_track("t3", "Album Three", 140);
        state.handle_event(AppEvent::AlbumTracksLoaded {
            album: make_album("a1", "Album"),
            tracks: vec![t1, t2.clone(), t3.clone()],
            auto_play: false,
        });
        state.handle_action(Action::Select);
        assert_eq!(state.upcoming_queue, vec![t2.clone(), t3.clone()]);

        state.handle_event(AppEvent::TrackSynced {
            track: t2.clone(),
            position: Duration::from_secs(10),
            status: PlaybackStatus::Playing,
        });

        assert_eq!(state.queue.current_index(), Some(1));
        assert_eq!(state.upcoming_queue, vec![t3.clone()]);
        assert_eq!(state.now_playing.as_ref().unwrap().track, t2);
        assert_eq!(
            state.handle_action(Action::NextTrack),
            vec![Command::PlayTrack(t3)]
        );
    }

    #[test]
    fn track_synced_for_external_track_resets_stale_local_queue_during_jam() {
        let mut state = AppState::new();
        let t1 = make_track("t1", "Album One", 100);
        let t2 = make_track("t2", "Album Two", 120);
        state.handle_event(AppEvent::AlbumTracksLoaded {
            album: make_album("a1", "Album"),
            tracks: vec![t1, t2],
            auto_play: false,
        });
        state.handle_action(Action::Select);
        assert!(!state.upcoming_queue.is_empty());
        state.jam_session = Some(make_jam_session());

        let external = make_track("ext", "External Jam Track", 150);
        state.handle_event(AppEvent::TrackSynced {
            track: external.clone(),
            position: Duration::from_secs(5),
            status: PlaybackStatus::Playing,
        });

        assert_eq!(state.queue.len(), 1);
        assert_eq!(state.queue.tracks(), std::slice::from_ref(&external));
        assert!(state.upcoming_queue.is_empty());
        assert_eq!(state.selected_queue_track, 0);
        assert_eq!(state.now_playing.as_ref().unwrap().track, external);
        assert_eq!(
            state.handle_action(Action::NextTrack),
            vec![Command::SkipNext]
        );
    }

    #[test]
    fn seek_actions_update_position_clamp_bounds_and_ignore_when_idle() {
        let mut state = AppState::new();
        assert!(state.handle_action(Action::SeekForward).is_empty());
        assert!(state.handle_action(Action::SeekBackward).is_empty());

        let track = make_track("t1", "Track One", 12);
        state.handle_event(AppEvent::TrackSynced {
            track,
            position: Duration::from_secs(4),
            status: PlaybackStatus::Playing,
        });

        assert_eq!(
            state.handle_action(Action::SeekForward),
            vec![Command::Seek(Duration::from_secs(9))]
        );
        assert_eq!(
            state.now_playing.as_ref().unwrap().position,
            Duration::from_secs(9)
        );

        assert_eq!(
            state.handle_action(Action::SeekForward),
            vec![Command::Seek(Duration::from_secs(12))]
        );
        assert_eq!(
            state.now_playing.as_ref().unwrap().position,
            Duration::from_secs(12)
        );

        assert_eq!(
            state.handle_action(Action::SeekBackward),
            vec![Command::Seek(Duration::from_secs(7))]
        );
        assert_eq!(
            state.now_playing.as_ref().unwrap().position,
            Duration::from_secs(7)
        );

        assert_eq!(
            state.handle_action(Action::SeekBackward),
            vec![Command::Seek(Duration::from_secs(2))]
        );
        assert_eq!(
            state.handle_action(Action::SeekBackward),
            vec![Command::Seek(Duration::ZERO)]
        );
        assert_eq!(state.now_playing.as_ref().unwrap().position, Duration::ZERO);
    }

    #[test]
    fn remove_playlist_and_track_confirmation_flow_and_guards() {
        let mut state = AppState::new();
        let liked = make_album(LIKED_SONGS_ID, "Liked Songs");
        let saved_album = make_album("alb_1", "Goldberg");
        let root_playlist = make_album("playlist:pl_root", "Root Mix");
        let nested_playlist = make_album("playlist:pl_nested", "Nested Mix");

        state.handle_event(AppEvent::SavedAlbumsLoaded {
            items: vec![
                LibraryItem::Collection(liked),
                LibraryItem::Collection(saved_album.clone()),
                LibraryItem::Folder {
                    name: "Classical".to_string(),
                    items: vec![LibraryItem::Collection(nested_playlist.clone())],
                },
                LibraryItem::Collection(root_playlist),
            ],
            liked_track_ids: HashSet::new(),
        });

        // Ignored outside Library (Search, Queue, Jam).
        assert!(state.handle_action(Action::RequestRemove).is_empty());
        assert_eq!(state.input_mode, InputMode::Normal);
        state.handle_action(Action::SwitchView(View::Queue));
        assert!(state.handle_action(Action::RequestRemove).is_empty());
        state.handle_action(Action::SwitchView(View::Jam));
        assert!(state.handle_action(Action::RequestRemove).is_empty());

        // In Library: ignored on Liked Songs (index 0), Saved Album (index 1), Folder (index 2).
        state.handle_action(Action::SwitchView(View::MyAlbums));
        assert!(state.handle_action(Action::RequestRemove).is_empty());
        state.handle_action(Action::MoveDown);
        assert!(state.handle_action(Action::RequestRemove).is_empty());
        state.handle_action(Action::MoveDown);
        assert!(state.handle_action(Action::RequestRemove).is_empty());

        // Allowed on Root Mix (index 3) -> cancel clears pending_delete.
        state.handle_action(Action::MoveDown);
        assert!(state.handle_action(Action::RequestRemove).is_empty());
        assert_eq!(state.input_mode, InputMode::ConfirmDelete);
        assert_eq!(
            state.pending_delete,
            Some(PendingDelete::Playlist {
                playlist_id: "pl_root".to_string(),
                playlist_name: "Root Mix".to_string(),
            })
        );
        state.handle_action(Action::ModalCancel);
        assert_eq!(state.input_mode, InputMode::Normal);
        assert_eq!(state.pending_delete, None);

        // Enter nested folder and confirm removal of Nested Mix.
        state.handle_action(Action::MoveUp); // index 2: Classical folder
        state.handle_action(Action::Select);
        assert!(state.handle_action(Action::RequestRemove).is_empty());
        assert_eq!(state.input_mode, InputMode::ConfirmDelete);
        let cmds = state.handle_action(Action::ModalSubmit);
        assert_eq!(state.input_mode, InputMode::Normal);
        assert_eq!(state.pending_delete, None);
        assert_eq!(
            cmds,
            vec![Command::RemovePlaylist {
                playlist_id: "pl_nested".to_string(),
                playlist_name: "Nested Mix".to_string(),
            }]
        );
        state.handle_event(AppEvent::PlaylistRemoved {
            playlist_id: "pl_nested".to_string(),
            playlist_name: "Nested Mix".to_string(),
        });
        assert!(state.current_library_items().is_empty());
        assert_eq!(state.selected_saved_album, 0);
        assert_eq!(
            state.notice_message.as_deref(),
            Some("Removed playlist \"Nested Mix\"")
        );

        // Ignored inside non-playlist album detail.
        let t1 = make_track("t1", "Aria", 120);
        let t2 = make_track("t2", "Var 1", 100);
        state.handle_event(AppEvent::AlbumTracksLoaded {
            album: saved_album,
            tracks: vec![t1.clone(), t2.clone()],
            auto_play: false,
        });
        assert!(state.handle_action(Action::RequestRemove).is_empty());
        assert_eq!(state.input_mode, InputMode::Normal);

        // Allowed inside playlist detail; confirm removes selected track and clamps index.
        state.handle_event(AppEvent::AlbumTracksLoaded {
            album: make_album("playlist:pl_root", "Root Mix"),
            tracks: vec![t1, t2],
            auto_play: false,
        });
        state.handle_action(Action::MoveDown);
        assert!(state.handle_action(Action::RequestRemove).is_empty());
        assert_eq!(state.input_mode, InputMode::ConfirmDelete);
        let cmds = state.handle_action(Action::ModalSubmit);
        assert_eq!(
            cmds,
            vec![Command::RemoveTrackFromPlaylist {
                playlist_id: "pl_root".to_string(),
                playlist_name: "Root Mix".to_string(),
                track_id: "t2".to_string(),
                track_title: "Var 1".to_string(),
            }]
        );
        state.handle_event(AppEvent::TrackRemovedFromPlaylist {
            playlist_id: "pl_root".to_string(),
            playlist_name: "Root Mix".to_string(),
            track_id: "t2".to_string(),
            track_title: "Var 1".to_string(),
        });
        let detail = state.album_detail.as_ref().unwrap();
        assert_eq!(detail.tracks.len(), 1);
        assert_eq!(detail.tracks[0].id.as_str(), "t1");
        assert_eq!(detail.selected_track, 0);
        assert_eq!(
            state.notice_message.as_deref(),
            Some("Removed \"Var 1\" from \"Root Mix\"")
        );
    }

    fn numbered_tracks(count: usize) -> Vec<Track> {
        (0..count)
            .map(|i| make_track(&format!("t{i}"), &format!("Track {i}"), 100))
            .collect()
    }

    fn play_collection(state: &mut AppState, album: Album, tracks: Vec<Track>) {
        state.handle_event(AppEvent::AlbumTracksLoaded {
            album,
            tracks,
            auto_play: true,
        });
    }

    fn play_radio(state: &mut AppState, seed: &Track) {
        state.handle_action(Action::SwitchView(View::Search));
        state.handle_event(AppEvent::SearchCompleted {
            query: "q".to_string(),
            results: SearchResults {
                tracks: vec![seed.clone()],
                albums: vec![],
            },
        });
        state.handle_action(Action::Select);
    }

    fn make_jam_session() -> JamSession {
        JamSession {
            session_id: "s".to_string(),
            join_token: "t".to_string(),
            join_url: "u".to_string(),
            owner_id: "o".to_string(),
            is_owner: false,
            is_listening: true,
            is_controlling: false,
            members: Vec::new(),
        }
    }

    fn foreign_sync(track: Track) -> AppEvent {
        AppEvent::TrackSynced {
            track,
            position: Duration::from_secs(7),
            status: PlaybackStatus::Playing,
        }
    }

    #[test]
    fn starting_collections_and_radio_sets_queue_context() {
        let mut state = AppState::new();
        assert_eq!(state.queue_context, QueueContext::None);

        let tracks = numbered_tracks(3);
        state.handle_event(AppEvent::AlbumTracksLoaded {
            album: make_album("a1", "Blue"),
            tracks: tracks.clone(),
            auto_play: false,
        });
        assert_eq!(state.queue_context, QueueContext::None);
        state.handle_action(Action::Select);
        assert_eq!(
            state.queue_context,
            QueueContext::Collection {
                label: "Album: Blue".to_string(),
                tracks: tracks.clone(),
            }
        );

        play_collection(
            &mut state,
            make_album(&format!("{PLAYLIST_ID_PREFIX}p1"), "Mix"),
            tracks.clone(),
        );
        assert_eq!(
            state.queue_context,
            QueueContext::Collection {
                label: "Playlist: Mix".to_string(),
                tracks: tracks.clone(),
            }
        );

        play_collection(
            &mut state,
            make_album(LIKED_SONGS_ID, "Liked"),
            tracks.clone(),
        );
        assert_eq!(
            state.queue_context,
            QueueContext::Collection {
                label: "Liked Songs".to_string(),
                tracks,
            }
        );

        play_radio(&mut state, &make_track("seed", "Seed", 100));
        assert_eq!(state.queue_context, QueueContext::Radio);
    }

    #[test]
    fn foreign_sync_of_relinked_copy_updates_only_status_and_position() {
        let mut state = AppState::new();
        let tracks = numbered_tracks(3);
        play_collection(&mut state, make_album("a1", "Blue"), tracks.clone());
        let queue_before = state.queue.clone();
        let upcoming_before = state.upcoming_queue.clone();

        let mut relinked = tracks[0].clone();
        relinked.id = TrackId::new("relinked").unwrap();
        let cmds = state.handle_event(AppEvent::TrackSynced {
            track: relinked,
            position: Duration::from_secs(9),
            status: PlaybackStatus::Paused,
        });

        assert!(cmds.is_empty());
        assert_eq!(state.queue, queue_before);
        assert_eq!(state.upcoming_queue, upcoming_before);
        let np = state.now_playing.as_ref().unwrap();
        assert_eq!(np.track, tracks[0]);
        assert_eq!(np.status, PlaybackStatus::Paused);
        assert_eq!(np.position, Duration::from_secs(9));
    }

    #[test]
    fn foreign_sync_in_collection_advances_to_next_local_track() {
        let mut state = AppState::new();
        let tracks = numbered_tracks(3);
        play_collection(&mut state, make_album("a1", "Blue"), tracks.clone());

        let cmds = state.handle_event(foreign_sync(make_track("x", "Soloist Pick", 100)));

        assert_eq!(cmds, vec![Command::PlayTrack(tracks[1].clone())]);
        assert_eq!(state.queue.len(), 3);
        assert_eq!(state.upcoming_queue, vec![tracks[2].clone()]);
        assert_eq!(state.now_playing.as_ref().unwrap().track, tracks[1]);
        assert!(matches!(
            state.queue_context,
            QueueContext::Collection { .. }
        ));
    }

    #[test]
    fn foreign_sync_in_radio_advances_and_refills() {
        let mut state = AppState::new();
        let seed = make_track("seed", "Seed", 100);
        let r1 = make_track("r1", "Radio One", 100);
        let r2 = make_track("r2", "Radio Two", 100);
        play_radio(&mut state, &seed);
        state.handle_event(AppEvent::RadioTracksLoaded(vec![r1.clone(), r2.clone()]));

        let cmds = state.handle_event(foreign_sync(make_track("x", "Soloist Pick", 100)));

        assert_eq!(
            cmds,
            vec![Command::PlayTrack(r1), Command::FetchTrackRadio(r2)]
        );
        assert_eq!(state.queue_context, QueueContext::Radio);
        assert_eq!(state.queue.len(), 3);
    }

    #[test]
    fn foreign_sync_with_exhausted_queue_stops_playback_and_keeps_queue() {
        let mut state = AppState::new();
        let tracks = numbered_tracks(1);
        play_collection(&mut state, make_album("a1", "Blue"), tracks.clone());

        let cmds = state.handle_event(foreign_sync(make_track("x", "Soloist Pick", 100)));

        assert_eq!(cmds, vec![Command::StopPlayback]);
        let np = state.now_playing.as_ref().unwrap();
        assert_eq!(np.status, PlaybackStatus::Stopped);
        assert_eq!(np.position, Duration::ZERO);
        assert_eq!(state.queue.tracks(), tracks.as_slice());

        let cmds = state.handle_event(AppEvent::TrackSynced {
            track: make_track("x", "Soloist Pick", 100),
            position: Duration::from_secs(7),
            status: PlaybackStatus::Paused,
        });

        assert!(cmds.is_empty());
        let np = state.now_playing.as_ref().unwrap();
        assert_eq!(np.status, PlaybackStatus::Stopped);
        assert_eq!(np.position, Duration::ZERO);
        assert_eq!(state.queue.tracks(), tracks.as_slice());
    }

    #[test]
    fn foreign_sync_replaces_queue_without_context_or_during_jam() {
        let mut state = AppState::new();
        let foreign = make_track("x", "Soloist Pick", 100);
        state.handle_event(foreign_sync(foreign.clone()));
        assert_eq!(state.queue.tracks(), std::slice::from_ref(&foreign));
        assert_eq!(state.queue_context, QueueContext::None);

        play_collection(&mut state, make_album("a1", "Blue"), numbered_tracks(3));
        state.jam_session = Some(make_jam_session());
        let cmds = state.handle_event(foreign_sync(foreign.clone()));
        assert!(cmds.is_empty());
        assert_eq!(state.queue.tracks(), std::slice::from_ref(&foreign));
        assert_eq!(state.queue_context, QueueContext::None);
        assert!(state.upcoming_queue.is_empty());
    }

    #[test]
    fn queue_updated_is_ignored_in_radio_and_collection_and_capped_when_idle() {
        let soloist_queue = numbered_tracks(30);

        let mut state = AppState::new();
        state.handle_event(AppEvent::QueueUpdated(soloist_queue.clone()));
        assert_eq!(state.upcoming_queue.len(), QUEUE_UPCOMING_LIMIT);

        let mut state = AppState::new();
        play_radio(&mut state, &make_track("seed", "Seed", 100));
        state.handle_event(AppEvent::QueueUpdated(soloist_queue.clone()));
        assert!(state.upcoming_queue.is_empty());

        let mut state = AppState::new();
        play_collection(&mut state, make_album("a1", "Blue"), numbered_tracks(1));
        state.handle_event(AppEvent::QueueUpdated(soloist_queue));
        assert!(state.upcoming_queue.is_empty());
    }

    #[test]
    fn queue_hides_history_but_previous_track_returns_to_it() {
        let mut state = AppState::new();
        let tracks = numbered_tracks(5);
        play_collection(&mut state, make_album("a1", "Blue"), tracks.clone());
        state.handle_event(AppEvent::TrackEnded);
        state.handle_event(AppEvent::TrackEnded);

        assert_eq!(state.queue_display_tracks(), tracks[2..].to_vec());
        assert_eq!(state.selected_queue_track, 0);

        let cmds = state.handle_action(Action::PreviousTrack);

        assert_eq!(cmds, vec![Command::PlayTrack(tracks[1].clone())]);
        assert_eq!(state.queue_display_tracks(), tracks[1..].to_vec());
    }

    #[test]
    fn radio_history_is_capped_at_one_hundred_tracks() {
        let mut state = AppState::new();
        let seed = make_track("seed", "Seed", 100);
        play_radio(&mut state, &seed);
        let radio: Vec<Track> = (0..130)
            .map(|i| make_track(&format!("r{i}"), &format!("Radio {i}"), 100))
            .collect();
        state.handle_event(AppEvent::RadioTracksLoaded(radio));
        let full = state.queue.tracks().to_vec();
        assert_eq!(full.len(), 131);

        state.queue.set_tracks(full.clone(), 120);
        state.now_playing = Some(NowPlaying {
            track: full[120].clone(),
            status: PlaybackStatus::Playing,
            position: Duration::ZERO,
        });
        state.sync_upcoming_from_queue();

        assert_eq!(state.queue.current_index(), Some(QUEUE_HISTORY_LIMIT));
        assert_eq!(state.queue.current(), Some(&full[120]));
        assert_eq!(state.queue.tracks()[0], full[20]);
    }

    #[test]
    fn queue_display_caps_radio_at_twenty_rows_and_shows_full_collection() {
        let mut state = AppState::new();
        let seed = make_track("seed", "Seed", 100);
        play_radio(&mut state, &seed);
        let radio: Vec<Track> = (0..30)
            .map(|i| make_track(&format!("r{i}"), &format!("Radio {i}"), 100))
            .collect();
        state.handle_event(AppEvent::RadioTracksLoaded(radio));
        assert_eq!(state.upcoming_queue.len(), 30);
        assert_eq!(state.queue_display_tracks().len(), 20);

        play_collection(&mut state, make_album("a1", "Blue"), numbered_tracks(31));
        assert_eq!(state.queue_display_tracks().len(), 31);
    }

    #[test]
    fn radio_refill_fires_only_below_display_limit() {
        let mut state = AppState::new();
        let seed = make_track("seed", "Seed", 100);
        play_radio(&mut state, &seed);
        let radio: Vec<Track> = (0..25)
            .map(|i| make_track(&format!("r{i}"), &format!("Radio {i}"), 100))
            .collect();
        state.handle_event(AppEvent::RadioTracksLoaded(radio.clone()));

        assert_eq!(
            state.handle_action(Action::NextTrack),
            vec![Command::PlayTrack(radio[0].clone())]
        );
        state.handle_action(Action::NextTrack);
        state.handle_action(Action::NextTrack);
        state.handle_action(Action::NextTrack);
        state.handle_action(Action::NextTrack);
        assert_eq!(state.upcoming_queue.len(), 20);
        assert_eq!(
            state.handle_action(Action::NextTrack),
            vec![Command::PlayTrack(radio[5].clone())]
        );
        assert_eq!(state.upcoming_queue.len(), QUEUE_UPCOMING_LIMIT);
        let cmds = state.handle_action(Action::NextTrack);
        assert_eq!(
            cmds,
            vec![
                Command::PlayTrack(radio[6].clone()),
                Command::FetchTrackRadio(radio[24].clone())
            ]
        );
    }

    #[test]
    fn shuffle_in_collection_permutes_upcoming_and_off_restores_order() {
        let mut state = AppState::new();
        let tracks = numbered_tracks(40);
        play_collection(&mut state, make_album("a1", "Blue"), tracks.clone());
        state.handle_action(Action::NextTrack);

        assert!(state.handle_action(Action::ToggleShuffle).is_empty());
        assert!(state.shuffle);
        assert_eq!(state.queue.current_index(), Some(1));
        assert_eq!(state.queue.current(), Some(&tracks[1]));
        assert_eq!(state.queue.tracks()[..2], tracks[..2]);
        let mut shuffled_tail = state.queue.tracks()[2..].to_vec();
        assert_eq!(state.upcoming_queue, shuffled_tail);
        shuffled_tail.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        let mut original_tail = tracks[2..].to_vec();
        original_tail.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        assert_eq!(shuffled_tail, original_tail);

        assert!(state.handle_action(Action::ToggleShuffle).is_empty());
        assert!(!state.shuffle);
        assert_eq!(state.queue.tracks(), tracks.as_slice());
        assert_eq!(state.queue.current_index(), Some(1));
        assert_eq!(state.upcoming_queue, tracks[2..].to_vec());
    }

    #[test]
    fn shuffle_outside_collection_is_a_silent_no_op() {
        let mut state = AppState::new();
        play_radio(&mut state, &make_track("seed", "Seed", 100));
        state.status_message = Some("status".to_string());
        state.notice_message = Some("notice".to_string());

        assert!(state.handle_action(Action::ToggleShuffle).is_empty());
        assert!(!state.shuffle);
        assert_eq!(state.status_message.as_deref(), Some("status"));
        assert_eq!(state.notice_message.as_deref(), Some("notice"));

        let mut idle = AppState::new();
        assert!(idle.handle_action(Action::ToggleShuffle).is_empty());
        assert!(!idle.shuffle);
    }

    #[test]
    fn starting_a_new_queue_resets_shuffle() {
        let mut state = AppState::new();
        play_collection(&mut state, make_album("a1", "Blue"), numbered_tracks(5));
        state.handle_action(Action::ToggleShuffle);
        assert!(state.shuffle);

        play_collection(&mut state, make_album("a2", "Red"), numbered_tracks(5));
        assert!(!state.shuffle);

        state.handle_action(Action::ToggleShuffle);
        assert!(state.shuffle);
        play_radio(&mut state, &make_track("seed", "Seed", 100));
        assert!(!state.shuffle);
    }

    #[test]
    fn play_selected_in_album_detail_starts_from_first_track_and_select_from_selected() {
        let tracks = numbered_tracks(3);
        let open_detail = || {
            let mut state = AppState::new();
            state.handle_event(AppEvent::AlbumTracksLoaded {
                album: make_album("a1", "Blue"),
                tracks: tracks.clone(),
                auto_play: false,
            });
            state.handle_action(Action::MoveDown);
            state.handle_action(Action::MoveDown);
            state
        };

        let mut state = open_detail();
        assert_eq!(
            state.handle_action(Action::PlaySelected),
            vec![Command::PlayTrack(tracks[0].clone())]
        );

        let mut state = open_detail();
        assert_eq!(
            state.handle_action(Action::Select),
            vec![Command::PlayTrack(tracks[2].clone())]
        );
    }

    fn make_cover_art(id: &str) -> CoverArt {
        CoverArt {
            track_id: TrackId::new(id).unwrap(),
            width: 2,
            height: 2,
            rgb: std::sync::Arc::from(vec![0u8; 12]),
            encoded_png: std::sync::Arc::from(b"\x89PNG".to_vec()),
        }
    }

    #[test]
    fn toggle_focus_and_back_preserve_underlying_view_and_album_detail() {
        let mut state = AppState::new();
        state.handle_event(AppEvent::AlbumTracksLoaded {
            album: make_album("a1", "Blue"),
            tracks: numbered_tracks(2),
            auto_play: false,
        });
        assert!(state.album_detail.is_some());

        assert!(state.handle_action(Action::ToggleFocus).is_empty());
        assert!(state.focus_mode);

        // Esc exits focus_mode without closing album_detail.
        assert!(state.handle_action(Action::Back).is_empty());
        assert!(!state.focus_mode);
        assert!(state.album_detail.is_some());

        // ToggleFocus toggles on and off.
        assert!(state.handle_action(Action::ToggleFocus).is_empty());
        assert!(state.focus_mode);
        assert!(state.handle_action(Action::ToggleFocus).is_empty());
        assert!(!state.focus_mode);
    }

    #[test]
    fn cover_art_is_fetched_lazily_in_focus_mode_and_ignores_stale_responses() {
        let mut t1 = make_track("t1", "One", 120);
        t1.cover_url = Some("https://i.scdn.co/image/one".to_string());
        let mut t2 = make_track("t2", "Two", 120);
        t2.cover_url = Some("https://i.scdn.co/image/two".to_string());

        let mut state = AppState::new();
        // Outside focus mode, playing a track does not emit FetchCoverArt.
        let cmds = state.handle_event(AppEvent::TrackSynced {
            track: t1.clone(),
            position: Duration::ZERO,
            status: PlaybackStatus::Playing,
        });
        assert!(cmds.is_empty());

        // Entering focus mode emits FetchCoverArt for the playing track.
        let cmds = state.handle_action(Action::ToggleFocus);
        assert_eq!(
            cmds,
            vec![Command::FetchCoverArt {
                track_id: t1.id.clone(),
                cover_url: t1.cover_url.clone(),
            }]
        );
        // Re-toggling off and on while request is in flight does not duplicate.
        state.handle_action(Action::ToggleFocus);
        assert!(state.handle_action(Action::ToggleFocus).is_empty());

        // Track changes while in focus mode: clears stale art/request and fetches new track art.
        let cmds = state.handle_event(AppEvent::TrackSynced {
            track: t2.clone(),
            position: Duration::ZERO,
            status: PlaybackStatus::Playing,
        });
        assert_eq!(
            cmds,
            vec![Command::FetchCoverArt {
                track_id: t2.id.clone(),
                cover_url: t2.cover_url.clone(),
            }]
        );

        // Stale response for t1 is ignored.
        state.handle_event(AppEvent::CoverArtLoaded(make_cover_art("t1")));
        assert!(state.cover_art.is_none());

        // Matching response for t2 is stored.
        let art2 = make_cover_art("t2");
        state.handle_event(AppEvent::CoverArtLoaded(art2.clone()));
        assert_eq!(state.cover_art, Some(art2));
    }

    #[test]
    fn toggle_retro_art_flips_flag_without_emitting_commands() {
        let mut state = AppState::new();
        assert!(!state.retro_art);

        assert!(state.handle_action(Action::ToggleRetroArt).is_empty());
        assert!(state.retro_art);

        assert!(state.handle_action(Action::ToggleRetroArt).is_empty());
        assert!(!state.retro_art);
    }
}
