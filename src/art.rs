use std::fmt::Write as _;
use std::io::Cursor;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use image::imageops::FilterType;
use image::{ImageBuffer, ImageFormat, Rgb, Rgba, RgbaImage};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use thiserror::Error;

use crate::domain::{CoverArt, TrackId};

const MAX_COVER_DIMENSION: u32 = 512;
const KITTY_CHUNK_SIZE: usize = 4096;
pub const KITTY_COVER_IMAGE_ID: u32 = 1;
pub const KITTY_BAR_IMAGE_ID: u32 = 2;
pub const KITTY_BAR_ALT_IMAGE_ID: u32 = 3;
pub const BAR_CELL_W: u32 = 16;

/// Terminal cover-art rendering protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtProtocol {
    Kitty,
    HalfBlock,
}

/// Detects the cover-art protocol from explicit environment values.
pub fn detect_art_protocol_from_env(
    term: Option<&str>,
    term_program: Option<&str>,
    kitty_window_id: Option<&str>,
    ghostty_resources_dir: Option<&str>,
    in_tmux_or_screen: bool,
) -> ArtProtocol {
    if in_tmux_or_screen {
        return ArtProtocol::HalfBlock;
    }

    if ghostty_resources_dir.is_some_and(|v| !v.is_empty())
        || kitty_window_id.is_some_and(|v| !v.is_empty())
    {
        return ArtProtocol::Kitty;
    }

    if let Some(prog) = term_program {
        let lower = prog.to_ascii_lowercase();
        if matches!(lower.as_str(), "ghostty" | "kitty" | "wezterm") {
            return ArtProtocol::Kitty;
        }
    }

    if let Some(term_val) = term {
        let lower = term_val.to_ascii_lowercase();
        if lower.contains("ghostty") || lower.contains("kitty") {
            return ArtProtocol::Kitty;
        }
    }

    ArtProtocol::HalfBlock
}

/// Detects the cover-art protocol from the current process environment.
pub fn detect_art_protocol() -> ArtProtocol {
    let term = std::env::var("TERM").ok();
    let term_program = std::env::var("TERM_PROGRAM").ok();
    let kitty_window_id = std::env::var("KITTY_WINDOW_ID").ok();
    let ghostty_resources_dir = std::env::var("GHOSTTY_RESOURCES_DIR").ok();
    let in_tmux_or_screen = std::env::var("TMUX").is_ok_and(|v| !v.is_empty())
        || std::env::var("STY").is_ok_and(|v| !v.is_empty());

    detect_art_protocol_from_env(
        term.as_deref(),
        term_program.as_deref(),
        kitty_window_id.as_deref(),
        ghostty_resources_dir.as_deref(),
        in_tmux_or_screen,
    )
}

/// Renders `art` into `rows` lines of `cols` 2×2 Unicode quadrant-block spans using 24-bit RGB.
pub fn render_retro_art_lines(art: &CoverArt, cols: u16, rows: u16) -> Vec<Line<'static>> {
    if cols == 0 || rows == 0 || art.width == 0 || art.height == 0 {
        return Vec::new();
    }
    let expected_len = (art.width as usize)
        .saturating_mul(art.height as usize)
        .saturating_mul(3);
    if art.rgb.len() != expected_len {
        return Vec::new();
    }

    let Some(src) = ImageBuffer::<Rgb<u8>, &[u8]>::from_raw(art.width, art.height, &art.rgb) else {
        return Vec::new();
    };
    let target_w = u32::from(cols) * 2;
    let target_h = u32::from(rows) * 2;
    let resized = image::imageops::resize(&src, target_w, target_h, FilterType::Lanczos3);

    let mut lines = Vec::with_capacity(usize::from(rows));
    for cy in 0..u32::from(rows) {
        let y0 = cy * 2;
        let y1 = y0 + 1;
        let mut spans = Vec::with_capacity(usize::from(cols));
        for cx in 0..u32::from(cols) {
            let x0 = cx * 2;
            let x1 = x0 + 1;
            let pixels = [
                resized.get_pixel(x0, y0).0,
                resized.get_pixel(x1, y0).0,
                resized.get_pixel(x0, y1).0,
                resized.get_pixel(x1, y1).0,
            ];
            let (glyph, fg, bg) = best_quadrant_cell(pixels);
            spans.push(Span::styled(glyph, Style::default().fg(fg).bg(bg)));
        }
        lines.push(Line::from(spans));
    }
    lines
}

fn best_quadrant_cell(pixels: [[u8; 3]; 4]) -> (&'static str, Color, Color) {
    if pixels[0] == pixels[1] && pixels[1] == pixels[2] && pixels[2] == pixels[3] {
        let [r, g, b] = pixels[0];
        let color = Color::Rgb(r, g, b);
        return ("█", color, color);
    }

    const GLYPHS: [&str; 8] = ["", "▘", "▝", "▀", "▖", "▌", "▞", "▛"];

    let mut best_mask = 1usize;
    let mut best_err = u32::MAX;
    let mut best_fg = [0u8; 3];
    let mut best_bg = [0u8; 3];

    for mask in 1..=7usize {
        let mut fg_sum = [0u32; 3];
        let mut bg_sum = [0u32; 3];
        let mut fg_count = 0u32;
        let mut bg_count = 0u32;

        for (i, p) in pixels.iter().enumerate() {
            if (mask & (1 << i)) != 0 {
                fg_sum[0] += u32::from(p[0]);
                fg_sum[1] += u32::from(p[1]);
                fg_sum[2] += u32::from(p[2]);
                fg_count += 1;
            } else {
                bg_sum[0] += u32::from(p[0]);
                bg_sum[1] += u32::from(p[1]);
                bg_sum[2] += u32::from(p[2]);
                bg_count += 1;
            }
        }

        let fg = [
            (fg_sum[0] / fg_count) as u8,
            (fg_sum[1] / fg_count) as u8,
            (fg_sum[2] / fg_count) as u8,
        ];
        let bg = [
            (bg_sum[0] / bg_count) as u8,
            (bg_sum[1] / bg_count) as u8,
            (bg_sum[2] / bg_count) as u8,
        ];

        let mut err = 0u32;
        for (i, p) in pixels.iter().enumerate() {
            let target = if (mask & (1 << i)) != 0 { fg } else { bg };
            for c in 0..3 {
                let d = i32::from(p[c]) - i32::from(target[c]);
                err += (d * d) as u32;
            }
        }

        if err < best_err {
            best_err = err;
            best_mask = mask;
            best_fg = fg;
            best_bg = bg;
        }
    }

    (
        GLYPHS[best_mask],
        Color::Rgb(best_fg[0], best_fg[1], best_fg[2]),
        Color::Rgb(best_bg[0], best_bg[1], best_bg[2]),
    )
}

fn corner_signed_distance(x: u32, y: u32, width: u32, height: u32, radius: f32) -> f32 {
    let px = x as f32 + 0.5;
    let py = y as f32 + 0.5;
    let w = width as f32;
    let h = height as f32;

    let dx = if px < radius {
        radius - px
    } else if px > w - radius {
        px - (w - radius)
    } else {
        0.0
    };
    let dy = if py < radius {
        radius - py
    } else if py > h - radius {
        py - (h - radius)
    } else {
        0.0
    };

    if dx > 0.0 && dy > 0.0 {
        dx.hypot(dy) - radius
    } else {
        -radius
    }
}

/// Encodes `png_bytes` as a cursor-positioned, chunked Kitty Graphics Protocol sequence for `next_id`,
/// optionally deleting `prev_id` after placement inside the same synchronized update.
pub fn kitty_swap_image_png(
    png_bytes: &[u8],
    area: Rect,
    next_id: u32,
    prev_id: Option<u32>,
) -> String {
    if area.width == 0 || area.height == 0 || png_bytes.is_empty() {
        return String::new();
    }

    let encoded = BASE64_STANDARD.encode(png_bytes);
    let chunks: Vec<&[u8]> = encoded.as_bytes().chunks(KITTY_CHUNK_SIZE).collect();
    let total = chunks.len();

    let mut out = String::with_capacity(encoded.len() + 64 * total + 64);
    let _ = write!(out, "\x1b[?2026h\x1b7\x1b[{};{}H", area.y + 1, area.x + 1);

    for (idx, chunk) in chunks.into_iter().enumerate() {
        let more = usize::from(idx + 1 < total);
        let chunk_str = std::str::from_utf8(chunk).expect("base64 is valid ASCII");
        if idx == 0 {
            let _ = write!(
                out,
                "\x1b_Ga=T,f=100,i={next_id},p=1,c={},r={},C=1,q=2,m={more};{chunk_str}\x1b\\",
                area.width, area.height
            );
        } else {
            let _ = write!(out, "\x1b_Gm={more};{chunk_str}\x1b\\");
        }
    }

    if let Some(old) = prev_id
        && old != next_id
    {
        out.push_str(&kitty_clear_image_id(old));
    }

    out.push_str("\x1b8\x1b[?2026l");
    out
}

/// Encodes `png_bytes` as a cursor-positioned, chunked Kitty Graphics Protocol sequence for `image_id`.
pub fn kitty_transmit_and_place_png(png_bytes: &[u8], area: Rect, image_id: u32) -> String {
    kitty_swap_image_png(png_bytes, area, image_id, None)
}

/// Encodes `art.encoded_png` as a cursor-positioned, chunked Kitty Graphics Protocol sequence.
pub fn kitty_transmit_and_place(art: &CoverArt, area: Rect) -> String {
    kitty_transmit_and_place_png(&art.encoded_png, area, KITTY_COVER_IMAGE_ID)
}

/// Returns the Kitty Graphics Protocol escape sequence that deletes `image_id`.
pub fn kitty_clear_image_id(image_id: u32) -> String {
    format!("\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\")
}

/// Returns the Kitty Graphics Protocol escape sequence that deletes image ID 1.
pub fn kitty_clear_image() -> &'static str {
    "\x1b_Ga=d,d=I,i=1,q=2\x1b\\"
}

/// Renders a vertically centered progress bar with subtle rounded corners as PNG bytes.
pub fn render_progress_bar_png(
    cols: u16,
    ratio: f64,
    filled_rgb: [u8; 3],
    empty_rgb: [u8; 3],
) -> Vec<u8> {
    if cols == 0 {
        return Vec::new();
    }

    const CELL_H: u32 = 32;
    const BAR_Y_START: u32 = 8;
    const BAR_Y_END: u32 = 24;
    const BAR_H: u32 = BAR_Y_END - BAR_Y_START;
    const CORNER_RADIUS: f32 = 4.0;

    let w = u32::from(cols) * BAR_CELL_W;
    let h = CELL_H;
    let fill_exact = ratio.clamp(0.0, 1.0) * f64::from(w);

    let mut rgba_img = RgbaImage::new(w, h);
    for y in BAR_Y_START..BAR_Y_END {
        let local_y = y - BAR_Y_START;
        for x in 0..w {
            let d = corner_signed_distance(x, local_y, w, BAR_H, CORNER_RADIUS);
            let alpha = ((0.5 - d).clamp(0.0, 1.0) * 255.0).round() as u8;
            let coverage = (fill_exact - f64::from(x)).clamp(0.0, 1.0);
            let r = (f64::from(empty_rgb[0]) * (1.0 - coverage)
                + f64::from(filled_rgb[0]) * coverage)
                .round() as u8;
            let g = (f64::from(empty_rgb[1]) * (1.0 - coverage)
                + f64::from(filled_rgb[1]) * coverage)
                .round() as u8;
            let b = (f64::from(empty_rgb[2]) * (1.0 - coverage)
                + f64::from(filled_rgb[2]) * coverage)
                .round() as u8;
            rgba_img.put_pixel(x, y, Rgba([r, g, b, alpha]));
        }
    }

    let mut png_bytes = Vec::new();
    rgba_img
        .write_to(&mut Cursor::new(&mut png_bytes), ImageFormat::Png)
        .expect("PNG encoding of non-empty RGBA buffer should succeed");
    png_bytes
}

/// Errors produced when decoding or normalizing cover art images.
#[derive(Debug, Error)]
pub enum ArtError {
    #[error("failed to decode cover art image: {0}")]
    Decode(#[from] image::ImageError),
    #[error("cover art image has zero width or height")]
    EmptyImage,
}

/// Decodes raw JPEG or PNG bytes into a normalized `CoverArt` bounded to `512x512`.
pub fn decode_cover_art(track_id: TrackId, raw_bytes: &[u8]) -> Result<CoverArt, ArtError> {
    let mut img = image::load_from_memory(raw_bytes)?;
    if img.width() == 0 || img.height() == 0 {
        return Err(ArtError::EmptyImage);
    }

    if img.width() > MAX_COVER_DIMENSION || img.height() > MAX_COVER_DIMENSION {
        img = img.resize(
            MAX_COVER_DIMENSION,
            MAX_COVER_DIMENSION,
            FilterType::Triangle,
        );
    }

    let rgb_img = img.to_rgb8();
    let (width, height) = rgb_img.dimensions();
    if width == 0 || height == 0 {
        return Err(ArtError::EmptyImage);
    }

    let radius = (width.min(height) as f32 * 0.08).max(1.0);
    let mut rgba_img = RgbaImage::new(width, height);
    for (x, y, Rgb([r, g, b])) in rgb_img.enumerate_pixels() {
        let d = corner_signed_distance(x, y, width, height, radius);
        let alpha = ((0.5 - d).clamp(0.0, 1.0) * 255.0).round() as u8;
        rgba_img.put_pixel(x, y, Rgba([*r, *g, *b, alpha]));
    }

    let mut png_bytes = Vec::new();
    rgba_img.write_to(&mut Cursor::new(&mut png_bytes), ImageFormat::Png)?;

    Ok(CoverArt {
        track_id,
        width,
        height,
        rgb: Arc::from(rgb_img.into_raw()),
        encoded_png: Arc::from(png_bytes),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    fn encode_test_png(width: u32, height: u32) -> Vec<u8> {
        let mut img = RgbImage::new(width, height);
        for (x, y, pixel) in img.enumerate_pixels_mut() {
            *pixel = Rgb([(x % 256) as u8, (y % 256) as u8, 128]);
        }
        let mut bytes = Vec::new();
        img.write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .expect("encode test png");
        bytes
    }

    #[test]
    fn decode_cover_art_decodes_valid_png_with_rgb_and_png_bytes() {
        let track_id = TrackId::new("trk_1").unwrap();
        let raw = encode_test_png(16, 12);

        let art = decode_cover_art(track_id.clone(), &raw).unwrap();
        assert_eq!(art.track_id, track_id);
        assert_eq!(art.width, 16);
        assert_eq!(art.height, 12);
        assert_eq!(art.rgb.len(), (16 * 12 * 3) as usize);
        assert!(art.encoded_png.starts_with(b"\x89PNG"));
    }

    #[test]
    fn decode_cover_art_scales_down_oversized_images_to_max_512() {
        let track_id = TrackId::new("trk_2").unwrap();
        let raw = encode_test_png(800, 600);

        let art = decode_cover_art(track_id, &raw).unwrap();
        assert!(art.width <= MAX_COVER_DIMENSION);
        assert!(art.height <= MAX_COVER_DIMENSION);
        assert_eq!(art.width, 512);
        assert_eq!(art.height, 384);
        assert_eq!(art.rgb.len(), (art.width * art.height * 3) as usize);
    }

    #[test]
    fn decode_cover_art_rejects_invalid_bytes() {
        let track_id = TrackId::new("trk_3").unwrap();
        let err = decode_cover_art(track_id, b"not an image").unwrap_err();
        assert!(matches!(err, ArtError::Decode(_)));
    }

    #[test]
    fn detect_art_protocol_from_env_handles_terminals_and_tmux() {
        assert_eq!(
            detect_art_protocol_from_env(Some("xterm-ghostty"), None, None, Some("/usr"), false),
            ArtProtocol::Kitty
        );
        assert_eq!(
            detect_art_protocol_from_env(Some("xterm-kitty"), None, Some("1"), None, false),
            ArtProtocol::Kitty
        );
        assert_eq!(
            detect_art_protocol_from_env(
                Some("xterm-256color"),
                Some("WezTerm"),
                None,
                None,
                false
            ),
            ArtProtocol::Kitty
        );
        assert_eq!(
            detect_art_protocol_from_env(
                Some("xterm-256color"),
                Some("Terminator"),
                None,
                None,
                false
            ),
            ArtProtocol::HalfBlock
        );
        assert_eq!(
            detect_art_protocol_from_env(Some("xterm-kitty"), Some("kitty"), Some("1"), None, true),
            ArtProtocol::HalfBlock
        );
    }

    #[test]
    fn render_retro_art_lines_guards_zero_and_invalid_sizes() {
        let art = CoverArt {
            track_id: TrackId::new("trk_guard").unwrap(),
            width: 2,
            height: 2,
            rgb: Arc::from(vec![10; 2 * 2 * 3]),
            encoded_png: Arc::from(Vec::new()),
        };

        assert!(render_retro_art_lines(&art, 0, 2).is_empty());
        assert!(render_retro_art_lines(&art, 2, 0).is_empty());

        let zero_art = CoverArt {
            width: 0,
            rgb: Arc::from(Vec::new()),
            ..art.clone()
        };
        assert!(render_retro_art_lines(&zero_art, 2, 2).is_empty());

        let bad_art = CoverArt {
            rgb: Arc::from(vec![0; 3]),
            ..art
        };
        assert!(render_retro_art_lines(&bad_art, 2, 2).is_empty());
    }

    #[test]
    fn render_retro_art_lines_solid_color_fills_all_cells_with_square_edges() {
        let mut rgb = Vec::with_capacity(8 * 8 * 3);
        for _ in 0..(8 * 8) {
            rgb.extend_from_slice(&[120, 45, 200]);
        }
        let art = CoverArt {
            track_id: TrackId::new("trk_solid").unwrap(),
            width: 8,
            height: 8,
            rgb: Arc::from(rgb),
            encoded_png: Arc::from(Vec::new()),
        };

        let lines = render_retro_art_lines(&art, 6, 4);
        assert_eq!(lines.len(), 4);
        for line in &lines {
            assert_eq!(line.spans.len(), 6);
            for span in &line.spans {
                assert_eq!(span.content, "█");
                assert_eq!(span.style.fg, Some(Color::Rgb(120, 45, 200)));
                assert_eq!(span.style.bg, Some(Color::Rgb(120, 45, 200)));
            }
        }
    }

    #[test]
    fn render_retro_art_lines_vertical_split_uses_left_half_block() {
        let mut rgb = Vec::with_capacity(2 * 2 * 3);
        for _y in 0..2 {
            rgb.extend_from_slice(&[220, 20, 20]);
            rgb.extend_from_slice(&[20, 40, 220]);
        }
        let art = CoverArt {
            track_id: TrackId::new("trk_vsplit").unwrap(),
            width: 2,
            height: 2,
            rgb: Arc::from(rgb),
            encoded_png: Arc::from(Vec::new()),
        };

        let lines = render_retro_art_lines(&art, 1, 1);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans.len(), 1);
        assert_eq!(lines[0].spans[0].content, "▌");
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Rgb(220, 20, 20)));
        assert_eq!(lines[0].spans[0].style.bg, Some(Color::Rgb(20, 40, 220)));
    }

    #[test]
    fn render_retro_art_lines_horizontal_split_uses_upper_half_block() {
        let mut rgb = Vec::with_capacity(2 * 2 * 3);
        for _x in 0..2 {
            rgb.extend_from_slice(&[200, 10, 20]);
        }
        for _x in 0..2 {
            rgb.extend_from_slice(&[10, 30, 220]);
        }
        let art = CoverArt {
            track_id: TrackId::new("trk_hsplit").unwrap(),
            width: 2,
            height: 2,
            rgb: Arc::from(rgb),
            encoded_png: Arc::from(Vec::new()),
        };

        let lines = render_retro_art_lines(&art, 1, 1);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans.len(), 1);
        assert_eq!(lines[0].spans[0].content, "▀");
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Rgb(200, 10, 20)));
        assert_eq!(lines[0].spans[0].style.bg, Some(Color::Rgb(10, 30, 220)));
    }

    #[test]
    fn kitty_transmit_and_place_formats_single_and_multi_chunk_apc_and_guards_empty() {
        let empty_art = CoverArt {
            track_id: TrackId::new("trk_empty").unwrap(),
            width: 1,
            height: 1,
            rgb: Arc::from(vec![0, 0, 0]),
            encoded_png: Arc::from(Vec::new()),
        };
        assert_eq!(
            kitty_transmit_and_place(&empty_art, Rect::new(2, 3, 10, 5)),
            ""
        );

        let small_art = CoverArt {
            encoded_png: Arc::from(b"png-bytes".to_vec()),
            ..empty_art.clone()
        };
        assert_eq!(
            kitty_transmit_and_place(&small_art, Rect::new(0, 0, 0, 5)),
            ""
        );
        let single = kitty_transmit_and_place(&small_art, Rect::new(4, 2, 12, 6));
        assert!(
            single.starts_with(
                "\x1b[?2026h\x1b7\x1b[3;5H\x1b_Ga=T,f=100,i=1,p=1,c=12,r=6,C=1,q=2,m=0;"
            )
        );
        assert!(single.ends_with("\x1b\\\x1b8\x1b[?2026l"));
        assert!(!single.contains("\x1b_Gm="));

        let large_art = CoverArt {
            encoded_png: Arc::from(vec![0xAB; 4096]),
            ..empty_art
        };
        let multi = kitty_transmit_and_place(&large_art, Rect::new(1, 1, 8, 4));
        assert!(
            multi.starts_with(
                "\x1b[?2026h\x1b7\x1b[2;2H\x1b_Ga=T,f=100,i=1,p=1,c=8,r=4,C=1,q=2,m=1;"
            )
        );
        assert!(multi.contains("\x1b_Gm=0;"));
        assert!(multi.ends_with("\x1b\\\x1b8\x1b[?2026l"));
    }

    #[test]
    fn kitty_clear_image_returns_expected_sequence() {
        assert_eq!(kitty_clear_image(), "\x1b_Ga=d,d=I,i=1,q=2\x1b\\");
    }

    #[test]
    fn decode_cover_art_applies_rounded_corner_alpha_to_encoded_png() {
        let track_id = TrackId::new("trk_round").unwrap();
        let raw = encode_test_png(64, 64);
        let art = decode_cover_art(track_id, &raw).unwrap();

        let decoded = image::load_from_memory(&art.encoded_png)
            .unwrap()
            .into_rgba8();
        assert_eq!(decoded.get_pixel(0, 0).0[3], 0);
        assert_eq!(decoded.get_pixel(63, 0).0[3], 0);
        assert_eq!(decoded.get_pixel(0, 63).0[3], 0);
        assert_eq!(decoded.get_pixel(63, 63).0[3], 0);
        assert_eq!(decoded.get_pixel(32, 32).0[3], 255);
    }

    #[test]
    fn render_progress_bar_png_draws_subtly_rounded_split_bar_and_guards_zero_cols() {
        assert!(render_progress_bar_png(0, 0.5, [125, 207, 255], [86, 95, 137]).is_empty());

        let filled = [125, 207, 255];
        let empty = [86, 95, 137];
        let png = render_progress_bar_png(10, 80.5 / 160.0, filled, empty);
        let decoded = image::load_from_memory(&png).unwrap().into_rgba8();

        assert_eq!(decoded.dimensions(), (160, 32));
        assert_eq!(decoded.get_pixel(10, 0).0[3], 0);
        assert_eq!(decoded.get_pixel(10, 31).0[3], 0);
        assert_eq!(decoded.get_pixel(0, 8).0[3], 0);
        assert_eq!(decoded.get_pixel(159, 8).0[3], 0);
        assert_eq!(
            decoded.get_pixel(79, 16).0,
            [filled[0], filled[1], filled[2], 255]
        );
        assert_eq!(
            decoded.get_pixel(81, 16).0,
            [empty[0], empty[1], empty[2], 255]
        );
        let edge = decoded.get_pixel(80, 16).0;
        assert_eq!(edge[3], 255);
        for c in 0..3 {
            let lo = filled[c].min(empty[c]);
            let hi = filled[c].max(empty[c]);
            assert!(edge[c] > lo && edge[c] < hi);
        }
    }

    #[test]
    fn kitty_transmit_and_clear_support_explicit_image_id() {
        let seq = kitty_transmit_and_place_png(b"png", Rect::new(2, 4, 20, 1), KITTY_BAR_IMAGE_ID);
        assert!(
            seq.starts_with(
                "\x1b[?2026h\x1b7\x1b[5;3H\x1b_Ga=T,f=100,i=2,p=1,c=20,r=1,C=1,q=2,m=0;"
            )
        );
        assert!(seq.ends_with("\x1b\\\x1b8\x1b[?2026l"));
        assert_eq!(kitty_clear_image_id(2), "\x1b_Ga=d,d=I,i=2,q=2\x1b\\");
    }

    #[test]
    fn kitty_swap_image_png_places_next_before_deleting_prev_inside_sync_update() {
        let seq = kitty_swap_image_png(
            b"png",
            Rect::new(2, 4, 20, 1),
            KITTY_BAR_ALT_IMAGE_ID,
            Some(KITTY_BAR_IMAGE_ID),
        );
        assert!(seq.starts_with("\x1b[?2026h\x1b7\x1b[5;3H"));
        assert!(seq.ends_with("\x1b8\x1b[?2026l"));

        let place_idx = seq.find("a=T,f=100,i=3,").unwrap();
        let delete_idx = seq.find("\x1b_Ga=d,d=I,i=2,q=2\x1b\\").unwrap();
        let sync_end_idx = seq.find("\x1b8\x1b[?2026l").unwrap();
        assert!(place_idx < delete_idx);
        assert!(delete_idx < sync_end_idx);

        let same_id = kitty_swap_image_png(b"png", Rect::new(2, 4, 20, 1), 2, Some(2));
        assert!(!same_id.contains("a=d,d=I,i=2"));
    }
}
