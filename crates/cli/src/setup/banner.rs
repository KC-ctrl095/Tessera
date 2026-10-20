//! The setup banner: a band of tesserae that drop into place and settle,
//! then the wordmark is laid in gold. Like the apps' mosaic art, the band
//! is mostly stone with the odd
//! smalti tessera, thinning toward its ends.
//!
//! Tiles are drawn on a half-block pixel grid (two pixels per character
//! row): each tessera is 2×2 pixels, which is square on a typical terminal
//! font, with one pixel of grout between tiles.

use std::time::{Duration, Instant};

use super::term::{
    Depth, Ev, GOLD, LAPIS, PAD, RawGuard, Rgb, STONE, Sty, TERRACOTTA, Term, VERDIGRIS, paint_at,
    sgr,
};

/// Tile rows in the band.
const ROWS: usize = 3;
/// Pixel pitch of one tile (2 px tile + 1 px grout).
const PITCH: usize = 3;
const FALL_MS: f32 = 420.0;
const FRAME: Duration = Duration::from_millis(30);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tile {
    pub x: usize,
    pub y: usize,
    pub color: Rgb,
    /// When it starts to fall (ms).
    pub delay: f32,
    /// How far above its place it starts (px).
    pub drop: f32,
}

fn hash(a: usize, b: usize, salt: u32) -> u32 {
    let mut h = (a as u32).wrapping_mul(0x9E37_79B1) ^ (b as u32).wrapping_mul(0x85EB_CA77) ^ salt;
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^ (h >> 15)
}

/// The band's tiles for `cols` tiles across. Deterministic.
pub fn tiles(cols: usize) -> Vec<Tile> {
    let mut out = Vec::with_capacity(cols * ROWS);
    for x in 0..cols {
        for y in 0..ROWS {
            let h = hash(x, y, 7);
            // Distance from the middle of the band (0 centre, 1 ends).
            let t = x as f32 / (cols.max(2) - 1) as f32;
            let edge = (t - 0.5).abs() * 2.0;
            // Mostly stone; a few smalti, gold most often, toward the middle.
            let roll = h % 1000;
            let accent = if edge < 0.8 {
                match roll {
                    0..=59 => Some(GOLD),
                    60..=89 => Some(LAPIS),
                    90..=104 => Some(VERDIGRIS),
                    105..=114 => Some(TERRACOTTA),
                    _ => None,
                }
            } else {
                None
            };
            // Stones: the lighter ones in the middle, darker toward the ends.
            let stone = STONE[0].mix(STONE[2], (1.0 - edge) * 0.4 + ((h >> 12) % 5) as f32 / 14.0);
            let base = match accent {
                Some(a) => a.mix(stone, 0.18),
                None => stone,
            };
            // Hand-laid: every tessera is a little lighter or darker.
            let jitter = 0.9 + ((h >> 20) % 17) as f32 / 100.0;
            out.push(Tile {
                x,
                y,
                color: base.scale(jitter),
                // Columns fill left to right, each from the bottom up, so a
                // falling tile never passes through one already in place.
                delay: x as f32 * 30.0 + (ROWS - 1 - y) as f32 * 70.0 + ((h >> 4) % 60) as f32,
                // From above the band down to its row.
                drop: (y * PITCH) as f32 + 3.0 + ((h >> 16) % 3) as f32,
            });
        }
    }
    out
}

/// Total animation length for `tiles`.
pub fn duration_ms(tiles: &[Tile]) -> f32 {
    tiles.iter().map(|t| t.delay).fold(0.0, f32::max) + FALL_MS
}

/// Ease-out with a small overshoot: tiles land, dip and settle.
fn ease_out_back(p: f32) -> f32 {
    let c1 = 1.5;
    let c3 = c1 + 1.0;
    let q = p - 1.0;
    1.0 + c3 * q * q * q + c1 * q * q
}

/// Pixel grid (`[row][col]`) at time `t` ms.
pub fn pixels(tiles: &[Tile], cols: usize, t: f32) -> Vec<Vec<Option<Rgb>>> {
    let w = cols * PITCH - 1;
    let h = ROWS * PITCH - 1;
    let mut px = vec![vec![None; w]; h];
    for tile in tiles {
        let p = ((t - tile.delay) / FALL_MS).clamp(0.0, 1.0);
        if t < tile.delay {
            continue;
        }
        let offset = (1.0 - ease_out_back(p)) * tile.drop;
        let y0 = (tile.y * PITCH) as f32 - offset;
        let y0 = y0.round() as i32;
        // Tiles brighten as they land.
        let c = tile.color.mix(STONE[0], (1.0 - p) * 0.55);
        for dy in 0..2 {
            let y = y0 + dy;
            if y < 0 || y as usize >= h {
                continue;
            }
            for dx in 0..2 {
                px[y as usize][tile.x * PITCH + dx] = Some(c);
            }
        }
    }
    px
}

/// Text rows for a pixel grid (two pixel rows per text row).
pub fn render(px: &[Vec<Option<Rgb>>], depth: Depth) -> Vec<String> {
    let mut rows = Vec::new();
    let w = px.first().map(|r| r.len()).unwrap_or(0);
    for r in (0..px.len()).step_by(2) {
        let mut line = String::new();
        let mut cur = String::new();
        for c in 0..w {
            let top = px[r][c];
            let bot = px.get(r + 1).and_then(|row| row[c]);
            let (ch, st) = match (top, bot) {
                (None, None) => (' ', Sty::plain()),
                (Some(a), None) => ('▀', Sty::fg(a)),
                (None, Some(b)) => ('▄', Sty::fg(b)),
                (Some(a), Some(b)) if a == b => ('█', Sty::fg(a)),
                (Some(a), Some(b)) => ('▀', Sty::fg(a).on(b)),
            };
            let code = sgr(st, depth);
            if code != cur {
                if !cur.is_empty() {
                    line.push_str("\x1b[0m");
                }
                line.push_str(&code);
                cur = code;
            }
            line.push(ch);
        }
        if !cur.is_empty() {
            line.push_str("\x1b[0m");
        }
        rows.push(line);
    }
    rows
}

/// Wordmark rows at time `t` ms after it starts: letters are laid left to
/// right with a bright glint that cools to gold.
fn wordmark(t: f32, depth: Depth) -> Vec<String> {
    crate::banner::WORDMARK
        .iter()
        .map(|row| {
            let mut s = String::new();
            for (i, ch) in row.chars().enumerate() {
                let at = i as f32 * 11.0;
                if t < at {
                    s.push(' ');
                    continue;
                }
                let age = ((t - at) / 220.0).clamp(0.0, 1.0);
                let c = STONE[5].mix(GOLD, age);
                if ch == ' ' {
                    s.push(' ');
                } else {
                    s.push_str(&paint_at(&ch.to_string(), Sty::fg(c).bold(), depth));
                }
            }
            s
        })
        .collect()
}

const WORDMARK_MS: f32 = 27.0 * 11.0 + 220.0;

/// Tiles across for a terminal `cols` wide.
pub fn band_cols(cols: usize) -> usize {
    ((cols.saturating_sub(6)) / PITCH).clamp(8, 18)
}

fn frame_lines(
    tiles: &[Tile],
    n: usize,
    t_band: f32,
    t_word: Option<f32>,
    depth: Depth,
) -> Vec<String> {
    let mut lines: Vec<String> = render(&pixels(tiles, n, t_band), depth)
        .into_iter()
        .map(|l| format!("{PAD}{l}"))
        .collect();
    lines.push(String::new());
    match t_word {
        Some(t) => lines.extend(wordmark(t, depth).into_iter().map(|l| format!("{PAD}{l}"))),
        None => lines.extend(std::iter::repeat_n(
            String::new(),
            crate::banner::WORDMARK.len(),
        )),
    }
    lines
}

/// Draw the banner (animated when the terminal allows it).
pub async fn play(term: &mut Term, subtitle: &str) -> anyhow::Result<()> {
    let depth = term.caps.depth;
    let version = env!("CARGO_PKG_VERSION");
    if depth == Depth::None || !term.caps.fancy || term.cols() < 40 {
        println!();
        if term.caps.fancy && term.cols() >= 40 {
            // No colours: the wordmark still reads in plain blocks.
            for l in crate::banner::WORDMARK {
                println!("{PAD}{l}");
            }
            println!("{PAD}v{version} · {subtitle}");
        } else {
            println!("{PAD}tessera {version} — {subtitle}");
        }
        println!();
        return Ok(());
    }
    let n = band_cols(term.cols());
    let tiles = tiles(n);
    let band_ms = duration_ms(&tiles);
    println!();
    if term.caps.anim {
        let _raw = RawGuard::new(false, true);
        let start = Instant::now();
        loop {
            let t = start.elapsed().as_secs_f32() * 1000.0;
            // The wordmark starts while the last tiles are still settling.
            let word_start = band_ms - 250.0;
            let tw = (t >= word_start).then_some(t - word_start);
            term.draw_live(&frame_lines(&tiles, n, t, tw, depth));
            if tw.is_some_and(|w| w >= WORDMARK_MS) {
                break;
            }
            // Any key skips to the end; Ctrl+C stops.
            if let Ev::Key(_) = term.next(Some(FRAME)).await? {
                term.draw_live(&frame_lines(&tiles, n, 1e9, Some(1e9), depth));
                break;
            }
        }
        term.commit_live();
    } else {
        for l in frame_lines(&tiles, n, 1e9, Some(1e9), depth) {
            println!("{l}");
        }
    }
    println!(
        "{PAD}{}",
        paint_at(
            &format!("v{version} · {subtitle}"),
            Sty::fg(STONE[3]),
            depth
        )
    );
    println!();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::term::strip_ansi;
    use super::*;

    #[test]
    fn tiles_settle_exactly_into_place() {
        let n = band_cols(80);
        let t = tiles(n);
        assert_eq!(t.len(), n * ROWS);
        let end = pixels(&t, n, duration_ms(&t) + 1.0);
        // Every tile covers its own 2×2 block; grout stays empty.
        for tile in &t {
            for dy in 0..2 {
                for dx in 0..2 {
                    assert!(end[tile.y * PITCH + dy][tile.x * PITCH + dx].is_some());
                }
            }
        }
        for row in &end {
            for x in (2..row.len()).step_by(PITCH) {
                assert!(row[x].is_none(), "grout column {x}");
            }
        }
        assert!(
            end[2].iter().chain(&end[5]).all(Option::is_none),
            "grout rows"
        );
        // Every tile starts above the band.
        assert_eq!(
            pixels(&t, n, 0.0)
                .iter()
                .flatten()
                .filter(|p| p.is_some())
                .count(),
            0
        );
    }

    #[test]
    fn frames_render_at_every_depth_and_width() {
        for cols in [40, 80, 200] {
            let n = band_cols(cols);
            let t = tiles(n);
            for depth in [Depth::Ansi16, Depth::Ansi256, Depth::True] {
                for ms in [0.0, 150.0, 500.0, 5000.0] {
                    let lines = frame_lines(&t, n, ms, Some(ms), depth);
                    assert_eq!(lines.len(), 4 + 1 + 2);
                    for l in &lines {
                        assert!(
                            super::super::term::width(l) <= cols,
                            "{cols}: {}",
                            strip_ansi(l)
                        );
                    }
                }
            }
        }
        let rows = render(&pixels(&tiles(8), 8, 1e9), Depth::True);
        let plain: String = rows.iter().map(|r| strip_ansi(r)).collect();
        assert!(plain.contains('█') && plain.contains('▀'), "{plain}");
    }
}
