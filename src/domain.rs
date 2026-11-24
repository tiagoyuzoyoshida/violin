use std::time::Duration;
use thiserror::Error;

/// Errors produced when constructing or validating domain types.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DomainError {
    /// Returned when a required identifier is empty or whitespace-only.
    #[error("{field} must not be empty")]
    EmptyId { field: &'static str },
}

fn validate_non_empty_id(
    id: impl Into<String>,
    field: &'static str,
) -> Result<String, DomainError> {
    let value = id.into();
    if value.trim().is_empty() {
        return Err(DomainError::EmptyId { field });
    }
    Ok(value)
}

/// Validated non-empty track identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrackId(String);

impl TrackId {
    /// Creates a `TrackId`, failing with `DomainError::EmptyId` if `id` is empty or whitespace-only.
    pub fn new(id: impl Into<String>) -> Result<Self, DomainError> {
        validate_non_empty_id(id, "track_id").map(Self)
    }

    /// Returns the underlying identifier string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Validated non-empty album identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AlbumId(String);

impl AlbumId {
    /// Creates an `AlbumId`, failing with `DomainError::EmptyId` if `id` is empty or whitespace-only.
    pub fn new(id: impl Into<String>) -> Result<Self, DomainError> {
        validate_non_empty_id(id, "album_id").map(Self)
    }

    /// Returns the underlying identifier string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Metadata for a playable track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub id: TrackId,
    pub title: String,
    pub artists: Vec<String>,
    pub album_name: String,
    pub duration: Duration,
    pub cover_url: Option<String>,
}

/// Decoded cover art image ready for terminal rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverArt {
    pub track_id: TrackId,
    pub width: u32,
    pub height: u32,
    pub rgb: std::sync::Arc<[u8]>,
    pub encoded_png: std::sync::Arc<[u8]>,
}

impl Track {
    /// Formats `artists` as a comma-separated string.
    pub fn artists_display(&self) -> String {
        self.artists.join(", ")
    }
}

/// Metadata for an album.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Album {
    pub id: AlbumId,
    pub name: String,
    pub artists: Vec<String>,
    pub release_year: Option<u16>,
    pub total_tracks: u32,
}

impl Album {
    /// Formats `artists` as a comma-separated string.
    pub fn artists_display(&self) -> String {
        self.artists.join(", ")
    }
}

/// Entry in the Library view: a playable collection (Liked Songs, playlist, or saved album)
/// or a nested Spotify playlist folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibraryItem {
    Collection(Album),
    Folder {
        name: String,
        items: Vec<LibraryItem>,
    },
}

/// Participant in an active Spotify Jam session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JamMember {
    pub username: String,
    pub display_name: String,
    pub is_listening: bool,
    pub is_controlling: bool,
}

/// Active Spotify Jam (social-connect) session metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JamSession {
    pub session_id: String,
    pub join_token: String,
    pub join_url: String,
    pub owner_id: String,
    pub is_owner: bool,
    pub is_listening: bool,
    pub is_controlling: bool,
    pub members: Vec<JamMember>,
}

/// Combined track and album search results.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchResults {
    pub tracks: Vec<Track>,
    pub albums: Vec<Album>,
}

/// Current state of audio playback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackStatus {
    Stopped,
    Playing,
    Paused,
}

/// Ordered list of tracks with an optional active cursor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlaybackQueue {
    tracks: Vec<Track>,
    current_index: Option<usize>,
}

impl PlaybackQueue {
    /// Replaces the queue tracks and sets the cursor to `start_index` clamped to the last element,
    /// or clears the cursor if `tracks` is empty.
    pub fn set_tracks(&mut self, tracks: Vec<Track>, start_index: usize) -> Option<&Track> {
        if tracks.is_empty() {
            self.tracks.clear();
            self.current_index = None;
            return None;
        }
        let clamped = start_index.min(tracks.len() - 1);
        self.tracks = tracks;
        self.current_index = Some(clamped);
        self.current()
    }

    /// Returns the currently selected track, if any.
    pub fn current(&self) -> Option<&Track> {
        self.current_index.and_then(|idx| self.tracks.get(idx))
    }

    /// Returns the index of the currently selected track, if any.
    pub fn current_index(&self) -> Option<usize> {
        self.current_index
    }

    /// Returns the number of tracks in the queue.
    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    /// Returns `true` if the queue contains no tracks.
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    /// Returns a slice of all tracks in the queue.
    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    /// Advances the cursor to the next track if one exists; otherwise leaves it unchanged and returns `None`.
    pub fn next_track(&mut self) -> Option<&Track> {
        let idx = self.current_index?;
        if idx + 1 < self.tracks.len() {
            self.current_index = Some(idx + 1);
            return self.current();
        }
        None
    }

    /// Moves the cursor to the previous track if one exists; otherwise leaves it unchanged and returns `None`.
    pub fn previous_track(&mut self) -> Option<&Track> {
        let idx = self.current_index?;
        if idx > 0 {
            self.current_index = Some(idx - 1);
            return self.current();
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_track(id: &str, title: &str) -> Track {
        Track {
            id: TrackId::new(id).expect("valid track id"),
            title: title.to_string(),
            artists: vec!["Artist A".to_string(), "Artist B".to_string()],
            album_name: "Album".to_string(),
            duration: Duration::from_secs(180),
            cover_url: None,
        }
    }

    #[test]
    fn track_id_accepts_non_empty_and_rejects_blank() {
        let valid = TrackId::new("trk_1").unwrap();
        assert_eq!(valid.as_str(), "trk_1");

        assert_eq!(
            TrackId::new(""),
            Err(DomainError::EmptyId { field: "track_id" })
        );
        assert_eq!(
            TrackId::new("   \t\n "),
            Err(DomainError::EmptyId { field: "track_id" })
        );
    }

    #[test]
    fn album_id_accepts_non_empty_and_rejects_blank() {
        let valid = AlbumId::new("alb_1").unwrap();
        assert_eq!(valid.as_str(), "alb_1");

        assert_eq!(
            AlbumId::new(""),
            Err(DomainError::EmptyId { field: "album_id" })
        );
        assert_eq!(
            AlbumId::new("   "),
            Err(DomainError::EmptyId { field: "album_id" })
        );
    }

    #[test]
    fn artists_display_joins_with_comma_and_space() {
        let track = sample_track("t1", "One");
        assert_eq!(track.artists_display(), "Artist A, Artist B");

        let album = Album {
            id: AlbumId::new("a1").unwrap(),
            name: "Sonata".to_string(),
            artists: vec!["Beethoven".to_string()],
            release_year: Some(1803),
            total_tracks: 3,
        };
        assert_eq!(album.artists_display(), "Beethoven");

        let empty_artists = Album {
            id: AlbumId::new("a2").unwrap(),
            name: "Unknown".to_string(),
            artists: vec![],
            release_year: None,
            total_tracks: 0,
        };
        assert_eq!(empty_artists.artists_display(), "");
    }

    #[test]
    fn queue_starts_empty_and_handles_empty_set() {
        let mut queue = PlaybackQueue::default();
        assert!(queue.is_empty());
        assert_eq!(queue.len(), 0);
        assert_eq!(queue.current(), None);
        assert_eq!(queue.current_index(), None);
        assert_eq!(queue.next_track(), None);
        assert_eq!(queue.previous_track(), None);

        queue.set_tracks(vec![sample_track("t1", "One")], 0);
        assert_eq!(queue.len(), 1);

        assert_eq!(queue.set_tracks(vec![], 0), None);
        assert!(queue.is_empty());
        assert_eq!(queue.current(), None);
        assert_eq!(queue.current_index(), None);
    }

    #[test]
    fn queue_set_tracks_clamps_out_of_bounds_start_index() {
        let mut queue = PlaybackQueue::default();
        let tracks = vec![sample_track("t1", "One"), sample_track("t2", "Two")];

        let current = queue.set_tracks(tracks, 99).unwrap();
        assert_eq!(current.title, "Two");
        assert_eq!(queue.current_index(), Some(1));
        assert_eq!(queue.tracks().len(), 2);
    }

    #[test]
    fn queue_next_and_previous_respect_boundaries() {
        let mut queue = PlaybackQueue::default();
        let tracks = vec![
            sample_track("t1", "One"),
            sample_track("t2", "Two"),
            sample_track("t3", "Three"),
        ];
        queue.set_tracks(tracks, 0);

        assert_eq!(queue.previous_track(), None);
        assert_eq!(queue.current_index(), Some(0));

        assert_eq!(queue.next_track().unwrap().title, "Two");
        assert_eq!(queue.current_index(), Some(1));

        assert_eq!(queue.next_track().unwrap().title, "Three");
        assert_eq!(queue.current_index(), Some(2));

        assert_eq!(queue.next_track(), None);
        assert_eq!(queue.current_index(), Some(2));

        assert_eq!(queue.previous_track().unwrap().title, "Two");
        assert_eq!(queue.current_index(), Some(1));
    }
}
