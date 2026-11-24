use std::time::Duration;

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table,
        TableState, Tabs, Wrap,
    },
};

use crate::{
    app::{AppState, InputMode, NowPlaying, PendingDelete, QueueContext, SearchSection, View},
    art::{self, ArtProtocol},
    domain::{Album, LibraryItem, PlaybackStatus, Track},
};

/// Formats a `Duration` as `MM:SS` (minutes may exceed two digits for long tracks).
pub fn format_duration(duration: Duration) -> String {
    let total_secs = duration.as_secs();
    let minutes = total_secs / 60;
    let seconds = total_secs % 60;
    format!("{minutes:02}:{seconds:02}")
}

fn rounded_block<'a>(title: impl Into<Line<'a>>) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(title)
}

/// Terminal cell rectangles reserved for Kitty Graphics Protocol overlays.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OverlayRects {
    pub art_rect: Option<Rect>,
    pub bar_rect: Option<Rect>,
}

/// Renders the entire `AppState` into the given `ratatui::Frame`.
pub fn render(frame: &mut Frame, state: &AppState) {
    let _ = render_with_protocol(frame, state, ArtProtocol::HalfBlock, ArtProtocol::HalfBlock);
}

/// Renders `AppState` using `art_protocol` for cover art and `bar_protocol` for the progress bar.
pub fn render_with_protocol(
    frame: &mut Frame,
    state: &AppState,
    art_protocol: ArtProtocol,
    bar_protocol: ArtProtocol,
) -> OverlayRects {
    if state.focus_mode {
        return render_focus_view(frame, state, art_protocol, bar_protocol);
    }

    let has_footer = state.status_message.is_some() || state.notice_message.is_some();
    let bar_rect = if has_footer {
        let [header_area, body_area, now_playing_area, footer_area] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(4),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        render_header(frame, header_area, state);
        render_body(frame, body_area, state);
        let bar_rect = render_now_playing(frame, now_playing_area, state, bar_protocol);
        render_footer(frame, footer_area, state);
        bar_rect
    } else {
        let [header_area, body_area, now_playing_area] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(4),
        ])
        .areas(frame.area());
        render_header(frame, header_area, state);
        render_body(frame, body_area, state);
        render_now_playing(frame, now_playing_area, state, bar_protocol)
    };

    match state.input_mode {
        InputMode::CreatePlaylistInput => render_text_modal(
            frame,
            " Create Playlist ",
            "Playlist name",
            &state.create_playlist_input,
            Color::Green,
        ),
        InputMode::JamJoinInput => render_text_modal(
            frame,
            " Join Jam ",
            "Jam link or token",
            &state.jam_join_input,
            Color::Magenta,
        ),
        InputMode::AddToPlaylistPicker => render_playlist_picker_modal(frame, state),
        InputMode::ConfirmDelete => render_confirm_delete_modal(frame, state),
        InputMode::Normal | InputMode::SearchInput => {}
    }

    OverlayRects {
        art_rect: None,
        bar_rect,
    }
}

fn render_focus_view(
    frame: &mut Frame,
    state: &AppState,
    art_protocol: ArtProtocol,
    bar_protocol: ArtProtocol,
) -> OverlayRects {
    let area = frame.area();
    if area.width == 0 || area.height == 0 {
        return OverlayRects::default();
    }

    let title_x = if area.width > 1 { area.x + 1 } else { area.x };
    let title_w = area.width.saturating_sub(title_x - area.x).min(6);
    frame.render_widget(
        Paragraph::new(Line::styled(
            "violin",
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD),
        )),
        Rect::new(title_x, area.y, title_w, 1),
    );

    let art_rows = area
        .height
        .saturating_sub(5)
        .min(area.width.saturating_sub(4) / 2)
        .min(24);
    let art_cols = art_rows * 2;
    let stack_h = art_rows + 2;
    let available_h = area.height.saturating_sub(1);
    let top_y = area.y + 1 + available_h.saturating_sub(stack_h) / 2;
    let art_x = area.x + area.width.saturating_sub(art_cols) / 2;
    let art_rect = Rect::new(art_x, top_y, art_cols, art_rows);

    if art_rect.width > 0
        && art_rect.height > 0
        && let Some(art) = &state.cover_art
    {
        match art_protocol {
            ArtProtocol::HalfBlock => {
                let lines = art::render_retro_art_lines(art, art_rect.width, art_rect.height);
                frame.render_widget(Paragraph::new(lines), art_rect);
            }
            ArtProtocol::Kitty => {}
        }
    }

    let bar_w = art_cols.max(20).min(area.width.saturating_sub(4));
    let bar_x = area.x + area.width.saturating_sub(bar_w) / 2;
    let bar_y = top_y + art_rows + 1;
    let bar_rect = if bar_w > 0 && bar_y < area.y + area.height {
        let rect = Rect::new(bar_x, bar_y, bar_w, 1);
        match bar_protocol {
            ArtProtocol::HalfBlock => {
                let ratio = playback_ratio(state.now_playing.as_ref());
                let (filled_fg, empty_bg) = if state
                    .now_playing
                    .as_ref()
                    .is_some_and(|np| np.status == PlaybackStatus::Playing)
                {
                    (Color::Cyan, Color::DarkGray)
                } else {
                    (Color::Rgb(110, 110, 110), Color::Rgb(45, 45, 45))
                };
                frame.render_widget(
                    Paragraph::new(build_progress_bar_line_styled(
                        ratio,
                        usize::from(bar_w),
                        filled_fg,
                        empty_bg,
                    )),
                    rect,
                );
                None
            }
            ArtProtocol::Kitty => Some(rect),
        }
    } else {
        None
    };

    OverlayRects {
        art_rect: (art_rect.width > 0 && art_rect.height > 0).then_some(art_rect),
        bar_rect,
    }
}

fn render_header(frame: &mut Frame, area: Rect, state: &AppState) {
    let selected = match state.active_view {
        View::Search => 0,
        View::MyAlbums => 1,
        View::Queue => 2,
        View::Jam => 3,
    };

    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray))
        .title_top(Line::styled(
            " violin ",
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD),
        ));
    let badge = match state.input_mode {
        InputMode::Normal => state.jam_session.as_ref().map(|_| {
            (
                " [ JAM ] ",
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            )
        }),
        InputMode::SearchInput => Some((
            " SEARCH ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        InputMode::CreatePlaylistInput => Some((
            " NEW PLAYLIST ",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )),
        InputMode::AddToPlaylistPicker => Some((
            " ADD TO PLAYLIST ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        InputMode::JamJoinInput => Some((
            " JOIN JAM ",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        )),
        InputMode::ConfirmDelete => Some((
            " CONFIRM DELETE ",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
    };
    if let Some((label, style)) = badge {
        block = block.title_top(Line::styled(label, style).right_aligned());
    }

    let tabs = Tabs::new(["Search", "Library", "Queue", "Jam"])
        .block(block)
        .select(selected)
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );

    frame.render_widget(tabs, area);
}

fn render_body(frame: &mut Frame, area: Rect, state: &AppState) {
    if let Some(detail) = state.active_album_detail() {
        render_album_detail(frame, area, detail, state);
    } else {
        match state.active_view {
            View::Search => render_search(frame, area, state),
            View::MyAlbums => render_my_albums(frame, area, state),
            View::Queue => render_queue(frame, area, state),
            View::Jam => render_jam(frame, area, state),
        }
    }
}

fn render_search(frame: &mut Frame, area: Rect, state: &AppState) {
    let [input_area, results_area] =
        Layout::vertical([Constraint::Length(3), Constraint::Min(3)]).areas(area);

    render_search_input(frame, input_area, state);
    render_search_results(frame, results_area, state);
}

fn render_search_input(frame: &mut Frame, area: Rect, state: &AppState) {
    let is_editing = state.input_mode == InputMode::SearchInput;
    let border_style = if is_editing {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let (text, text_style) = if state.search_query.is_empty() && !is_editing {
        (
            "Search tracks and albums...",
            Style::default().fg(Color::DarkGray),
        )
    } else {
        (state.search_query.as_str(), Style::default())
    };

    let input = Paragraph::new(text)
        .style(text_style)
        .block(rounded_block(" Search ").border_style(border_style));
    frame.render_widget(input, area);

    if is_editing && area.width > 2 && area.height > 2 {
        let max_offset = area.width.saturating_sub(3) as usize;
        let query_width = state.search_query.chars().count().min(max_offset) as u16;
        frame.set_cursor_position(Position::new(area.x + 1 + query_width, area.y + 1));
    }
}

fn render_search_results(frame: &mut Frame, area: Rect, state: &AppState) {
    let title = match state.search_section {
        SearchSection::Tracks => " [Tracks] / Albums ",
        SearchSection::Albums => " Tracks / [Albums] ",
    };
    let block = rounded_block(title);

    match state.search_section {
        SearchSection::Tracks => {
            if state.search_results.tracks.is_empty() {
                let empty = Paragraph::new("No tracks yet.")
                    .style(Style::default().fg(Color::DarkGray))
                    .block(block);
                frame.render_widget(empty, area);
                return;
            }
            render_track_table(
                frame,
                area,
                block,
                TableRows {
                    tracks: &state.search_results.tracks,
                    selected: state.selected_search_track,
                },
                true,
                state,
            );
        }
        SearchSection::Albums => {
            if state.search_results.albums.is_empty() {
                let empty = Paragraph::new("No albums yet.")
                    .style(Style::default().fg(Color::DarkGray))
                    .block(block);
                frame.render_widget(empty, area);
                return;
            }
            render_album_table(
                frame,
                area,
                block,
                &state.search_results.albums,
                state.selected_search_album,
            );
        }
    }
}

fn render_my_albums(frame: &mut Frame, area: Rect, state: &AppState) {
    let breadcrumb = state.current_library_breadcrumb();
    let title = if breadcrumb.is_empty() {
        " Library ".to_string()
    } else {
        format!(" Library / {} ", breadcrumb.join(" / "))
    };
    let block = rounded_block(title);
    let entries = state.current_library_items();
    if entries.is_empty() {
        let msg = if breadcrumb.is_empty() {
            "No playlists, liked songs, or saved albums found."
        } else {
            "This folder is empty."
        };
        let empty = Paragraph::new(msg)
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(empty, area);
        return;
    }

    let items: Vec<ListItem> = entries
        .iter()
        .map(|entry| match entry {
            LibraryItem::Folder { name, items } => {
                let noun = if items.len() == 1 { "item" } else { "items" };
                ListItem::new(format!("[Folder] {name} ({} {noun})", items.len())).style(
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
            }
            LibraryItem::Collection(album) => ListItem::new(format_album_line(album)),
        })
        .collect();
    render_selectable_list(frame, area, block, items, state.selected_saved_album);
}

fn render_queue(frame: &mut Frame, area: Rect, state: &AppState) {
    let title = match &state.queue_context {
        QueueContext::Collection { label, .. } => format!(" Up Next · {label} "),
        QueueContext::None | QueueContext::Radio => " Up Next (Queue) ".to_string(),
    };
    let block = rounded_block(title);
    let tracks = state.queue_display_tracks();
    if tracks.is_empty() {
        let empty = Paragraph::new("Queue is empty. Play a track to see upcoming songs.")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(empty, area);
        return;
    }

    render_track_table(
        frame,
        area,
        block,
        TableRows {
            tracks: &tracks,
            selected: state.selected_queue_track,
        },
        false,
        state,
    );
}

fn render_jam(frame: &mut Frame, area: Rect, state: &AppState) {
    let block = rounded_block(" Jam ");
    let Some(jam) = &state.jam_session else {
        let empty = Paragraph::new("No active Jam session.")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(empty, area);
        return;
    };

    let mut lines = Vec::new();
    let role = if jam.is_owner { "Host" } else { "Participant" };
    lines.push(Line::from(format!("Session: {} ({role})", jam.session_id)));
    if !jam.join_url.is_empty() {
        lines.push(Line::from(format!("Join URL: {}", jam.join_url)));
    } else if !jam.join_token.is_empty() {
        lines.push(Line::from(format!("Join Token: {}", jam.join_token)));
    }
    lines.push(Line::from(""));
    lines.push(Line::styled(
        format!("Members ({}):", jam.members.len()),
        Style::default().add_modifier(Modifier::BOLD),
    ));
    for member in &jam.members {
        let name = if member.display_name.is_empty() {
            member.username.as_str()
        } else {
            member.display_name.as_str()
        };
        let mut tags = Vec::new();
        if member.is_controlling {
            tags.push("controlling");
        }
        if member.is_listening {
            tags.push("listening");
        }
        let suffix = if tags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", tags.join(", "))
        };
        lines.push(Line::from(format!("  - {name}{suffix}")));
    }
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_album_detail(
    frame: &mut Frame,
    area: Rect,
    detail: &crate::app::AlbumDetail,
    state: &AppState,
) {
    let album_label = format_album_line(&detail.album);
    let block = rounded_block(format!(" {album_label} "));

    if detail.tracks.is_empty() {
        let empty = Paragraph::new("No tracks in this album.")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(empty, area);
        return;
    }

    render_track_table(
        frame,
        area,
        block,
        TableRows {
            tracks: &detail.tracks,
            selected: detail.selected_track,
        },
        false,
        state,
    );
}

/// Tracks to list and the highlighted row.
struct TableRows<'a> {
    tracks: &'a [Track],
    selected: usize,
}

fn render_track_table(
    frame: &mut Frame,
    area: Rect,
    block: Block,
    rows: TableRows,
    show_album: bool,
    state: &AppState,
) {
    let TableRows { tracks, selected } = rows;
    let active_id = state.now_playing.as_ref().map(|np| np.track.id.as_str());
    let header_style = Style::default().fg(Color::DarkGray);
    let header = if show_album {
        Row::new(vec![
            Cell::from(" #"),
            Cell::from(""),
            Cell::from("Title"),
            Cell::from("Artist"),
            Cell::from("Album"),
            Cell::from(Line::from("Time").right_aligned()),
        ])
    } else {
        Row::new(vec![
            Cell::from(" #"),
            Cell::from(""),
            Cell::from("Title"),
            Cell::from("Artist"),
            Cell::from(Line::from("Time").right_aligned()),
        ])
    }
    .style(header_style);

    let clamped_selected = (!tracks.is_empty()).then(|| selected.min(tracks.len() - 1));
    let rows: Vec<Row> = tracks
        .iter()
        .enumerate()
        .map(|(idx, track)| {
            let is_active = active_id == Some(track.id.as_str());
            let is_selected = clamped_selected == Some(idx);
            let liked = state.liked_track_ids.contains(track.id.as_str());

            let (index_cell, liked_cell, title_cell, artist_cell, album_cell, time_cell) =
                if is_selected {
                    let text_style = if is_active {
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Black).bg(Color::Cyan)
                    };
                    let index_cell = if is_active {
                        Cell::from(" ▶").style(text_style)
                    } else {
                        Cell::from(format!("{:>2}", idx + 1)).style(text_style)
                    };
                    let liked_cell = if liked {
                        Cell::from("♥").style(Style::default().fg(Color::Red).bg(Color::Cyan))
                    } else {
                        Cell::from("").style(Style::default().bg(Color::Cyan))
                    };
                    (
                        index_cell,
                        liked_cell,
                        Cell::from(track.title.as_str()).style(text_style),
                        Cell::from(track.artists_display()).style(text_style),
                        Cell::from(track.album_name.as_str()).style(text_style),
                        Cell::from(Line::from(format_duration(track.duration)).right_aligned())
                            .style(text_style),
                    )
                } else {
                    let index_cell = if is_active {
                        Cell::from(" ▶")
                    } else {
                        Cell::from(format!("{:>2}", idx + 1))
                            .style(Style::default().fg(Color::DarkGray))
                    };
                    let liked_cell = if liked {
                        Cell::from("♥").style(Style::default().fg(Color::Red))
                    } else {
                        Cell::from("")
                    };
                    (
                        index_cell,
                        liked_cell,
                        Cell::from(track.title.as_str()),
                        Cell::from(track.artists_display()),
                        Cell::from(track.album_name.as_str()),
                        Cell::from(Line::from(format_duration(track.duration)).right_aligned()),
                    )
                };

            let row = if show_album {
                Row::new(vec![
                    index_cell,
                    liked_cell,
                    title_cell,
                    artist_cell,
                    album_cell,
                    time_cell,
                ])
            } else {
                Row::new(vec![
                    index_cell,
                    liked_cell,
                    title_cell,
                    artist_cell,
                    time_cell,
                ])
            };

            if is_active && !is_selected {
                row.style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                row
            }
        })
        .collect();

    let widths: Vec<Constraint> = if show_album {
        vec![
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Percentage(40),
            Constraint::Percentage(30),
            Constraint::Percentage(30),
            Constraint::Length(6),
        ]
    } else {
        vec![
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Percentage(55),
            Constraint::Percentage(45),
            Constraint::Length(6),
        ]
    };

    let mut table_state = TableState::default();
    table_state.select(clamped_selected);

    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .row_highlight_style(Style::default().bg(Color::Cyan));

    frame.render_stateful_widget(table, area, &mut table_state);
}

fn render_album_table(
    frame: &mut Frame,
    area: Rect,
    block: Block,
    albums: &[Album],
    selected: usize,
) {
    let header = Row::new(vec![
        Cell::from("#"),
        Cell::from("Album"),
        Cell::from("Artist"),
        Cell::from(Line::from("Year").right_aligned()),
    ])
    .style(Style::default().fg(Color::DarkGray));

    let rows: Vec<Row> = albums
        .iter()
        .enumerate()
        .map(|(idx, album)| {
            let year = album
                .release_year
                .map(|y| y.to_string())
                .unwrap_or_default();
            Row::new(vec![
                Cell::from(format!("{:>2}", idx + 1)).style(Style::default().fg(Color::DarkGray)),
                Cell::from(album.name.as_str()),
                Cell::from(album.artists_display()),
                Cell::from(Line::from(year).right_aligned()),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(4),
        Constraint::Percentage(55),
        Constraint::Percentage(45),
        Constraint::Length(6),
    ];

    let mut table_state = TableState::default();
    if !albums.is_empty() {
        table_state.select(Some(selected.min(albums.len() - 1)));
    }

    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .row_highlight_style(Style::default().bg(Color::Cyan).fg(Color::Black));

    frame.render_stateful_widget(table, area, &mut table_state);
}

fn render_now_playing(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    bar_protocol: ArtProtocol,
) -> Option<Rect> {
    let shuffle_active =
        state.shuffle && matches!(state.queue_context, QueueContext::Collection { .. });
    let title = if shuffle_active {
        " Now Playing [SHUFFLE] "
    } else {
        " Now Playing "
    };
    let block = rounded_block(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [track_area, progress_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(inner);

    let [left_track_area, vol_area] =
        Layout::horizontal([Constraint::Min(10), Constraint::Length(9)]).areas(track_area);

    let (track_line, time_label, ratio) = match &state.now_playing {
        Some(np) => {
            let symbol = match np.status {
                PlaybackStatus::Playing => "⏸ ",
                PlaybackStatus::Paused | PlaybackStatus::Stopped => "▶ ",
            };
            let mut spans = vec![Span::raw(symbol)];
            if state.liked_track_ids.contains(np.track.id.as_str()) {
                spans.push(Span::styled("♥ ", Style::default().fg(Color::Red)));
            }
            spans.push(Span::raw(format!(
                "{} — {}",
                np.track.title,
                np.track.artists_display()
            )));

            let dur = np.track.duration;
            let pos = np.position.min(dur);
            let label = format!(" {} / {}", format_duration(pos), format_duration(dur));
            let ratio = playback_ratio(Some(np));
            (Line::from(spans), label, ratio)
        }
        None => (
            Line::styled("Nothing playing", Style::default().fg(Color::DarkGray)),
            " 00:00 / 00:00".to_string(),
            0.0,
        ),
    };

    frame.render_widget(Paragraph::new(track_line), left_track_area);
    frame.render_widget(
        Paragraph::new(Line::from(format!("Vol {}%", state.volume)).right_aligned())
            .style(Style::default().fg(Color::DarkGray)),
        vol_area,
    );

    let time_width = time_label.chars().count() as u16;
    let [bar_area, time_area] =
        Layout::horizontal([Constraint::Min(4), Constraint::Length(time_width)])
            .areas(progress_area);

    let bar_width = bar_area.width as usize;
    let bar_rect = if bar_width > 0 {
        match bar_protocol {
            ArtProtocol::HalfBlock => {
                frame.render_widget(
                    Paragraph::new(build_progress_bar_line(ratio, bar_width)),
                    bar_area,
                );
                None
            }
            ArtProtocol::Kitty => Some(bar_area),
        }
    } else {
        None
    };
    frame.render_widget(
        Paragraph::new(time_label).style(Style::default().fg(Color::DarkGray)),
        time_area,
    );
    bar_rect
}

fn playback_ratio(now_playing: Option<&NowPlaying>) -> f64 {
    let Some(np) = now_playing else {
        return 0.0;
    };
    let dur = np.track.duration;
    if dur.as_millis() == 0 {
        return 0.0;
    }
    let pos = np.position.min(dur);
    (pos.as_secs_f64() / dur.as_secs_f64()).clamp(0.0, 1.0)
}

fn build_progress_bar_line(ratio: f64, bar_width: usize) -> Line<'static> {
    build_progress_bar_line_styled(ratio, bar_width, Color::Cyan, Color::DarkGray)
}

fn build_progress_bar_line_styled(
    ratio: f64,
    bar_width: usize,
    filled_fg: Color,
    empty_bg: Color,
) -> Line<'static> {
    const SUBCELL_BLOCKS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
    let total_eighths = ((ratio * (bar_width * 8) as f64).round() as usize).min(bar_width * 8);
    let full_cells = total_eighths / 8;
    let rem = total_eighths % 8;
    let empty_cells = bar_width.saturating_sub(full_cells + usize::from(rem > 0));
    let mut spans = vec![Span::styled(
        "█".repeat(full_cells),
        Style::default().fg(filled_fg),
    )];
    if rem > 0 {
        spans.push(Span::styled(
            SUBCELL_BLOCKS[rem],
            Style::default().fg(filled_fg).bg(empty_bg),
        ));
    }
    spans.push(Span::styled(
        " ".repeat(empty_cells),
        Style::default().bg(empty_bg),
    ));
    Line::from(spans)
}

fn render_footer(frame: &mut Frame, area: Rect, state: &AppState) {
    if let Some(msg) = &state.status_message {
        frame.render_widget(
            Paragraph::new(msg.as_str()).style(Style::default().fg(Color::Red)),
            area,
        );
        return;
    }
    if let Some(notice) = &state.notice_message {
        frame.render_widget(
            Paragraph::new(notice.as_str()).style(Style::default().fg(Color::Green)),
            area,
        );
    }
}

fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let popup_width = (area.width.saturating_mul(percent_x) / 100)
        .max(24)
        .min(area.width);
    let popup_height = height.min(area.height);
    let x = area.x + area.width.saturating_sub(popup_width) / 2;
    let y = area.y + area.height.saturating_sub(popup_height) / 2;
    Rect::new(x, y, popup_width, popup_height)
}

fn render_text_modal(
    frame: &mut Frame,
    title: &str,
    placeholder: &str,
    value: &str,
    accent: Color,
) {
    let popup = centered_rect(60, 5, frame.area());
    frame.render_widget(Clear, popup);
    let block = rounded_block(title).border_style(Style::default().fg(accent));
    let (text, style) = if value.is_empty() {
        (placeholder, Style::default().fg(Color::DarkGray))
    } else {
        (value, Style::default())
    };
    frame.render_widget(Paragraph::new(text).style(style).block(block), popup);
    if popup.width > 2 && popup.height > 2 {
        let max_offset = popup.width.saturating_sub(3) as usize;
        let offset = value.chars().count().min(max_offset) as u16;
        frame.set_cursor_position(Position::new(popup.x + 1 + offset, popup.y + 1));
    }
}

fn render_confirm_delete_modal(frame: &mut Frame, state: &AppState) {
    let Some(pending) = &state.pending_delete else {
        return;
    };
    let message = match pending {
        PendingDelete::Playlist { playlist_name, .. } => {
            format!("Remove playlist \"{playlist_name}\"?")
        }
        PendingDelete::TrackFromPlaylist {
            playlist_name,
            track_title,
            ..
        } => format!("Remove \"{track_title}\" from \"{playlist_name}\"?"),
    };

    let popup = centered_rect(60, 5, frame.area());
    frame.render_widget(Clear, popup);
    let block = rounded_block(" Confirm Delete ").border_style(Style::default().fg(Color::Red));
    frame.render_widget(
        Paragraph::new(message)
            .wrap(Wrap { trim: true })
            .block(block),
        popup,
    );
}

fn render_playlist_picker_modal(frame: &mut Frame, state: &AppState) {
    let entries = state.current_picker_items();
    let breadcrumb = state.current_picker_breadcrumb();
    let height = (entries.len() as u16 + 4).clamp(6, 16);
    let popup = centered_rect(60, height, frame.area());
    frame.render_widget(Clear, popup);

    let base_title = match &state.add_to_playlist_track {
        Some(track) => format!("Add \"{}\" to Playlist", track.title),
        None => "Add to Playlist".to_string(),
    };
    let title = if breadcrumb.is_empty() {
        format!(" {base_title} ")
    } else {
        format!(" {base_title} / {} ", breadcrumb.join(" / "))
    };

    let block = rounded_block(title).border_style(Style::default().fg(Color::Cyan));
    let items: Vec<ListItem> = entries
        .iter()
        .map(|entry| match entry {
            LibraryItem::Folder { name, items } => {
                let noun = if items.len() == 1 { "item" } else { "items" };
                ListItem::new(format!("[Folder] {name} ({} {noun})", items.len())).style(
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
            }
            LibraryItem::Collection(pl) => ListItem::new(format_album_line(pl)),
        })
        .collect();
    render_selectable_list(frame, popup, block, items, state.selected_playlist_picker);
}

fn render_selectable_list(
    frame: &mut Frame,
    area: Rect,
    block: Block,
    items: Vec<ListItem>,
    selected: usize,
) {
    let mut list_state = ListState::default();
    if !items.is_empty() {
        list_state.select(Some(selected.min(items.len() - 1)));
    }
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::Cyan).fg(Color::Black))
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn format_album_line(album: &Album) -> String {
    if !album.artists.is_empty() {
        return match album.release_year {
            Some(year) => format!("{} — {} ({year})", album.name, album.artists_display()),
            None => format!("{} — {}", album.name, album.artists_display()),
        };
    }
    if album.total_tracks > 0 {
        let noun = if album.total_tracks == 1 {
            "track"
        } else {
            "tracks"
        };
        return format!("{}  ({} {noun})", album.name, album.total_tracks);
    }
    album.name.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::{AlbumDetail, AppEvent, NowPlaying},
        domain::{AlbumId, SearchResults, Track, TrackId},
    };
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

    fn make_track(id: &str, title: &str, secs: u64) -> Track {
        Track {
            id: TrackId::new(id).unwrap(),
            title: title.to_string(),
            artists: vec!["Beethoven".to_string()],
            album_name: "Sonatas".to_string(),
            duration: Duration::from_secs(secs),
            cover_url: None,
        }
    }

    fn make_album(id: &str, name: &str, year: Option<u16>) -> Album {
        Album {
            id: AlbumId::new(id).unwrap(),
            name: name.to_string(),
            artists: vec!["Martha Argerich".to_string()],
            release_year: year,
            total_tracks: 3,
        }
    }

    fn render_buffer(state: &AppState, width: u16, height: u16) -> Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, state)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn buffer_to_string(buffer: &Buffer) -> String {
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn render_to_string(state: &AppState, width: u16, height: u16) -> String {
        buffer_to_string(&render_buffer(state, width, height))
    }

    #[test]
    fn format_duration_formats_zero_standard_and_long_tracks() {
        assert_eq!(format_duration(Duration::from_secs(0)), "00:00");
        assert_eq!(format_duration(Duration::from_secs(65)), "01:05");
        assert_eq!(format_duration(Duration::from_secs(3661)), "61:01");
    }

    #[test]
    fn renders_default_search_view_with_rounded_borders_and_zero_shortcuts() {
        let state = AppState::new();
        let buf = render_buffer(&state, 100, 24);
        let screen = buffer_to_string(&buf);

        assert!(screen.contains("violin"));
        let top_row: String = (0..100).map(|x| buf[(x, 0)].symbol()).collect();
        let title_start = top_row.find("violin").expect("violin title on top row");
        let title_col = top_row[..title_start].chars().count();
        for offset in 0.."violin".len() {
            let cell = &buf[((title_col + offset) as u16, 0)];
            assert_eq!(cell.fg, Color::LightCyan);
            assert!(cell.modifier.contains(Modifier::BOLD));
        }
        assert!(screen.contains("Search"));
        assert!(screen.contains("Library"));
        assert!(screen.contains("Queue"));
        assert!(screen.contains("Jam"));
        assert!(screen.contains('╭'));
        assert!(screen.contains('╰'));
        assert!(screen.contains("Search tracks and albums..."));
        assert!(screen.contains("No tracks yet."));
        assert!(screen.contains("Nothing playing"));
        assert!(screen.contains("00:00 / 00:00"));
        assert!(screen.contains("Vol 80%"));

        for forbidden in [
            "Ctrl+l",
            "Esc",
            "s: switch",
            "j/k",
            "Enter",
            "q: quit",
            "[1]",
            "NORMAL",
        ] {
            assert!(
                !screen.contains(forbidden),
                "unexpected shortcut hint '{forbidden}' in screen:\n{screen}"
            );
        }
    }

    #[test]
    fn renders_search_results_tables_active_track_highlight_and_album_detail() {
        let mut state = AppState::new();
        let track = make_track("t1", "Kreutzer Sonata", 125);
        let album = make_album("a1", "Violin Sonatas", Some(1998));
        state.search_results = SearchResults {
            tracks: vec![track.clone()],
            albums: vec![album.clone()],
        };
        state.now_playing = Some(NowPlaying {
            track: track.clone(),
            status: PlaybackStatus::Playing,
            position: Duration::from_secs(10),
        });

        let buf = render_buffer(&state, 100, 24);
        let tracks_screen = buffer_to_string(&buf);
        assert!(tracks_screen.contains("[Tracks] / Albums"));
        assert!(!tracks_screen.contains("s: switch"));
        assert!(tracks_screen.contains("Title"));
        assert!(tracks_screen.contains("Artist"));
        assert!(tracks_screen.contains("Album"));
        assert!(tracks_screen.contains("Time"));
        assert!(tracks_screen.contains("Kreutzer Sonata"));
        assert!(tracks_screen.contains("Beethoven"));
        assert!(tracks_screen.contains("Sonatas"));
        assert!(tracks_screen.contains("02:05"));

        // Selected active track row has ▶ marker with Color::Black on Color::Cyan.
        let mut found_active_selected = false;
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                if cell.symbol() == "▶"
                    && cell.fg == Color::Black
                    && cell.bg == Color::Cyan
                    && y < 20
                {
                    found_active_selected = true;
                }
            }
        }
        assert!(
            found_active_selected,
            "expected selected active track ▶ with Color::Black on Color::Cyan"
        );

        state.search_section = SearchSection::Albums;
        let albums_screen = render_to_string(&state, 100, 24);
        assert!(albums_screen.contains("Tracks / [Albums]"));
        assert!(albums_screen.contains("Year"));
        assert!(albums_screen.contains("Violin Sonatas"));
        assert!(albums_screen.contains("Martha Argerich"));
        assert!(albums_screen.contains("1998"));

        state.active_view = View::MyAlbums;
        state.saved_albums = vec![LibraryItem::Collection(make_album(
            "a2",
            "Live Concert",
            None,
        ))];
        let my_albums_screen = render_to_string(&state, 100, 24);
        assert!(my_albums_screen.contains("Library"));
        assert!(my_albums_screen.contains("Live Concert — Martha Argerich"));

        state.album_detail = Some(AlbumDetail {
            origin_view: View::MyAlbums,
            album,
            tracks: vec![track],
            selected_track: 0,
        });
        let detail_screen = render_to_string(&state, 100, 24);
        assert!(!detail_screen.contains("[Esc: back]"));
        assert!(detail_screen.contains("Kreutzer Sonata"));
        assert!(detail_screen.contains("02:05"));

        state.active_view = View::Queue;
        let queue_screen = render_to_string(&state, 100, 24);
        assert!(queue_screen.contains("Queue"));
        assert!(!queue_screen.contains("Violin Sonatas"));
    }

    #[test]
    fn renders_now_playing_bar_volume_and_status_footer() {
        let mut state = AppState::new();
        let track = make_track("t1", "Adagio", 200);
        state.now_playing = Some(NowPlaying {
            track,
            status: PlaybackStatus::Playing,
            position: Duration::from_secs(65),
        });
        state.volume = 45;
        state.status_message = Some("Playback failed".to_string());

        let playing_screen = render_to_string(&state, 100, 24);
        assert!(playing_screen.contains("⏸"));
        assert!(playing_screen.contains("Adagio — Beethoven"));
        assert!(playing_screen.contains("Vol 45%"));
        assert!(playing_screen.contains("01:05 / 03:20"));
        assert!(playing_screen.contains('█'));
        assert!(playing_screen.contains('▎'));
        assert!(playing_screen.contains("Playback failed"));

        state.status_message = None;
        if let Some(np) = state.now_playing.as_mut() {
            np.status = PlaybackStatus::Paused;
        }
        let paused_screen = render_to_string(&state, 100, 24);
        assert!(paused_screen.contains("▶"));
        assert!(!paused_screen.contains("Playback failed"));
        assert!(!paused_screen.contains("Now Playing [SHUFFLE]"));

        if let Some(np) = state.now_playing.as_mut() {
            np.status = PlaybackStatus::Stopped;
        }
        let stopped_screen = render_to_string(&state, 100, 24);
        assert!(stopped_screen.contains("▶"));

        state.shuffle = true;
        let radio_screen = render_to_string(&state, 100, 24);
        assert!(!radio_screen.contains("[SHUFFLE]"));

        state.queue_context = QueueContext::Collection {
            label: "Album: Blue".to_string(),
            tracks: Vec::new(),
        };
        let shuffled_screen = render_to_string(&state, 100, 24);
        assert!(shuffled_screen.contains("Now Playing [SHUFFLE]"));
    }

    #[test]
    fn renders_queue_view_empty_and_populated() {
        let mut state = AppState::new();
        state.active_view = View::Queue;

        let empty_screen = render_to_string(&state, 100, 24);
        assert!(empty_screen.contains("Up Next (Queue)"));
        assert!(empty_screen.contains("Queue is empty. Play a track to see upcoming songs."));

        state.now_playing = Some(NowPlaying {
            track: make_track("t1", "Adagio", 200),
            status: PlaybackStatus::Playing,
            position: Duration::from_secs(10),
        });
        state.upcoming_queue = vec![make_track("q1", "Chaconne", 245)];
        let populated_screen = render_to_string(&state, 100, 24);
        assert!(populated_screen.contains("▶"));
        assert!(populated_screen.contains("Adagio"));
        assert!(populated_screen.contains("Chaconne"));
        assert!(populated_screen.contains("Beethoven"));
        assert!(populated_screen.contains("04:05"));

        state.queue_context = QueueContext::Collection {
            label: "Album: Kind of Blue".to_string(),
            tracks: Vec::new(),
        };
        let album_screen = render_to_string(&state, 100, 24);
        assert!(album_screen.contains("Up Next · Album: Kind of Blue"));
        assert!(!album_screen.contains("Up Next (Queue)"));
    }

    #[test]
    fn queue_view_starts_with_now_playing_highlighted() {
        let mut state = AppState::new();
        let tracks: Vec<Track> = ["Prelude", "Allemande", "Courante", "Sarabande"]
            .iter()
            .enumerate()
            .map(|(i, title)| make_track(&format!("t{i}"), title, 100))
            .collect();
        state.handle_event(AppEvent::AlbumTracksLoaded {
            album: make_album("a1", "Suites", None),
            tracks,
            auto_play: true,
        });
        state.handle_event(AppEvent::TrackEnded);
        state.handle_event(AppEvent::TrackEnded);

        let buf = render_buffer(&state, 100, 24);
        let screen = render_to_string(&state, 100, 24);

        assert!(!screen.contains("Prelude"));
        assert!(!screen.contains("Allemande"));
        let first_row = (0..buf.area.height)
            .find(|&y| {
                let line: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
                line.contains("Courante")
            })
            .expect("now-playing title not rendered");
        let header_row = (0..buf.area.height)
            .find(|&y| {
                let line: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
                line.contains("Title")
            })
            .expect("table header not rendered");
        assert_eq!(first_row, header_row + 1);
        assert_eq!(buf[(5, first_row)].bg, Color::Cyan);
    }

    #[test]
    fn renders_library_folders_breadcrumbs_and_empty_folder() {
        let mut state = AppState::new();
        state.active_view = View::MyAlbums;
        state.saved_albums = vec![
            LibraryItem::Folder {
                name: "Albums".to_string(),
                items: vec![
                    LibraryItem::Collection(make_album("a1", "Kind of Blue", Some(1959))),
                    LibraryItem::Folder {
                        name: "Empty Sub".to_string(),
                        items: vec![],
                    },
                ],
            },
            LibraryItem::Folder {
                name: "Single".to_string(),
                items: vec![LibraryItem::Collection(make_album("a2", "Solo", None))],
            },
        ];

        let root_screen = render_to_string(&state, 100, 24);
        assert!(root_screen.contains("Library"));
        assert!(root_screen.contains("[Folder] Albums (2 items)"));
        assert!(root_screen.contains("[Folder] Single (1 item)"));

        state.library_folder_path = vec![0];
        let folder_screen = render_to_string(&state, 100, 24);
        assert!(folder_screen.contains("Library / Albums"));
        assert!(!folder_screen.contains("[Esc: back]"));
        assert!(folder_screen.contains("Kind of Blue — Martha Argerich (1959)"));
        assert!(folder_screen.contains("[Folder] Empty Sub (0 items)"));

        state.library_folder_path = vec![0, 1];
        let empty_screen = render_to_string(&state, 100, 24);
        assert!(empty_screen.contains("Library / Albums / Empty Sub"));
        assert!(empty_screen.contains("This folder is empty."));
    }

    #[test]
    fn renders_liked_heart_clean_empty_artist_playlists_notices_jam_and_folder_picker_modal() {
        use crate::domain::{JamMember, JamSession};

        let mut state = AppState::new();
        let track = make_track("trk_heart", "Chaconne", 245);
        let unliked_track = make_track("trk_plain", "Sarabande", 180);
        state.liked_track_ids.insert("trk_heart".to_string());
        state.search_results = SearchResults {
            tracks: vec![track.clone(), unliked_track],
            albums: vec![],
        };
        state.now_playing = Some(NowPlaying {
            track: track.clone(),
            status: PlaybackStatus::Playing,
            position: Duration::from_secs(10),
        });
        state.notice_message = Some("Created playlist \"Bach\"".to_string());

        let buf = render_buffer(&state, 100, 24);
        let search_screen = buffer_to_string(&buf);
        let table_lines: String = search_screen
            .lines()
            .take(20)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(table_lines.contains("Chaconne"));
        assert!(table_lines.contains("Sarabande"));
        assert!(search_screen.contains("04:05"));
        assert!(search_screen.contains("Created playlist \"Bach\""));

        let row_x_for = |needle: &str| -> u16 {
            search_screen
                .lines()
                .find_map(|line| {
                    line.find(needle)
                        .map(|idx| line[..idx].chars().count() as u16)
                })
                .expect("expected track title in rendered buffer")
        };
        assert_eq!(
            row_x_for("Chaconne"),
            row_x_for("Sarabande"),
            "liked and unliked track titles must align to the same x-column"
        );

        let red_hearts: Vec<(u16, u16)> = (0..buf.area.height)
            .flat_map(|y| (0..buf.area.width).map(move |x| (x, y)))
            .filter_map(|(x, y)| {
                let cell = &buf[(x, y)];
                (cell.symbol() == "♥" && cell.fg == Color::Red).then_some((x, y))
            })
            .collect();
        let table_heart = red_hearts
            .iter()
            .copied()
            .find(|&(_, y)| y < 20)
            .expect("expected liked ♥ in track table with Color::Red");
        assert!(
            table_heart.0 < row_x_for("Chaconne"),
            "expected table ♥ in dedicated column to the left of title"
        );
        let heart_cell = &buf[table_heart];
        assert_eq!(heart_cell.fg, Color::Red);
        assert_eq!(heart_cell.bg, Color::Cyan);
        assert!(!heart_cell.modifier.contains(Modifier::REVERSED));
        let title_cell = &buf[(row_x_for("Chaconne"), table_heart.1)];
        assert_eq!(title_cell.fg, Color::Black);
        assert_eq!(title_cell.bg, Color::Cyan);
        assert!(
            red_hearts.iter().any(|&(_, y)| y >= 20),
            "expected liked ♥ in now playing bar with Color::Red"
        );
        state.now_playing = None;

        // Clean empty-artist playlist formatting.
        state.notice_message = None;
        state.active_view = View::MyAlbums;
        state.saved_albums = vec![
            LibraryItem::Folder {
                name: "Curated".to_string(),
                items: vec![LibraryItem::Collection(Album {
                    id: AlbumId::new("playlist:p_nested").unwrap(),
                    name: "Deep Mix".to_string(),
                    artists: vec![],
                    release_year: None,
                    total_tracks: 5,
                })],
            },
            LibraryItem::Collection(Album {
                id: AlbumId::new("playlist:p1").unwrap(),
                name: "Night Mix".to_string(),
                artists: vec![],
                release_year: None,
                total_tracks: 12,
            }),
            LibraryItem::Collection(Album {
                id: AlbumId::new("playlist:p2").unwrap(),
                name: "Empty Mix".to_string(),
                artists: vec![],
                release_year: None,
                total_tracks: 0,
            }),
        ];
        let lib_screen = render_to_string(&state, 100, 24);
        assert!(lib_screen.contains("Night Mix  (12 tracks)"));
        assert!(!lib_screen.contains("Night Mix —"));
        assert!(lib_screen.contains("Empty Mix"));

        // Jam view + [ JAM ] badge without shortcut hints.
        state.active_view = View::Jam;
        let no_jam = render_to_string(&state, 100, 24);
        assert!(no_jam.contains("No active Jam session."));
        assert!(!no_jam.contains("Press 'J'"));
        assert!(!no_jam.contains("[ JAM ]"));

        state.jam_session = Some(JamSession {
            session_id: "sess_1".to_string(),
            join_token: "tok_1".to_string(),
            join_url: "https://spotify.link/tok_1".to_string(),
            owner_id: "alice".to_string(),
            is_owner: true,
            is_listening: true,
            is_controlling: true,
            members: vec![JamMember {
                username: "alice".to_string(),
                display_name: "Alice".to_string(),
                is_listening: true,
                is_controlling: true,
            }],
        });
        let active_jam = render_to_string(&state, 100, 24);
        assert!(active_jam.contains("[ JAM ]"));
        assert!(active_jam.contains("Session: sess_1 (Host)"));
        assert!(active_jam.contains("Alice [controlling, listening]"));

        // Modals.
        state.input_mode = InputMode::CreatePlaylistInput;
        state.create_playlist_input = "Morning".to_string();
        let create_modal = render_to_string(&state, 100, 24);
        assert!(create_modal.contains("NEW PLAYLIST"));
        assert!(create_modal.contains("Create Playlist"));
        assert!(create_modal.contains("Morning"));

        // Folder-aware Add-to-Playlist modal (root and nested folder breadcrumb).
        state.input_mode = InputMode::AddToPlaylistPicker;
        state.add_to_playlist_track = Some(track);
        let picker_root = render_to_string(&state, 100, 24);
        assert!(picker_root.contains("ADD TO PLAYLIST"));
        assert!(picker_root.contains("Add \"Chaconne\" to Playlist"));
        assert!(picker_root.contains("[Folder] Curated (1 item)"));
        assert!(picker_root.contains("Night Mix  (12 tracks)"));

        state.picker_folder_path = vec![0];
        let picker_nested = render_to_string(&state, 100, 24);
        assert!(picker_nested.contains("Add \"Chaconne\" to Playlist / Curated"));
        assert!(picker_nested.contains("Deep Mix  (5 tracks)"));

        state.input_mode = InputMode::JamJoinInput;
        state.jam_join_input = "tok_99".to_string();
        let jam_modal = render_to_string(&state, 100, 24);
        assert!(jam_modal.contains("JOIN JAM"));
        assert!(jam_modal.contains("Join Jam"));
        assert!(jam_modal.contains("tok_99"));

        state.input_mode = InputMode::ConfirmDelete;
        state.pending_delete = Some(PendingDelete::Playlist {
            playlist_id: "pl_1".to_string(),
            playlist_name: "Night Mix".to_string(),
        });
        let confirm_playlist = render_to_string(&state, 100, 24);
        assert!(confirm_playlist.contains("CONFIRM DELETE"));
        assert!(confirm_playlist.contains("Confirm Delete"));
        assert!(confirm_playlist.contains("Remove playlist \"Night Mix\"?"));

        state.pending_delete = Some(PendingDelete::TrackFromPlaylist {
            playlist_id: "pl_1".to_string(),
            playlist_name: "Night Mix".to_string(),
            track_id: "trk_1".to_string(),
            track_title: "Chaconne".to_string(),
        });
        let confirm_track = render_to_string(&state, 100, 24);
        assert!(confirm_track.contains("CONFIRM DELETE"));
        assert!(confirm_track.contains("Remove \"Chaconne\" from \"Night Mix\"?"));
    }

    #[test]
    fn renders_subcell_progress_bar_at_zero_intermediate_and_full() {
        let mut state = AppState::new();
        // Width 100 -> Now Playing inner width 98; time label " 00:00 / 01:24" is 14 chars -> bar_width = 84.
        let track = make_track("t1", "Adagio", 84);
        state.now_playing = Some(NowPlaying {
            track,
            status: PlaybackStatus::Playing,
            position: Duration::ZERO,
        });

        // 0%: no filled or partial blocks; unfilled track uses DarkGray background.
        let zero_buf = render_buffer(&state, 100, 24);
        let zero_str = buffer_to_string(&zero_buf);
        assert!(!zero_str.contains('█'));
        for ch in ['▏', '▎', '▍', '▌', '▋', '▊', '▉'] {
            assert!(!zero_str.contains(ch));
        }
        let bar_y = (0..zero_buf.area.height)
            .find(|&y| {
                let row: String = (0..zero_buf.area.width)
                    .map(|x| zero_buf[(x, y)].symbol())
                    .collect();
                row.contains("00:00 / 01:24")
            })
            .unwrap();
        assert_eq!(zero_buf[(1, bar_y)].bg, Color::DarkGray);
        assert_eq!(zero_buf[(84, bar_y)].bg, Color::DarkGray);

        // Intermediate fractional steps on bar_width = 84 with duration = 84s:
        // 125ms -> 1/8 of a cell ("▏")
        state.now_playing.as_mut().unwrap().position = Duration::from_millis(125);
        let one_eighth_buf = render_buffer(&state, 100, 24);
        assert_eq!(one_eighth_buf[(1, bar_y)].symbol(), "▏");
        assert_eq!(one_eighth_buf[(1, bar_y)].fg, Color::Cyan);
        assert_eq!(one_eighth_buf[(1, bar_y)].bg, Color::DarkGray);

        // 10.5s -> 10 full cells + 4/8 of cell 11 ("▌")
        state.now_playing.as_mut().unwrap().position = Duration::from_millis(10_500);
        let half_buf = render_buffer(&state, 100, 24);
        assert_eq!(half_buf[(10, bar_y)].symbol(), "█");
        assert_eq!(half_buf[(10, bar_y)].fg, Color::Cyan);
        assert_eq!(half_buf[(11, bar_y)].symbol(), "▌");
        assert_eq!(half_buf[(11, bar_y)].fg, Color::Cyan);
        assert_eq!(half_buf[(11, bar_y)].bg, Color::DarkGray);
        assert_eq!(half_buf[(12, bar_y)].symbol(), " ");
        assert_eq!(half_buf[(12, bar_y)].bg, Color::DarkGray);

        // 100%: all 84 bar cells are full "█" in Cyan, no partial blocks.
        state.now_playing.as_mut().unwrap().position = Duration::from_secs(84);
        let full_buf = render_buffer(&state, 100, 24);
        for x in 1..=84 {
            assert_eq!(full_buf[(x, bar_y)].symbol(), "█");
            assert_eq!(full_buf[(x, bar_y)].fg, Color::Cyan);
        }
    }

    fn make_solid_cover_art(r: u8, g: u8, b: u8) -> crate::domain::CoverArt {
        let mut rgb = Vec::with_capacity(4 * 4 * 3);
        for _ in 0..16 {
            rgb.extend_from_slice(&[r, g, b]);
        }
        crate::domain::CoverArt {
            track_id: TrackId::new("trk_art").unwrap(),
            width: 4,
            height: 4,
            rgb: std::sync::Arc::from(rgb),
            encoded_png: std::sync::Arc::from(vec![1, 2, 3]),
        }
    }

    #[test]
    fn focus_mode_renders_only_violin_art_and_progress_bar_with_dim_when_paused() {
        let mut state = AppState::new();
        state.focus_mode = true;
        state.now_playing = Some(NowPlaying {
            track: make_track("t1", "Kreutzer Sonata", 100),
            status: PlaybackStatus::Playing,
            position: Duration::from_secs(50),
        });
        state.cover_art = Some(make_solid_cover_art(120, 45, 200));

        let buf = render_buffer(&state, 80, 24);
        let screen = buffer_to_string(&buf);

        assert!(screen.contains("violin"));
        assert_eq!(buf[(1, 0)].symbol(), "v");
        assert_eq!(buf[(1, 0)].fg, Color::LightCyan);
        assert!(buf[(1, 0)].modifier.contains(Modifier::BOLD));

        assert!(screen.contains('█'));
        let has_art_color = (0..buf.area.height).any(|y| {
            (0..buf.area.width)
                .any(|x| buf[(x, y)].symbol() == "█" && buf[(x, y)].fg == Color::Rgb(120, 45, 200))
        });
        assert!(has_art_color);

        let has_cyan_bar = (0..buf.area.height).any(|y| {
            (0..buf.area.width)
                .any(|x| buf[(x, y)].symbol() == "█" && buf[(x, y)].fg == Color::Cyan)
        });
        assert!(has_cyan_bar);

        for forbidden in [
            "⏸",
            "▶",
            "■",
            "||",
            "Kreutzer Sonata",
            "Beethoven",
            "00:50",
            "01:40",
            "Search",
            "Library",
            "Queue",
            "Jam",
            "Now Playing",
            "╭",
            "╰",
            "Vol",
        ] {
            assert!(
                !screen.contains(forbidden),
                "unexpected '{forbidden}' in focus mode:\n{screen}"
            );
        }

        state.now_playing.as_mut().unwrap().status = PlaybackStatus::Paused;
        let paused_buf = render_buffer(&state, 80, 24);
        let has_dim_bar = (0..paused_buf.area.height).any(|y| {
            (0..paused_buf.area.width).any(|x| {
                paused_buf[(x, y)].symbol() == "█"
                    && paused_buf[(x, y)].fg == Color::Rgb(110, 110, 110)
            })
        });
        assert!(has_dim_bar);
        let has_cyan_when_paused = (0..paused_buf.area.height).any(|y| {
            (0..paused_buf.area.width)
                .any(|x| paused_buf[(x, y)].symbol() == "█" && paused_buf[(x, y)].fg == Color::Cyan)
        });
        assert!(!has_cyan_when_paused);
    }

    #[test]
    fn focus_mode_with_kitty_protocol_leaves_art_and_bar_rects_blank_and_returns_rects() {
        let mut state = AppState::new();
        state.focus_mode = true;
        state.now_playing = Some(NowPlaying {
            track: make_track("t1", "Chaconne", 120),
            status: PlaybackStatus::Paused,
            position: Duration::from_secs(30),
        });
        state.cover_art = Some(make_solid_cover_art(10, 200, 30));

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut overlays = OverlayRects::default();
        terminal
            .draw(|frame| {
                overlays =
                    render_with_protocol(frame, &state, ArtProtocol::Kitty, ArtProtocol::Kitty);
            })
            .unwrap();

        let rect = overlays
            .art_rect
            .expect("expected Some(art_rect) in focus mode");
        assert!(rect.width > 0 && rect.height > 0);
        let bar_rect = overlays
            .bar_rect
            .expect("expected Some(bar_rect) in focus mode");
        assert!(bar_rect.width > 0 && bar_rect.height == 1);

        let buf = terminal.backend().buffer();
        for y in rect.y..rect.y + rect.height {
            for x in rect.x..rect.x + rect.width {
                assert_ne!(buf[(x, y)].symbol(), "█");
                assert_eq!(buf[(x, y)].symbol(), " ");
            }
        }
        for x in bar_rect.x..bar_rect.x + bar_rect.width {
            assert_eq!(buf[(x, bar_rect.y)].symbol(), " ");
            assert_eq!(buf[(x, bar_rect.y)].bg, Color::Reset);
        }
    }

    #[test]
    fn normal_mode_with_kitty_bar_protocol_leaves_bar_rect_blank_and_returns_rect() {
        let mut state = AppState::new();
        state.now_playing = Some(NowPlaying {
            track: make_track("t1", "Chaconne", 120),
            status: PlaybackStatus::Playing,
            position: Duration::from_secs(30),
        });

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut overlays = OverlayRects::default();
        terminal
            .draw(|frame| {
                overlays =
                    render_with_protocol(frame, &state, ArtProtocol::HalfBlock, ArtProtocol::Kitty);
            })
            .unwrap();

        assert_eq!(overlays.art_rect, None);
        let bar_rect = overlays
            .bar_rect
            .expect("expected Some(bar_rect) in normal mode");
        assert!(bar_rect.width > 0 && bar_rect.height == 1);

        let buf = terminal.backend().buffer();
        for x in bar_rect.x..bar_rect.x + bar_rect.width {
            assert_eq!(buf[(x, bar_rect.y)].symbol(), " ");
            assert_eq!(buf[(x, bar_rect.y)].bg, Color::Reset);
        }
    }
}
