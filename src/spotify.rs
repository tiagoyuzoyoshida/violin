use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use librespot_core::error::ErrorKind;
use librespot_core::{Session, SpotifyId, SpotifyUri};
use librespot_protocol::extended_metadata::{BatchedEntityRequest, EntityRequest, ExtensionQuery};
use librespot_protocol::extension_kind::ExtensionKind;
use librespot_protocol::metadata as proto_metadata;
use librespot_protocol::playlist4_external::{
    Add, CreateListReply, Delta, Item, ListAttributes, ListAttributesPartialState, ListChanges, Op,
    Rem, SelectedListContent, UpdateListAttributes, op::Kind as OpKind,
};
use librespot_protocol::social_connect_v2::Session as ProtoJamSession;
use protobuf::{EnumOrUnknown, Message, MessageField};
use reqwest::StatusCode;
use serde::Deserialize;
use thiserror::Error;

use crate::domain::{
    Album, AlbumId, DomainError, JamMember, JamSession, LibraryItem, SearchResults, Track, TrackId,
};

const DEFAULT_PATHFINDER_URL: &str = "https://api-partner.spotify.com/pathfinder/v2/query";
const SEARCH_DESKTOP_OPERATION: &str = "searchDesktop";
const SEARCH_DESKTOP_SHA256: &str =
    "75bbf6bfcfdf85b8fc828417bfad92b7cd66bf7f556d85670f4da8292373ebec";
const SEARCH_LIMIT: u32 = 20;
const COLLECTION_PAGE_LIMIT: u32 = 200;
const SAVED_ALBUMS_LIMIT: usize = 50;
const ROOTLIST_LIMIT: usize = 200;
const PLAYLIST_TRACKS_LIMIT: usize = 100;
const METADATA_BATCH_SIZE: usize = 50;
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_RETRY_AFTER_SECS: u64 = 1;

pub const LIKED_SONGS_ID: &str = "liked-songs";
pub const PLAYLIST_ID_PREFIX: &str = "playlist:";

#[derive(Debug, Error)]
pub enum SpotifyError {
    #[error("Spotify HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Spotify desktop client error: {0}")]
    SpClient(String),
    #[error("Spotify API rate limited; retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },
    #[error("Spotify API error (HTTP {status}): {message}")]
    Api { status: u16, message: String },
    #[error("failed to parse Spotify JSON response: {0}")]
    ParseJson(#[from] serde_json::Error),
    #[error("failed to parse Spotify protobuf metadata: {0}")]
    ParseProto(#[from] protobuf::Error),
    #[error("invalid Spotify URI '{uri}': {reason}")]
    InvalidUri { uri: String, reason: String },
    #[error("invalid domain data from Spotify: {0}")]
    Domain(#[from] DomainError),
}

#[derive(Debug, Clone)]
pub struct SpotifyClient {
    http: reqwest::Client,
    pathfinder_url: String,
}

impl SpotifyClient {
    /// Creates a `SpotifyClient` targeting Spotify's desktop Pathfinder endpoint.
    pub fn new() -> Result<Self, SpotifyError> {
        Self::with_pathfinder_url(DEFAULT_PATHFINDER_URL)
    }

    /// Creates a `SpotifyClient` targeting a custom Pathfinder endpoint URL.
    pub fn with_pathfinder_url(url: impl Into<String>) -> Result<Self, SpotifyError> {
        let http = reqwest::Client::builder().timeout(HTTP_TIMEOUT).build()?;
        Ok(Self {
            http,
            pathfinder_url: url.into(),
        })
    }

    /// Searches Spotify tracks and albums matching `query` via desktop Pathfinder v2.
    pub async fn search(
        &self,
        session: &Session,
        query: &str,
    ) -> Result<SearchResults, SpotifyError> {
        let auth_tok = session
            .login5()
            .auth_token()
            .await
            .map_err(|err| SpotifyError::SpClient(err.to_string()))?;
        let client_tok = session
            .spclient()
            .client_token()
            .await
            .map_err(|err| SpotifyError::SpClient(err.to_string()))?;

        let payload = serde_json::json!({
            "variables": {
                "searchTerm": query,
                "offset": 0,
                "limit": SEARCH_LIMIT,
                "numberOfTopResults": 5,
                "includeAudiobooks": false,
                "includeArtistHasConcertsField": false,
                "includePreReleases": false,
                "includeLocalConcertsField": false
            },
            "operationName": SEARCH_DESKTOP_OPERATION,
            "extensions": {
                "persistedQuery": {
                    "version": 1,
                    "sha256Hash": SEARCH_DESKTOP_SHA256
                }
            }
        });

        let response = self
            .http
            .post(&self.pathfinder_url)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("{} {}", auth_tok.token_type, auth_tok.access_token),
            )
            .header("client-token", client_tok)
            .json(&payload)
            .send()
            .await?;

        let body = read_success_body(response).await?;
        parse_pathfinder_search_response(&body)
    }

    /// Fetches the current user's Liked Songs, Playlists, Saved Albums, and liked track IDs via `spclient`.
    pub async fn load_library(
        &self,
        session: &Session,
    ) -> Result<(Vec<LibraryItem>, HashSet<String>), SpotifyError> {
        let response_body = self.fetch_collection_paging(session).await?;
        let liked_track_uris = parse_collection_uris(
            &response_body,
            "spotify:track:",
            COLLECTION_PAGE_LIMIT as usize,
        )?;
        let liked_track_ids = liked_track_uris
            .iter()
            .filter_map(|uri| uri.strip_prefix("spotify:track:"))
            .map(ToOwned::to_owned)
            .collect();
        let saved_album_uris = parse_collection_album_uris(&response_body)?;

        let mut collections = Vec::new();
        if !liked_track_uris.is_empty() {
            collections.push(LibraryItem::Collection(Album {
                id: AlbumId::new(LIKED_SONGS_ID)?,
                name: "Liked Songs".to_string(),
                artists: vec!["Your Library".to_string()],
                release_year: None,
                total_tracks: liked_track_uris.len() as u32,
            }));
        }

        match session
            .spclient()
            .get_rootlist(0, Some(ROOTLIST_LIMIT))
            .await
        {
            Ok(rootlist_bytes) => {
                collections.extend(parse_rootlist_items_proto(
                    &rootlist_bytes,
                    &session.username(),
                )?);
            }
            Err(err) => {
                if collections.is_empty() {
                    return Err(SpotifyError::SpClient(err.to_string()));
                }
            }
        }

        for chunk in saved_album_uris.chunks(METADATA_BATCH_SIZE) {
            let mut metadata_by_uri =
                fetch_extended_metadata_batch(session, chunk, ExtensionKind::ALBUM_V4).await?;
            for uri in chunk {
                if let Some(bytes) = metadata_by_uri.remove(uri) {
                    collections.push(LibraryItem::Collection(parse_album_metadata_proto(&bytes)?));
                }
            }
        }
        Ok((collections, liked_track_ids))
    }

    /// Likes (`liked = true`) or unlikes (`liked = false`) a track in the user's collection.
    pub async fn set_track_liked(
        &self,
        session: &Session,
        track_id: &str,
        liked: bool,
    ) -> Result<(), SpotifyError> {
        let timestamp_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let payload =
            build_collection_write_payload(&session.username(), track_id, liked, timestamp_secs);
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );
        session
            .spclient()
            .request(
                &http::Method::POST,
                "/collection/v2/write",
                Some(headers),
                Some(payload.as_bytes()),
            )
            .await
            .map_err(|err| SpotifyError::SpClient(err.to_string()))?;
        Ok(())
    }

    /// Creates a new playlist with `name` and prepends it to the user's rootlist.
    pub async fn create_playlist(
        &self,
        session: &Session,
        name: &str,
    ) -> Result<Album, SpotifyError> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(SpotifyError::SpClient(
                "playlist name must not be empty".to_string(),
            ));
        }

        let changes = build_create_playlist_changes(trimmed);
        let reply_bytes = session
            .spclient()
            .request_with_protobuf(&http::Method::POST, "/playlist/v2/playlist", None, &changes)
            .await
            .map_err(|err| SpotifyError::SpClient(err.to_string()))?;
        let base62 = parse_create_list_reply(&reply_bytes)?;

        let rootlist_changes = build_add_item_changes(&format!("spotify:playlist:{base62}"), true);
        let rootlist_endpoint =
            format!("/playlist/v2/user/{}/rootlist/changes", session.username());
        session
            .spclient()
            .request_with_protobuf(
                &http::Method::POST,
                &rootlist_endpoint,
                None,
                &rootlist_changes,
            )
            .await
            .map_err(|err| SpotifyError::SpClient(err.to_string()))?;

        Ok(Album {
            id: AlbumId::new(format!("{PLAYLIST_ID_PREFIX}{base62}"))?,
            name: trimmed.to_string(),
            artists: Vec::new(),
            release_year: None,
            total_tracks: 0,
        })
    }

    /// Appends `track_id` to the playlist identified by `playlist_base62`.
    pub async fn add_track_to_playlist(
        &self,
        session: &Session,
        playlist_base62: &str,
        track_id: &str,
    ) -> Result<(), SpotifyError> {
        let changes = build_add_item_changes(&format!("spotify:track:{track_id}"), false);
        let endpoint = format!("/playlist/v2/playlist/{playlist_base62}/changes");
        session
            .spclient()
            .request_with_protobuf(&http::Method::POST, &endpoint, None, &changes)
            .await
            .map_err(|err| SpotifyError::SpClient(err.to_string()))?;
        Ok(())
    }

    /// Removes the playlist identified by `playlist_base62` from the user's rootlist.
    pub async fn remove_playlist(
        &self,
        session: &Session,
        playlist_base62: &str,
    ) -> Result<(), SpotifyError> {
        let changes = build_remove_item_changes(&format!("spotify:playlist:{playlist_base62}"));
        let endpoint = format!("/playlist/v2/user/{}/rootlist/changes", session.username());
        session
            .spclient()
            .request_with_protobuf(&http::Method::POST, &endpoint, None, &changes)
            .await
            .map_err(|err| SpotifyError::SpClient(err.to_string()))?;
        Ok(())
    }

    /// Removes `track_id` from the playlist identified by `playlist_base62`.
    pub async fn remove_track_from_playlist(
        &self,
        session: &Session,
        playlist_base62: &str,
        track_id: &str,
    ) -> Result<(), SpotifyError> {
        let changes = build_remove_item_changes(&format!("spotify:track:{track_id}"));
        let endpoint = format!("/playlist/v2/playlist/{playlist_base62}/changes");
        session
            .spclient()
            .request_with_protobuf(&http::Method::POST, &endpoint, None, &changes)
            .await
            .map_err(|err| SpotifyError::SpClient(err.to_string()))?;
        Ok(())
    }

    /// Fetches the current active Spotify Jam session, returning `None` when none is active (404).
    pub async fn current_jam(&self, session: &Session) -> Result<Option<JamSession>, SpotifyError> {
        match session
            .spclient()
            .request(
                &http::Method::GET,
                "/social-connect/v2/sessions/current?alt=protobuf",
                None,
                None,
            )
            .await
        {
            Ok(bytes) => parse_jam_session_proto(&bytes).map(Some),
            Err(err) if err.kind == ErrorKind::NotFound => Ok(None),
            Err(err) => Err(SpotifyError::SpClient(err.to_string())),
        }
    }

    /// Creates (or returns the active) Spotify Jam session natively via `spclient`.
    pub async fn create_jam(&self, session: &Session) -> Result<JamSession, SpotifyError> {
        let bytes = session
            .spclient()
            .request(
                &http::Method::GET,
                "/social-connect/v2/sessions/current_or_new?activate=true&alt=protobuf",
                None,
                None,
            )
            .await
            .map_err(|err| SpotifyError::SpClient(err.to_string()))?;
        parse_jam_session_proto(&bytes)
    }

    /// Joins a Spotify Jam session using a share URL, URI, or join token.
    pub async fn join_jam(
        &self,
        session: &Session,
        input: &str,
    ) -> Result<JamSession, SpotifyError> {
        let token = extract_jam_token(input)?;
        let endpoint = format!("/social-connect/v2/sessions/join/{token}?alt=protobuf");
        let bytes = session
            .spclient()
            .request(&http::Method::POST, &endpoint, None, None)
            .await
            .map_err(|err| SpotifyError::SpClient(err.to_string()))?;
        parse_jam_session_proto(&bytes)
    }

    /// Leaves (or ends, if `is_owner`) the Spotify Jam session identified by `session_id`.
    pub async fn leave_jam(
        &self,
        session: &Session,
        session_id: &str,
        is_owner: bool,
    ) -> Result<(), SpotifyError> {
        let (method, endpoint) = if is_owner {
            (
                http::Method::DELETE,
                format!("/social-connect/v2/sessions/{session_id}"),
            )
        } else {
            (
                http::Method::POST,
                format!("/social-connect/v2/sessions/{session_id}/leave"),
            )
        };
        session
            .spclient()
            .request(&method, &endpoint, None, None)
            .await
            .map_err(|err| SpotifyError::SpClient(err.to_string()))?;
        Ok(())
    }

    /// Fetches the tracks belonging to Liked Songs, a playlist, or an album via `spclient`.
    pub async fn album_tracks(
        &self,
        session: &Session,
        album: &Album,
    ) -> Result<Vec<Track>, SpotifyError> {
        let track_uris = if album.id.as_str() == LIKED_SONGS_ID {
            let response_body = self.fetch_collection_paging(session).await?;
            parse_collection_track_uris(&response_body)?
        } else if let Some(pl_base62) = album.id.as_str().strip_prefix(PLAYLIST_ID_PREFIX) {
            let playlist_id =
                SpotifyId::from_base62(pl_base62).map_err(|err| SpotifyError::InvalidUri {
                    uri: format!("spotify:playlist:{pl_base62}"),
                    reason: err.to_string(),
                })?;
            let playlist_bytes = session
                .spclient()
                .get_playlist(&playlist_id)
                .await
                .map_err(|err| SpotifyError::SpClient(err.to_string()))?;
            parse_playlist_track_uris_proto(&playlist_bytes)?
        } else {
            let raw_uri = format!("spotify:album:{}", album.id.as_str());
            let album_uri =
                SpotifyUri::from_uri(&raw_uri).map_err(|err| SpotifyError::InvalidUri {
                    uri: raw_uri,
                    reason: err.to_string(),
                })?;

            let album_bytes = session
                .spclient()
                .get_album_metadata(&album_uri)
                .await
                .map_err(|err| SpotifyError::SpClient(err.to_string()))?;

            extract_album_track_uris(&album_bytes)?
        };

        self.fetch_tracks_by_uris(session, &track_uris, &album.name)
            .await
    }

    /// Fetches Spotify's generated Track Radio tracks for `track`, excluding `track` itself.
    pub async fn track_radio(
        &self,
        session: &Session,
        track: &Track,
    ) -> Result<Vec<Track>, SpotifyError> {
        let seed_uri = format!("spotify:track:{}", track.id.as_str());
        let track_uri =
            SpotifyUri::from_uri(&seed_uri).map_err(|err| SpotifyError::InvalidUri {
                uri: seed_uri.clone(),
                reason: err.to_string(),
            })?;

        let response_bytes = session
            .spclient()
            .get_radio_for_track(&track_uri)
            .await
            .map_err(|err| SpotifyError::SpClient(err.to_string()))?;
        let body = std::str::from_utf8(&response_bytes).map_err(|err| {
            SpotifyError::SpClient(format!("invalid UTF-8 in radio response: {err}"))
        })?;

        let Some(playlist_uri) = parse_radio_playlist_uri(body)? else {
            return Ok(Vec::new());
        };
        let Some(pl_base62) = playlist_uri.strip_prefix("spotify:playlist:") else {
            return Ok(Vec::new());
        };
        let playlist_id =
            SpotifyId::from_base62(pl_base62).map_err(|err| SpotifyError::InvalidUri {
                uri: playlist_uri.clone(),
                reason: err.to_string(),
            })?;
        let playlist_bytes = session
            .spclient()
            .get_playlist(&playlist_id)
            .await
            .map_err(|err| SpotifyError::SpClient(err.to_string()))?;

        let track_uris: Vec<String> = parse_playlist_track_uris_proto(&playlist_bytes)?
            .into_iter()
            .filter(|uri| uri != &seed_uri)
            .collect();
        self.fetch_tracks_by_uris(session, &track_uris, "").await
    }

    async fn fetch_collection_paging(&self, session: &Session) -> Result<String, SpotifyError> {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );
        let request_body = serde_json::json!({
            "username": session.username(),
            "set": "collection",
            "limit": COLLECTION_PAGE_LIMIT
        })
        .to_string();

        let response_bytes = session
            .spclient()
            .request_as_json(
                &http::Method::POST,
                "/collection/v2/paging",
                Some(headers),
                Some(&request_body),
            )
            .await
            .map_err(|err| SpotifyError::SpClient(err.to_string()))?;

        std::str::from_utf8(&response_bytes)
            .map(ToOwned::to_owned)
            .map_err(|err| SpotifyError::SpClient(format!("invalid UTF-8 in collection: {err}")))
    }

    async fn fetch_tracks_by_uris(
        &self,
        session: &Session,
        track_uris: &[String],
        fallback_album_name: &str,
    ) -> Result<Vec<Track>, SpotifyError> {
        if track_uris.is_empty() {
            return Ok(Vec::new());
        }

        let mut tracks = Vec::with_capacity(track_uris.len());
        for chunk in track_uris.chunks(METADATA_BATCH_SIZE) {
            let mut metadata_by_uri =
                fetch_extended_metadata_batch(session, chunk, ExtensionKind::TRACK_V4).await?;
            for uri in chunk {
                if let Some(bytes) = metadata_by_uri.remove(uri) {
                    tracks.push(parse_track_metadata_proto(&bytes, fallback_album_name)?);
                }
            }
        }
        Ok(tracks)
    }
}

async fn fetch_extended_metadata_batch(
    session: &Session,
    uris: &[String],
    kind: ExtensionKind,
) -> Result<HashMap<String, Vec<u8>>, SpotifyError> {
    let entity_request = uris
        .iter()
        .map(|uri| EntityRequest {
            entity_uri: uri.clone(),
            query: vec![ExtensionQuery {
                extension_kind: EnumOrUnknown::new(kind),
                ..Default::default()
            }],
            ..Default::default()
        })
        .collect();

    let req = BatchedEntityRequest {
        entity_request,
        ..Default::default()
    };

    let resp = session
        .spclient()
        .get_extended_metadata(req)
        .await
        .map_err(|err| SpotifyError::SpClient(err.to_string()))?;

    let mut result = HashMap::new();
    for arr in resp.extended_metadata {
        for entry in arr.extension_data {
            if let Some(any) = entry.extension_data.into_option()
                && !any.value.is_empty()
            {
                result.insert(entry.entity_uri, any.value);
            }
        }
    }
    Ok(result)
}

async fn read_success_body(response: reqwest::Response) -> Result<String, SpotifyError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response.text().await?);
    }

    if status == StatusCode::TOO_MANY_REQUESTS {
        let retry_after_secs = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|val| val.to_str().ok())
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(DEFAULT_RETRY_AFTER_SECS);
        return Err(SpotifyError::RateLimited { retry_after_secs });
    }

    let body = response.text().await.unwrap_or_default();
    let message = serde_json::from_str::<ApiErrorEnvelope>(&body)
        .ok()
        .map(|env| env.error.message)
        .filter(|msg| !msg.trim().is_empty())
        .unwrap_or_else(|| {
            status
                .canonical_reason()
                .unwrap_or("Spotify API request failed")
                .to_string()
        });

    Err(SpotifyError::Api {
        status: status.as_u16(),
        message,
    })
}

#[derive(Debug, Deserialize)]
struct ApiErrorEnvelope {
    error: ApiErrorBody,
}

#[derive(Debug, Deserialize)]
struct ApiErrorBody {
    message: String,
}

#[derive(Debug, Deserialize)]
struct PathfinderSearchResponse {
    data: Option<PathfinderData>,
}

#[derive(Debug, Deserialize)]
struct PathfinderData {
    search: Option<PathfinderSearch>,
}

#[derive(Debug, Deserialize)]
struct PathfinderSearch {
    tracks: Option<PathfinderItems<PathfinderTrackItem>>,
    albums: Option<PathfinderItems<PathfinderAlbum>>,
}

#[derive(Debug, Deserialize)]
struct PathfinderItems<T> {
    #[serde(default = "Vec::new")]
    items: Vec<Option<T>>,
}

#[derive(Debug, Deserialize)]
struct PathfinderTrackItem {
    track: Option<PathfinderTrack>,
}

#[derive(Debug, Deserialize)]
struct PathfinderTrack {
    id: Option<String>,
    uri: Option<String>,
    name: String,
    artists: Option<PathfinderArtists>,
    album: Option<PathfinderTrackAlbum>,
    duration: Option<PathfinderDuration>,
}

#[derive(Debug, Deserialize)]
struct PathfinderArtists {
    #[serde(default)]
    items: Vec<PathfinderArtistItem>,
}

#[derive(Debug, Deserialize)]
struct PathfinderArtistItem {
    profile: Option<PathfinderArtistProfile>,
}

#[derive(Debug, Deserialize)]
struct PathfinderArtistProfile {
    name: String,
}

#[derive(Debug, Deserialize)]
struct PathfinderTrackAlbum {
    name: String,
}

#[derive(Debug, Deserialize)]
struct PathfinderDuration {
    #[serde(rename = "totalMilliseconds")]
    total_milliseconds: u64,
}

#[derive(Debug, Deserialize)]
struct PathfinderAlbum {
    uri: String,
    name: String,
    artists: Option<PathfinderArtists>,
    date: Option<PathfinderDate>,
}

#[derive(Debug, Deserialize)]
struct PathfinderDate {
    year: Option<u16>,
}

#[derive(Debug, Deserialize)]
struct CollectionPagingResponse {
    #[serde(default)]
    items: Vec<CollectionItem>,
}

#[derive(Debug, Deserialize)]
struct CollectionItem {
    uri: String,
    #[serde(default)]
    is_removed: bool,
}

fn extract_pathfinder_artists(artists: Option<PathfinderArtists>) -> Vec<String> {
    artists
        .map(|a| {
            a.items
                .into_iter()
                .filter_map(|item| item.profile.map(|p| p.name))
                .collect()
        })
        .unwrap_or_default()
}

fn gid_to_base62(gid: &[u8]) -> Result<String, SpotifyError> {
    let spotify_id = SpotifyId::from_raw(gid)
        .map_err(|err| SpotifyError::SpClient(format!("invalid Spotify GID: {err}")))?;
    spotify_id
        .to_base62()
        .map_err(|err| SpotifyError::SpClient(format!("failed to encode Spotify ID: {err}")))
}

/// Deserializes a Spotify desktop Pathfinder `searchDesktop` JSON response into `SearchResults`.
pub fn parse_pathfinder_search_response(body: &str) -> Result<SearchResults, SpotifyError> {
    let parsed: PathfinderSearchResponse = serde_json::from_str(body)?;
    let Some(search) = parsed.data.and_then(|d| d.search) else {
        return Ok(SearchResults {
            tracks: Vec::new(),
            albums: Vec::new(),
        });
    };

    let mut tracks = Vec::new();
    if let Some(track_page) = search.tracks {
        for item in track_page.items.into_iter().flatten() {
            let Some(track) = item.track else {
                continue;
            };
            let raw_id = match track.id {
                Some(id) => id,
                None => track
                    .uri
                    .as_deref()
                    .and_then(|u| u.strip_prefix("spotify:track:"))
                    .unwrap_or_default()
                    .to_string(),
            };
            tracks.push(Track {
                id: TrackId::new(raw_id)?,
                title: track.name,
                artists: extract_pathfinder_artists(track.artists),
                album_name: track.album.map(|a| a.name).unwrap_or_default(),
                duration: Duration::from_millis(track.duration.map_or(0, |d| d.total_milliseconds)),
            });
        }
    }

    let mut albums = Vec::new();
    if let Some(album_page) = search.albums {
        for album in album_page.items.into_iter().flatten() {
            let raw_id = album
                .uri
                .strip_prefix("spotify:album:")
                .unwrap_or(&album.uri)
                .to_string();
            albums.push(Album {
                id: AlbumId::new(raw_id)?,
                name: album.name,
                artists: extract_pathfinder_artists(album.artists),
                release_year: album.date.and_then(|d| d.year),
                total_tracks: 0,
            });
        }
    }

    Ok(SearchResults { tracks, albums })
}

fn parse_collection_uris(
    body: &str,
    prefix: &str,
    limit: usize,
) -> Result<Vec<String>, SpotifyError> {
    let parsed: CollectionPagingResponse = serde_json::from_str(body)?;
    Ok(parsed
        .items
        .into_iter()
        .filter(|item| !item.is_removed && item.uri.starts_with(prefix))
        .map(|item| item.uri)
        .take(limit)
        .collect())
}

/// Extracts active `spotify:album:` URIs from a `spclient` `/collection/v2/paging` JSON response.
pub fn parse_collection_album_uris(body: &str) -> Result<Vec<String>, SpotifyError> {
    parse_collection_uris(body, "spotify:album:", SAVED_ALBUMS_LIMIT)
}

/// Extracts active `spotify:track:` URIs from a `spclient` `/collection/v2/paging` JSON response.
pub fn parse_collection_track_uris(body: &str) -> Result<Vec<String>, SpotifyError> {
    parse_collection_uris(body, "spotify:track:", PLAYLIST_TRACKS_LIMIT)
}

fn hex_digit_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn decode_folder_name(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
        } else if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) =
                (hex_digit_value(bytes[i + 1]), hex_digit_value(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    let decoded = String::from_utf8_lossy(&out);
    let trimmed = decoded.trim();
    if trimmed.is_empty() {
        "Folder".to_string()
    } else {
        trimmed.to_string()
    }
}

fn push_library_item(
    root: &mut Vec<LibraryItem>,
    stack: &mut [(String, Vec<LibraryItem>)],
    item: LibraryItem,
) {
    stack
        .last_mut()
        .map(|(_, items)| items)
        .unwrap_or(root)
        .push(item);
}

/// Returns `true` when `s` looks like an opaque Spotify internal user ID (20+ ASCII alphanumeric chars).
pub fn is_opaque_spotify_id(s: &str) -> bool {
    let trimmed = s.trim();
    trimmed.len() >= 20 && trimmed.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Decodes a `playlist4_external::SelectedListContent` rootlist protobuf into a tree of
/// `LibraryItem` entries, preserving nested `spotify:start-group` / `spotify:end-group` folders
/// and hiding opaque Spotify owner IDs or `current_username`.
pub fn parse_rootlist_items_proto(
    bytes: &[u8],
    current_username: &str,
) -> Result<Vec<LibraryItem>, SpotifyError> {
    let proto = SelectedListContent::parse_from_bytes(bytes)?;
    let mut root = Vec::new();
    let mut stack: Vec<(String, Vec<LibraryItem>)> = Vec::new();

    for (idx, item) in proto.contents.items.iter().enumerate() {
        let uri = item.uri();
        if let Some(rest) = uri.strip_prefix("spotify:start-group:") {
            let encoded_name = rest.split_once(':').map(|(_, n)| n).unwrap_or("");
            stack.push((decode_folder_name(encoded_name), Vec::new()));
            continue;
        }
        if uri.starts_with("spotify:end-group:") {
            if let Some((name, items)) = stack.pop() {
                push_library_item(&mut root, &mut stack, LibraryItem::Folder { name, items });
            }
            continue;
        }

        let Some(pl_id) = uri.strip_prefix("spotify:playlist:") else {
            continue;
        };
        if pl_id.is_empty() {
            continue;
        }
        let meta = proto.contents.meta_items.get(idx);
        let attrs = meta.and_then(|m| m.attributes.as_ref());
        if attrs.is_some_and(|a| a.deleted_by_owner()) {
            continue;
        }
        let name = attrs
            .map(|a| a.name().trim())
            .filter(|s| !s.is_empty())
            .unwrap_or("Playlist")
            .to_string();
        let owner = meta.map(|m| m.owner_username().trim()).unwrap_or("");
        let artists = if owner.is_empty()
            || (!current_username.is_empty() && owner == current_username)
            || is_opaque_spotify_id(owner)
        {
            Vec::new()
        } else {
            vec![owner.to_string()]
        };
        let total_tracks = meta
            .and_then(|m| u32::try_from(m.length()).ok())
            .unwrap_or(0);

        push_library_item(
            &mut root,
            &mut stack,
            LibraryItem::Collection(Album {
                id: AlbumId::new(format!("{PLAYLIST_ID_PREFIX}{pl_id}"))?,
                name,
                artists,
                release_year: None,
                total_tracks,
            }),
        );
    }

    while let Some((name, items)) = stack.pop() {
        push_library_item(&mut root, &mut stack, LibraryItem::Folder { name, items });
    }

    Ok(root)
}

/// Builds the JSON payload for `POST /collection/v2/write`.
pub fn build_collection_write_payload(
    username: &str,
    track_id: &str,
    liked: bool,
    timestamp_secs: u64,
) -> String {
    serde_json::json!({
        "username": username,
        "set": "collection",
        "items": [
            {
                "uri": format!("spotify:track:{track_id}"),
                "added_at": timestamp_secs,
                "is_removed": !liked
            }
        ]
    })
    .to_string()
}

/// Builds a `playlist4_external::ListChanges` protobuf to create a playlist named `name`.
pub fn build_create_playlist_changes(name: &str) -> ListChanges {
    let mut attrs = ListAttributes::new();
    attrs.set_name(name.to_string());

    let mut partial = ListAttributesPartialState::new();
    partial.values = MessageField::some(attrs);

    let mut update_attrs = UpdateListAttributes::new();
    update_attrs.new_attributes = MessageField::some(partial);

    let mut op = Op::new();
    op.set_kind(OpKind::UPDATE_LIST_ATTRIBUTES);
    op.update_list_attributes = MessageField::some(update_attrs);

    let mut delta = Delta::new();
    delta.ops = vec![op];

    let mut changes = ListChanges::new();
    changes.deltas = vec![delta];
    changes
}

/// Builds a `playlist4_external::ListChanges` protobuf to add `uri` at the start or end.
pub fn build_add_item_changes(uri: &str, add_first: bool) -> ListChanges {
    let mut item = Item::new();
    item.set_uri(uri.to_string());

    let mut add = Add::new();
    add.items = vec![item];
    if add_first {
        add.set_add_first(true);
    } else {
        add.set_add_last(true);
    }

    let mut op = Op::new();
    op.set_kind(OpKind::ADD);
    op.add = MessageField::some(add);

    let mut delta = Delta::new();
    delta.ops = vec![op];

    let mut changes = ListChanges::new();
    changes.deltas = vec![delta];
    changes
}

/// Builds a `playlist4_external::ListChanges` protobuf to remove `uri`.
pub fn build_remove_item_changes(uri: &str) -> ListChanges {
    let mut item = Item::new();
    item.set_uri(uri.to_string());

    let mut rem = Rem::new();
    rem.items = vec![item];
    rem.set_items_as_key(true);

    let mut op = Op::new();
    op.set_kind(OpKind::REM);
    op.rem = MessageField::some(rem);

    let mut delta = Delta::new();
    delta.ops = vec![op];

    let mut changes = ListChanges::new();
    changes.deltas = vec![delta];
    changes
}

/// Extracts the base62 playlist ID from a `playlist4_external::CreateListReply` protobuf.
pub fn parse_create_list_reply(bytes: &[u8]) -> Result<String, SpotifyError> {
    let reply = CreateListReply::parse_from_bytes(bytes)?;
    let uri = reply.uri();
    let base62 = uri
        .strip_prefix("spotify:playlist:")
        .filter(|id| !id.is_empty())
        .ok_or_else(|| SpotifyError::InvalidUri {
            uri: uri.to_string(),
            reason: "expected spotify:playlist:<id>".to_string(),
        })?;
    Ok(base62.to_string())
}

/// Decodes a `social_connect_v2::Session` protobuf into a domain `JamSession`.
pub fn parse_jam_session_proto(bytes: &[u8]) -> Result<JamSession, SpotifyError> {
    let proto = ProtoJamSession::parse_from_bytes(bytes)?;
    let members = proto
        .session_members
        .into_iter()
        .map(|member| JamMember {
            username: member.username,
            display_name: member.display_name,
            is_listening: member.is_listening,
            is_controlling: member.is_controlling,
        })
        .collect();
    let raw_url = proto.join_session_url.trim();
    let join_url = if !raw_url.is_empty() && !raw_url.starts_with("hm://") {
        raw_url.to_string()
    } else if !proto.join_session_token.trim().is_empty() {
        format!(
            "https://open.spotify.com/socialsession/{}",
            proto.join_session_token.trim()
        )
    } else {
        raw_url.to_string()
    };
    Ok(JamSession {
        session_id: proto.session_id,
        join_token: proto.join_session_token,
        join_url,
        owner_id: proto.session_owner_id,
        is_owner: proto.is_session_owner,
        is_listening: proto.is_listening,
        is_controlling: proto.is_controlling,
        members,
    })
}

/// Extracts a Spotify Jam join token from a raw token, `https://...` share URL, or `spotify:` URI.
pub fn extract_jam_token(input: &str) -> Result<String, SpotifyError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(SpotifyError::InvalidUri {
            uri: input.to_string(),
            reason: "Jam token or URL must not be empty".to_string(),
        });
    }
    let candidate = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        let without_query = trimmed.split(['?', '#']).next().unwrap_or(trimmed);
        without_query
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("")
    } else if trimmed.starts_with("spotify:") {
        trimmed.rsplit(':').next().unwrap_or("")
    } else {
        trimmed
    };

    if candidate.is_empty() || candidate.chars().any(char::is_whitespace) {
        return Err(SpotifyError::InvalidUri {
            uri: input.to_string(),
            reason: "invalid Jam token or URL".to_string(),
        });
    }
    Ok(candidate.to_string())
}

/// Extracts `spotify:track:` URIs from a `playlist4_external::SelectedListContent` protobuf message.
pub fn parse_playlist_track_uris_proto(bytes: &[u8]) -> Result<Vec<String>, SpotifyError> {
    let proto = SelectedListContent::parse_from_bytes(bytes)?;
    Ok(proto
        .contents
        .items
        .iter()
        .map(|item| item.uri().to_string())
        .filter(|uri| uri.starts_with("spotify:track:"))
        .take(PLAYLIST_TRACKS_LIMIT)
        .collect())
}

#[derive(Debug, Deserialize)]
struct RadioResponse {
    #[serde(rename = "mediaItems", default)]
    media_items: Vec<Option<RadioMediaItem>>,
}

#[derive(Debug, Deserialize)]
struct RadioMediaItem {
    #[serde(default)]
    uri: Option<String>,
}

/// Extracts the first `spotify:playlist:` URI from a `spclient` `get_radio_for_track` JSON response.
pub fn parse_radio_playlist_uri(body: &str) -> Result<Option<String>, SpotifyError> {
    let parsed: RadioResponse = serde_json::from_str(body)?;
    Ok(parsed
        .media_items
        .into_iter()
        .flatten()
        .filter_map(|item| item.uri)
        .find(|uri| uri.starts_with("spotify:playlist:")))
}

/// Decodes a `librespot_protocol::metadata::Album` protobuf message into a domain `Album`.
pub fn parse_album_metadata_proto(bytes: &[u8]) -> Result<Album, SpotifyError> {
    let proto = proto_metadata::Album::parse_from_bytes(bytes)?;
    let id = AlbumId::new(gid_to_base62(proto.gid())?)?;
    let artists = proto
        .artist
        .iter()
        .map(|artist| artist.name().to_string())
        .collect();
    let release_year = proto
        .date
        .as_ref()
        .and_then(|date| u16::try_from(date.year()).ok())
        .filter(|&year| year > 0);
    let total_tracks = proto.disc.iter().map(|disc| disc.track.len() as u32).sum();

    Ok(Album {
        id,
        name: proto.name().to_string(),
        artists,
        release_year,
        total_tracks,
    })
}

/// Extracts ordered `spotify:track:` URIs across discs from a `metadata::Album` protobuf message.
pub fn extract_album_track_uris(bytes: &[u8]) -> Result<Vec<String>, SpotifyError> {
    let proto = proto_metadata::Album::parse_from_bytes(bytes)?;
    let mut uris = Vec::new();
    for disc in &proto.disc {
        for track in &disc.track {
            let base62 = gid_to_base62(track.gid())?;
            uris.push(format!("spotify:track:{base62}"));
        }
    }
    Ok(uris)
}

/// Decodes a `librespot_protocol::metadata::Track` protobuf message into a domain `Track`.
pub fn parse_track_metadata_proto(
    bytes: &[u8],
    fallback_album_name: &str,
) -> Result<Track, SpotifyError> {
    let proto = proto_metadata::Track::parse_from_bytes(bytes)?;
    let id = TrackId::new(gid_to_base62(proto.gid())?)?;
    let artists = proto
        .artist
        .iter()
        .map(|artist| artist.name().to_string())
        .collect();
    let album_name = if proto.album.name().is_empty() {
        fallback_album_name.to_string()
    } else {
        proto.album.name().to_string()
    };
    let duration = Duration::from_millis(u64::try_from(proto.duration()).unwrap_or(0));

    Ok(Track {
        id,
        title: proto.name().to_string(),
        artists,
        album_name,
        duration,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use protobuf::MessageField;

    #[test]
    fn parse_pathfinder_search_response_maps_tracks_and_albums_and_skips_nulls() {
        let payload = r#"{
            "data": {
                "search": {
                    "tracks": {
                        "items": [
                            null,
                            { "track": null },
                            {
                                "track": {
                                    "id": "trk_1",
                                    "uri": "spotify:track:trk_1",
                                    "name": "Kreutzer Sonata: Presto",
                                    "artists": {
                                        "items": [
                                            {"profile": {"name": "Beethoven"}},
                                            {"profile": {"name": "Martha Argerich"}}
                                        ]
                                    },
                                    "album": {"name": "Violin Sonatas"},
                                    "duration": {"totalMilliseconds": 245000}
                                }
                            },
                            {
                                "track": {
                                    "uri": "spotify:track:trk_from_uri",
                                    "name": "Spring Sonata",
                                    "artists": {"items": [{"profile": {"name": "Beethoven"}}]},
                                    "album": {"name": "Violin Sonatas"},
                                    "duration": {"totalMilliseconds": 180000}
                                }
                            }
                        ]
                    },
                    "albums": {
                        "items": [
                            {
                                "uri": "spotify:album:alb_1",
                                "name": "Violin Sonatas",
                                "artists": {"items": [{"profile": {"name": "Beethoven"}}]},
                                "date": {"year": 2001}
                            },
                            null
                        ]
                    }
                }
            }
        }"#;

        let results = parse_pathfinder_search_response(payload).unwrap();
        assert_eq!(results.tracks.len(), 2);
        assert_eq!(results.tracks[0].id.as_str(), "trk_1");
        assert_eq!(results.tracks[0].title, "Kreutzer Sonata: Presto");
        assert_eq!(
            results.tracks[0].artists_display(),
            "Beethoven, Martha Argerich"
        );
        assert_eq!(results.tracks[0].album_name, "Violin Sonatas");
        assert_eq!(results.tracks[0].duration, Duration::from_millis(245_000));
        assert_eq!(results.tracks[1].id.as_str(), "trk_from_uri");

        assert_eq!(results.albums.len(), 1);
        assert_eq!(results.albums[0].id.as_str(), "alb_1");
        assert_eq!(results.albums[0].release_year, Some(2001));
        assert_eq!(results.albums[0].total_tracks, 0);
    }

    #[test]
    fn parse_pathfinder_search_response_handles_empty_results() {
        let payload = r#"{"data": {"search": {"tracks": {"items": []}, "albums": {"items": []}}}}"#;
        let results = parse_pathfinder_search_response(payload).unwrap();
        assert!(results.tracks.is_empty());
        assert!(results.albums.is_empty());
    }

    #[test]
    fn parse_pathfinder_search_response_propagates_empty_id_domain_error() {
        let payload = r#"{
            "data": {
                "search": {
                    "tracks": {
                        "items": [
                            {
                                "track": {
                                    "id": "   ",
                                    "name": "Bad Track",
                                    "artists": {"items": []},
                                    "album": {"name": "Album"},
                                    "duration": {"totalMilliseconds": 1000}
                                }
                            }
                        ]
                    }
                }
            }
        }"#;

        let err = parse_pathfinder_search_response(payload).unwrap_err();
        assert!(matches!(
            err,
            SpotifyError::Domain(DomainError::EmptyId { field: "track_id" })
        ));
    }

    #[test]
    fn parse_collection_album_uris_filters_removed_non_albums_and_limits() {
        let mut items = vec![
            r#"{"uri": "spotify:track:ignored1", "is_removed": false}"#.to_string(),
            r#"{"uri": "spotify:album:removed", "is_removed": true}"#.to_string(),
        ];
        for i in 0..60 {
            items.push(format!(r#"{{"uri": "spotify:album:alb_{i:02}"}}"#));
        }
        let payload = format!(r#"{{"items": [{}]}}"#, items.join(","));

        let uris = parse_collection_album_uris(&payload).unwrap();
        assert_eq!(uris.len(), SAVED_ALBUMS_LIMIT);
        assert_eq!(uris[0], "spotify:album:alb_00");
        assert_eq!(uris[49], "spotify:album:alb_49");
    }

    #[test]
    fn parse_album_metadata_proto_and_extract_album_track_uris() {
        let album_id = SpotifyId::from_base62("4aawyAB9vmqN3uQ7FjRGTy").unwrap();
        let track1_id = SpotifyId::from_base62("11dFghVXANMlKmJXsNCbNl").unwrap();
        let track2_id = SpotifyId::from_base62("6rqhFgbbKwnb9MLmUQDhG6").unwrap();

        let mut album = proto_metadata::Album::new();
        album.set_gid(album_id.to_raw().to_vec());
        album.set_name("Late Quartets".to_string());

        let mut artist = proto_metadata::Artist::new();
        artist.set_name("Takacs Quartet".to_string());
        album.artist.push(artist);

        let mut date = proto_metadata::Date::new();
        date.set_year(2005);
        album.date = MessageField::some(date);

        let mut trk1 = proto_metadata::Track::new();
        trk1.set_gid(track1_id.to_raw().to_vec());
        let mut trk2 = proto_metadata::Track::new();
        trk2.set_gid(track2_id.to_raw().to_vec());

        let mut disc1 = proto_metadata::Disc::new();
        disc1.track.push(trk1);
        let mut disc2 = proto_metadata::Disc::new();
        disc2.track.push(trk2);
        album.disc.push(disc1);
        album.disc.push(disc2);

        let bytes = album.write_to_bytes().unwrap();

        let parsed = parse_album_metadata_proto(&bytes).unwrap();
        assert_eq!(parsed.id.as_str(), "4aawyAB9vmqN3uQ7FjRGTy");
        assert_eq!(parsed.name, "Late Quartets");
        assert_eq!(parsed.artists, vec!["Takacs Quartet".to_string()]);
        assert_eq!(parsed.release_year, Some(2005));
        assert_eq!(parsed.total_tracks, 2);

        let uris = extract_album_track_uris(&bytes).unwrap();
        assert_eq!(
            uris,
            vec![
                "spotify:track:11dFghVXANMlKmJXsNCbNl",
                "spotify:track:6rqhFgbbKwnb9MLmUQDhG6"
            ]
        );
    }

    #[test]
    fn parse_track_metadata_proto_uses_album_name_or_fallback() {
        let track_id = SpotifyId::from_base62("11dFghVXANMlKmJXsNCbNl").unwrap();

        let mut track = proto_metadata::Track::new();
        track.set_gid(track_id.to_raw().to_vec());
        track.set_name("I. Adagio sostenuto".to_string());
        track.set_duration(310_000);

        let mut artist = proto_metadata::Artist::new();
        artist.set_name("Beethoven".to_string());
        track.artist.push(artist);

        let bytes_without_album = track.write_to_bytes().unwrap();
        let parsed_fallback =
            parse_track_metadata_proto(&bytes_without_album, "Moonlight Sonata").unwrap();
        assert_eq!(parsed_fallback.id.as_str(), "11dFghVXANMlKmJXsNCbNl");
        assert_eq!(parsed_fallback.title, "I. Adagio sostenuto");
        assert_eq!(parsed_fallback.artists, vec!["Beethoven".to_string()]);
        assert_eq!(parsed_fallback.album_name, "Moonlight Sonata");
        assert_eq!(parsed_fallback.duration, Duration::from_secs(310));

        let mut album = proto_metadata::Album::new();
        album.set_name("Piano Sonatas".to_string());
        track.album = MessageField::some(album);
        let bytes_with_album = track.write_to_bytes().unwrap();
        let parsed_with_album =
            parse_track_metadata_proto(&bytes_with_album, "Moonlight Sonata").unwrap();
        assert_eq!(parsed_with_album.album_name, "Piano Sonatas");
    }

    #[test]
    fn parse_collection_track_uris_filters_removed_non_tracks_and_limits() {
        let mut items = vec![
            r#"{"uri": "spotify:album:ignored", "is_removed": false}"#.to_string(),
            r#"{"uri": "spotify:track:removed", "is_removed": true}"#.to_string(),
        ];
        for i in 0..120 {
            items.push(format!(r#"{{"uri": "spotify:track:trk_{i:03}"}}"#));
        }
        let payload = format!(r#"{{"items": [{}]}}"#, items.join(","));

        let uris = parse_collection_track_uris(&payload).unwrap();
        assert_eq!(uris.len(), PLAYLIST_TRACKS_LIMIT);
        assert_eq!(uris[0], "spotify:track:trk_000");
        assert_eq!(uris[99], "spotify:track:trk_099");
    }

    #[test]
    fn parse_rootlist_playlists_and_playlist_tracks_proto() {
        use librespot_protocol::playlist4_external::{
            Item, ListAttributes, ListItems, MetaItem, SelectedListContent,
        };

        let mut pl1 = Item::new();
        pl1.set_uri("spotify:playlist:pl123".to_string());
        let mut pl_deleted = Item::new();
        pl_deleted.set_uri("spotify:playlist:pl_del".to_string());
        let mut non_pl = Item::new();
        non_pl.set_uri("spotify:episode:ep1".to_string());
        let mut pl2_no_meta = Item::new();
        pl2_no_meta.set_uri("spotify:playlist:pl456".to_string());

        let mut attrs1 = ListAttributes::new();
        attrs1.set_name("Violin Favorites".to_string());
        let mut meta1 = MetaItem::new();
        meta1.set_owner_username("yoshida".to_string());
        meta1.set_length(12);
        meta1.attributes = MessageField::some(attrs1);

        let mut attrs_del = ListAttributes::new();
        attrs_del.set_name("Deleted".to_string());
        attrs_del.set_deleted_by_owner(true);
        let mut meta_del = MetaItem::new();
        meta_del.attributes = MessageField::some(attrs_del);

        let mut contents = ListItems::new();
        contents.set_pos(0);
        contents.set_truncated(false);
        contents.items = vec![pl1, pl_deleted, non_pl, pl2_no_meta];
        contents.meta_items = vec![meta1, meta_del, MetaItem::new()];

        let mut rootlist = SelectedListContent::new();
        rootlist.contents = MessageField::some(contents);
        let rootlist_bytes = rootlist.write_to_bytes().unwrap();

        let items = parse_rootlist_items_proto(&rootlist_bytes, "someone_else").unwrap();
        assert_eq!(items.len(), 2);
        let LibraryItem::Collection(pl1_album) = &items[0] else {
            panic!("expected collection");
        };
        assert_eq!(pl1_album.id.as_str(), "playlist:pl123");
        assert_eq!(pl1_album.name, "Violin Favorites");
        assert_eq!(pl1_album.artists, vec!["yoshida".to_string()]);
        assert_eq!(pl1_album.total_tracks, 12);

        let LibraryItem::Collection(pl2_album) = &items[1] else {
            panic!("expected collection");
        };
        assert_eq!(pl2_album.id.as_str(), "playlist:pl456");
        assert_eq!(pl2_album.name, "Playlist");
        assert!(pl2_album.artists.is_empty());
        assert_eq!(pl2_album.total_tracks, 0);

        let mut trk1 = Item::new();
        trk1.set_uri("spotify:track:t1".to_string());
        let mut episode = Item::new();
        episode.set_uri("spotify:episode:e1".to_string());
        let mut trk2 = Item::new();
        trk2.set_uri("spotify:track:t2".to_string());

        let mut pl_contents = ListItems::new();
        pl_contents.set_pos(0);
        pl_contents.set_truncated(false);
        pl_contents.items = vec![trk1, episode, trk2];
        let mut pl_proto = SelectedListContent::new();
        pl_proto.contents = MessageField::some(pl_contents);
        let pl_bytes = pl_proto.write_to_bytes().unwrap();

        let uris = parse_playlist_track_uris_proto(&pl_bytes).unwrap();
        assert_eq!(uris, vec!["spotify:track:t1", "spotify:track:t2"]);
    }

    #[test]
    fn parse_radio_playlist_uri_extracts_playlist_and_handles_empty_or_invalid() {
        let body =
            r#"{"total": 1, "mediaItems": [{"uri": "spotify:playlist:37i9dQZF1E8DIFrhCfmG2T"}]}"#;
        assert_eq!(
            parse_radio_playlist_uri(body).unwrap().as_deref(),
            Some("spotify:playlist:37i9dQZF1E8DIFrhCfmG2T")
        );

        let mixed = r#"{"mediaItems": [null, {"uri": "spotify:track:t1"}, {"uri": "spotify:playlist:pl99"}]}"#;
        assert_eq!(
            parse_radio_playlist_uri(mixed).unwrap().as_deref(),
            Some("spotify:playlist:pl99")
        );

        assert_eq!(
            parse_radio_playlist_uri(r#"{"mediaItems": []}"#).unwrap(),
            None
        );
        assert!(parse_radio_playlist_uri("not json").is_err());
    }

    #[test]
    fn parse_rootlist_items_proto_handles_nested_folders_url_decoding_and_edges() {
        use librespot_protocol::playlist4_external::{
            Item, ListAttributes, ListItems, MetaItem, SelectedListContent,
        };

        let make_item = |uri: &str| {
            let mut item = Item::new();
            item.set_uri(uri.to_string());
            item
        };
        let make_meta = |name: &str, owner: &str, len: i32, deleted: bool| {
            let mut attrs = ListAttributes::new();
            attrs.set_name(name.to_string());
            attrs.set_deleted_by_owner(deleted);
            let mut meta = MetaItem::new();
            meta.set_owner_username(owner.to_string());
            meta.set_length(len);
            meta.attributes = MessageField::some(attrs);
            meta
        };

        let mut contents = ListItems::new();
        contents.set_pos(0);
        contents.set_truncated(false);
        contents.items = vec![
            // Stray end-group before any start-group should be ignored.
            make_item("spotify:end-group:stray0"),
            // Top-level playlist.
            make_item("spotify:playlist:pl_top"),
            // Outer folder with `+` trailing space.
            make_item("spotify:start-group:f1:Albums+"),
            make_item("spotify:playlist:pl_in_outer"),
            // Deleted playlist inside folder should be omitted.
            make_item("spotify:playlist:pl_deleted"),
            // Nested folder with %XX encoding and colon.
            make_item("spotify:start-group:f2:My%20Rock%20%26%20Jazz%3A%20Vol+1"),
            make_item("spotify:playlist:pl_nested"),
            make_item("spotify:end-group:f2"),
            make_item("spotify:end-group:f1"),
            // Unclosed trailing folder with blank name -> "Folder".
            make_item("spotify:start-group:f3:+++"),
            make_item("spotify:playlist:pl_unclosed"),
        ];
        contents.meta_items = vec![
            MetaItem::new(),
            make_meta("Top Playlist", "alice", 5, false),
            MetaItem::new(),
            make_meta("Outer Album", "alice", 10, false),
            make_meta("Gone", "alice", 1, true),
            MetaItem::new(),
            make_meta("Nested Mix", "bob", 3, false),
            MetaItem::new(),
            MetaItem::new(),
            MetaItem::new(),
            make_meta("Trailing", "alice", 2, false),
        ];

        let mut rootlist = SelectedListContent::new();
        rootlist.contents = MessageField::some(contents);
        let bytes = rootlist.write_to_bytes().unwrap();

        let items = parse_rootlist_items_proto(&bytes, "").unwrap();
        assert_eq!(items.len(), 3);

        match &items[0] {
            LibraryItem::Collection(a) => assert_eq!(a.name, "Top Playlist"),
            other => panic!("expected Collection, got {other:?}"),
        }

        match &items[1] {
            LibraryItem::Folder { name, items: outer } => {
                assert_eq!(name, "Albums");
                assert_eq!(outer.len(), 2);
                match &outer[0] {
                    LibraryItem::Collection(a) => assert_eq!(a.name, "Outer Album"),
                    other => panic!("expected Collection, got {other:?}"),
                }
                match &outer[1] {
                    LibraryItem::Folder {
                        name: inner_name,
                        items: inner,
                    } => {
                        assert_eq!(inner_name, "My Rock & Jazz: Vol 1");
                        assert_eq!(inner.len(), 1);
                        match &inner[0] {
                            LibraryItem::Collection(a) => assert_eq!(a.name, "Nested Mix"),
                            other => panic!("expected Collection, got {other:?}"),
                        }
                    }
                    other => panic!("expected inner Folder, got {other:?}"),
                }
            }
            other => panic!("expected outer Folder, got {other:?}"),
        }

        match &items[2] {
            LibraryItem::Folder { name, items: inner } => {
                assert_eq!(name, "Folder");
                assert_eq!(inner.len(), 1);
                match &inner[0] {
                    LibraryItem::Collection(a) => assert_eq!(a.name, "Trailing"),
                    other => panic!("expected Collection, got {other:?}"),
                }
            }
            other => panic!("expected unclosed Folder, got {other:?}"),
        }
    }

    #[test]
    fn parse_rootlist_hides_opaque_and_current_username_owners_but_keeps_named_curators() {
        use librespot_protocol::playlist4_external::{
            Item, ListAttributes, ListItems, MetaItem, SelectedListContent,
        };

        assert!(is_opaque_spotify_id("31ciwhlbttslyxwxknrojehrs5nu"));
        assert!(!is_opaque_spotify_id("spotify"));
        assert!(!is_opaque_spotify_id("yoshida"));
        assert!(!is_opaque_spotify_id("short_12345"));
        assert!(!is_opaque_spotify_id("has-dash-123456789012345"));

        let make_item = |uri: &str| {
            let mut item = Item::new();
            item.set_uri(uri.to_string());
            item
        };
        let make_meta = |name: &str, owner: &str, len: i32| {
            let mut attrs = ListAttributes::new();
            attrs.set_name(name.to_string());
            let mut meta = MetaItem::new();
            meta.set_owner_username(owner.to_string());
            meta.set_length(len);
            meta.attributes = MessageField::some(attrs);
            meta
        };

        let mut contents = ListItems::new();
        contents.set_pos(0);
        contents.set_truncated(false);
        contents.items = vec![
            make_item("spotify:playlist:pl_opaque"),
            make_item("spotify:playlist:pl_self"),
            make_item("spotify:playlist:pl_spotify"),
        ];
        contents.meta_items = vec![
            make_meta("Opaque Owned", "31ciwhlbttslyxwxknrojehrs5nu", 8),
            make_meta("Self Owned", "yoshida", 5),
            make_meta("Discover Weekly", "spotify", 30),
        ];

        let mut rootlist = SelectedListContent::new();
        rootlist.contents = MessageField::some(contents);
        let bytes = rootlist.write_to_bytes().unwrap();

        let items = parse_rootlist_items_proto(&bytes, "yoshida").unwrap();
        assert_eq!(items.len(), 3);
        let LibraryItem::Collection(opaque) = &items[0] else {
            panic!("expected collection");
        };
        assert!(opaque.artists.is_empty());

        let LibraryItem::Collection(self_owned) = &items[1] else {
            panic!("expected collection");
        };
        assert!(self_owned.artists.is_empty());

        let LibraryItem::Collection(curated) = &items[2] else {
            panic!("expected collection");
        };
        assert_eq!(curated.artists, vec!["spotify".to_string()]);
    }

    #[test]
    fn collection_write_and_playlist_mutation_builders_and_parsers_work() {
        let like_json = build_collection_write_payload("yoshida", "trk1", true, 1_700_000_000);
        let parsed: serde_json::Value = serde_json::from_str(&like_json).unwrap();
        assert_eq!(parsed["username"], "yoshida");
        assert_eq!(parsed["set"], "collection");
        assert_eq!(parsed["items"][0]["uri"], "spotify:track:trk1");
        assert_eq!(parsed["items"][0]["added_at"], 1_700_000_000);
        assert_eq!(parsed["items"][0]["is_removed"], false);

        let unlike_json = build_collection_write_payload("yoshida", "trk1", false, 1_700_000_001);
        let parsed_unlike: serde_json::Value = serde_json::from_str(&unlike_json).unwrap();
        assert_eq!(parsed_unlike["items"][0]["is_removed"], true);

        let create_changes = build_create_playlist_changes("Late Night Bach");
        let op = &create_changes.deltas[0].ops[0];
        assert_eq!(op.kind(), OpKind::UPDATE_LIST_ATTRIBUTES);
        assert_eq!(
            op.update_list_attributes.new_attributes.values.name(),
            "Late Night Bach"
        );

        let add_first_changes = build_add_item_changes("spotify:playlist:abc123", true);
        let add_first_op = &add_first_changes.deltas[0].ops[0];
        assert_eq!(add_first_op.kind(), OpKind::ADD);
        assert_eq!(add_first_op.add.items[0].uri(), "spotify:playlist:abc123");
        assert!(add_first_op.add.add_first());
        assert!(!add_first_op.add.has_add_last());

        let add_last_changes = build_add_item_changes("spotify:track:trk9", false);
        let add_last_op = &add_last_changes.deltas[0].ops[0];
        assert!(add_last_op.add.add_last());
        assert!(!add_last_op.add.has_add_first());

        let mut reply = CreateListReply::new();
        reply.set_uri("spotify:playlist:pl_new_123".to_string());
        let reply_bytes = reply.write_to_bytes().unwrap();
        assert_eq!(parse_create_list_reply(&reply_bytes).unwrap(), "pl_new_123");

        let mut bad_reply = CreateListReply::new();
        bad_reply.set_uri("spotify:album:not_a_playlist".to_string());
        assert!(parse_create_list_reply(&bad_reply.write_to_bytes().unwrap()).is_err());

        let rem_changes = build_remove_item_changes("spotify:track:trk9");
        let rem_op = &rem_changes.deltas[0].ops[0];
        assert_eq!(rem_op.kind(), OpKind::REM);
        assert!(rem_op.rem.items_as_key());
        assert_eq!(rem_op.rem.items.len(), 1);
        assert_eq!(rem_op.rem.items[0].uri(), "spotify:track:trk9");
    }

    #[test]
    fn jam_session_proto_and_token_extraction_work() {
        use librespot_protocol::social_connect_v2::SessionMember as ProtoJamMember;

        assert_eq!(extract_jam_token("abc123XYZ").unwrap(), "abc123XYZ");
        assert_eq!(
            extract_jam_token("https://open.spotify.com/social-connect/tok_99?si=foo#bar").unwrap(),
            "tok_99"
        );
        assert_eq!(
            extract_jam_token("https://spotify.link/jamToken1/").unwrap(),
            "jamToken1"
        );
        assert_eq!(
            extract_jam_token("spotify:social-connect:tok_uri").unwrap(),
            "tok_uri"
        );
        assert!(extract_jam_token("   ").is_err());
        assert!(extract_jam_token("has space").is_err());

        let mut member1 = ProtoJamMember::new();
        member1.username = "alice".to_string();
        member1.display_name = "Alice".to_string();
        member1.is_listening = true;
        member1.is_controlling = true;

        let mut proto = ProtoJamSession::new();
        proto.session_id = "sess_42".to_string();
        proto.join_session_token = "join_tok".to_string();
        proto.join_session_url = "https://spotify.link/join_tok".to_string();
        proto.session_owner_id = "alice".to_string();
        proto.is_session_owner = true;
        proto.is_listening = true;
        proto.is_controlling = true;
        proto.session_members = vec![member1];

        let bytes = proto.write_to_bytes().unwrap();
        let jam = parse_jam_session_proto(&bytes).unwrap();
        assert_eq!(jam.session_id, "sess_42");
        assert_eq!(jam.join_token, "join_tok");
        assert_eq!(jam.join_url, "https://spotify.link/join_tok");
        assert_eq!(jam.owner_id, "alice");
        assert!(jam.is_owner);
        assert!(jam.is_listening);
        assert!(jam.is_controlling);
        assert_eq!(jam.members.len(), 1);
        assert_eq!(jam.members[0].display_name, "Alice");

        let mut hm_proto = ProtoJamSession::new();
        hm_proto.session_id = "sess_hm".to_string();
        hm_proto.join_session_token = "1iRKwkEe7Z7m2Ktex8LoXz".to_string();
        hm_proto.join_session_url =
            "hm://social-connect/v2/sessions/join/1iRKwkEe7Z7m2Ktex8LoXz".to_string();
        let hm_bytes = hm_proto.write_to_bytes().unwrap();
        let hm_jam = parse_jam_session_proto(&hm_bytes).unwrap();
        assert_eq!(
            hm_jam.join_url,
            "https://open.spotify.com/socialsession/1iRKwkEe7Z7m2Ktex8LoXz"
        );
    }
}
