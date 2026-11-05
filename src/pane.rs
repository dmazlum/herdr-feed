use crate::fetch::get_bytes;
use crate::{Config, Item, build_queue, cache_dir, config, log, mark_seen, now, read_state};
use base64::Engine;
use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use regex::Regex;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

// ---------------------------------------------------------------- output

static OUT: Mutex<()> = Mutex::new(());

fn out(b: &[u8]) {
    let _g = OUT.lock().unwrap();
    let mut so = std::io::stdout().lock();
    let _ = so.write_all(b);
    let _ = so.flush();
}

struct Term {
    cols: u16,
    rows: u16,
    cw: f64,
    ch: f64,
}

fn term() -> Term {
    let ws = crossterm::terminal::window_size().ok();
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let (w, h) = ws.map(|w| (w.width, w.height)).unwrap_or((0, 0));
    Term {
        // layout math subtracts margins from these; a squeezed pane draws clipped, not wrapped
        cols: cols.max(MIN_COLS),
        rows: rows.max(MIN_ROWS),
        cw: if w > 0 && cols > 0 {
            w as f64 / cols as f64
        } else {
            9.0
        },
        ch: if h > 0 && rows > 0 {
            h as f64 / rows as f64
        } else {
            18.0
        },
    }
}

/// The smallest pane the card layout is computed for.
const MIN_COLS: u16 = 24;
const MIN_ROWS: u16 = 8;

fn kitty(data: &[u8], ctrl: &str) -> Vec<u8> {
    let b64 = base64::engine::general_purpose::STANDARD.encode(data);
    let chunks: Vec<&[u8]> = b64.as_bytes().chunks(4096).collect();
    let mut v = Vec::with_capacity(b64.len() + 64 * chunks.len());
    for (i, c) in chunks.iter().enumerate() {
        let m = (i + 1 < chunks.len()) as u8;
        if i == 0 {
            v.extend_from_slice(format!("\x1b_G{ctrl},m={m};").as_bytes());
        } else {
            v.extend_from_slice(format!("\x1b_Gm={m};").as_bytes());
        }
        v.extend_from_slice(c);
        v.extend_from_slice(b"\x1b\\");
    }
    v
}

const KITTY_CLEAR: &str = "\x1b_Ga=d,d=A,q=2\x1b\\";

/// Largest (cols, rows) box with the image's aspect inside max_cols × max_rows.
fn fit(wpx: f64, hpx: f64, max_cols: u16, max_rows: u16, t: &Term) -> (u16, u16) {
    let mut cols = max_cols as f64;
    let mut rows = (cols * t.cw * hpx / wpx / t.ch).round();
    if rows > max_rows as f64 {
        rows = max_rows as f64;
        cols = (rows * t.ch * wpx / hpx / t.cw).round();
    }
    (cols.max(1.0) as u16, rows.max(1.0) as u16)
}

// ---------------------------------------------------------------- media prep

#[derive(Clone)]
enum Prep {
    Pending,
    Ready(Option<(PathBuf, u32, u32)>),
}

fn png_dims(p: &PathBuf) -> Option<(u32, u32)> {
    let mut f = std::fs::File::open(p).ok()?;
    let mut h = [0u8; 24];
    f.read_exact(&mut h).ok()?;
    if &h[1..4] != b"PNG" {
        return None;
    }
    Some((
        u32::from_be_bytes(h[16..20].try_into().ok()?),
        u32::from_be_bytes(h[20..24].try_into().ok()?),
    ))
}

fn prep_image(url: &str) -> Option<(PathBuf, u32, u32)> {
    let dir = cache_dir().join("media");
    let _ = std::fs::create_dir_all(&dir);
    let hash = sha1_smol::Sha1::from(url).digest().to_string();
    let png = dir.join(format!("{}.png", &hash[..20]));
    if !png.exists() {
        let raw = dir.join(format!("{}.src", &hash[..20]));
        let bytes = get_bytes(url, 15_000_000)
            .map_err(|e| log(&format!("img fail {url}: {e}")))
            .ok()?;
        std::fs::write(&raw, bytes).ok()?;
        let ok = Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-i"])
            .arg(&raw)
            .args(["-frames:v", "1", "-vf", "scale='min(900,iw)':-2"])
            .arg(&png)
            .stdin(Stdio::null())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        let _ = std::fs::remove_file(&raw);
        if !ok {
            return None;
        }
    }
    let (w, h) = png_dims(&png)?;
    Some((png, w, h))
}

// ---------------------------------------------------------------- video (ffmpeg → kitty frames)
//
// mpv --vo=kitty sends every frame at full pixel size as base64 (MBs per frame on Retina), which
// stutters. Instead ffmpeg decodes small frames (≤ 640px wide), each is zlib-compressed and sent
// with a fixed image id, and the terminal scales it to the cell box. We pace frames ourselves, so
// pausing just stops reading (ffmpeg blocks on the pipe) and resuming carries on smoothly.

// Each frame gets a new image id; the old one is deleted only after the new one is placed, so
// there's never a gap between frames (re-sending one id deletes, then redraws: flicker).
const VIDEO_ID_BASE: u32 = 10_000;
const FPS: f64 = 15.0;

struct Player {
    procs: Vec<Child>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    pause: Arc<std::sync::atomic::AtomicBool>,
    finished: Arc<std::sync::atomic::AtomicBool>,
    frames: Arc<std::sync::atomic::AtomicU32>,
    /// Playing time so far, not counting pauses: `active` banked, plus the run since `since`.
    active: Duration,
    since: Option<std::time::Instant>,
    thread: Option<JoinHandle<()>>,
}

impl Player {
    #[allow(clippy::too_many_arguments)]
    fn start(
        url: &str,
        top: u16,
        left: u16,
        cols: u16,
        rows: u16,
        t: &Term,
        cfg: &Config,
    ) -> Option<Player> {
        use std::sync::atomic::{AtomicBool, Ordering};
        let box_w = cols as f64 * t.cw;
        let box_h = rows as f64 * t.ch;
        let w = (box_w.min(640.0) as u32) & !1;
        let h = ((w as f64 * box_h / box_w) as u32) & !1;
        log(&format!(
            "video start {url} box {cols}x{rows} cells, {box_w:.0}x{box_h:.0}px"
        ));
        if w < 16 || h < 16 {
            return None;
        }
        let cached = crate::fetch::video_cache(url);
        let url_owned = if cached.exists() {
            cached.display().to_string()
        } else {
            url.to_string()
        };
        let url = url_owned.as_str();
        let direct = cached.exists() || crate::fetch::is_direct_video(url);
        let mut procs = Vec::new();
        // YouTube's servers 403 a plain fetch; yt-dlp streams it to us instead
        let mut feeder = if direct {
            None
        } else {
            Command::new("yt-dlp")
                .args([
                    "-q",
                    "--no-warnings",
                    "--no-part",
                    "-f",
                    "18/b[height<=480]/b/bv*",
                    "-o",
                    "-",
                    "--",
                    url,
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| log(&format!("yt-dlp spawn: {e}")))
                .ok()
        };
        let input = if direct {
            url.to_string()
        } else {
            "-".to_string()
        };
        let vf = format!(
            "fps={FPS},scale={w}:{h}:force_original_aspect_ratio=decrease:flags=fast_bilinear,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2"
        );
        let mut ff = Command::new("ffmpeg")
            .args([
                "-loglevel",
                "error",
                "-t",
                &cfg.video_max.to_string(),
                "-i",
                &input,
                "-an",
                "-vf",
                &vf,
            ])
            .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
            .stdin(
                feeder
                    .as_mut()
                    .and_then(|f| f.stdout.take())
                    .map(Stdio::from)
                    .unwrap_or_else(Stdio::null),
            )
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| log(&format!("ffmpeg spawn: {e}")))
            .ok()?;
        let mut src = ff.stdout.take()?;
        procs.push(ff);
        procs.extend(feeder);

        let stop = Arc::new(AtomicBool::new(false));
        let pause = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let frames_n = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let (st, pa, fi, fr) = (
            stop.clone(),
            pause.clone(),
            finished.clone(),
            frames_n.clone(),
        );
        let thread = std::thread::spawn(move || {
            let size = (w * h * 3) as usize;
            let mut frame = vec![0u8; size];
            let tick = Duration::from_secs_f64(1.0 / FPS);
            let mut next = std::time::Instant::now();
            let (t0, mut frames) = (std::time::Instant::now(), 0u32);
            while !st.load(Ordering::Relaxed) {
                if pa.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(50));
                    next = std::time::Instant::now();
                    continue;
                }
                if src.read_exact(&mut frame).is_err() {
                    break;
                }
                let mut z =
                    flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
                if z.write_all(&frame).is_err() {
                    break;
                }
                let Ok(data) = z.finish() else { break };
                let id = VIDEO_ID_BASE + frames;
                let ctrl = format!("a=T,i={id},p=1,f=24,o=z,s={w},v={h},c={cols},r={rows},C=1,q=2");
                let mut b = format!("\x1b7\x1b[{top};{left}H").into_bytes();
                b.extend(kitty(&data, &ctrl));
                if frames > 0 {
                    b.extend(format!("\x1b_Ga=d,d=I,i={},q=2\x1b\\", id - 1).into_bytes());
                }
                b.extend(b"\x1b8");
                if st.load(Ordering::Relaxed) {
                    break;
                }
                out(&b);
                frames += 1;
                fr.store(frames, Ordering::Relaxed);
                next += tick;
                let now = std::time::Instant::now();
                if next > now {
                    std::thread::sleep(next - now);
                } else {
                    next = now; // fell behind: don't burst to catch up
                }
            }
            log(&format!(
                "video {w}x{h}: {frames} frames in {:.1}s",
                t0.elapsed().as_secs_f64()
            ));
            fi.store(true, Ordering::Relaxed);
        });
        Some(Player {
            procs,
            stop,
            pause,
            finished,
            frames: frames_n,
            active: Duration::ZERO,
            since: Some(std::time::Instant::now()),
            thread: Some(thread),
        })
    }

    fn set_paused(&mut self, p: bool) {
        self.pause.store(p, std::sync::atomic::Ordering::Relaxed);
        match (p, self.since) {
            (true, Some(t)) => {
                self.active += t.elapsed();
                self.since = None;
            }
            (false, None) => self.since = Some(std::time::Instant::now()),
            _ => {}
        }
    }

    fn played(&self) -> Duration {
        self.active + self.since.map_or(Duration::ZERO, |t| t.elapsed())
    }

    /// Finished, or still nothing to show 12s of playing time in (slow or failed source): give
    /// the card back. Time spent paused doesn't count, so a card drawn while paused keeps its clip.
    fn done(&mut self) -> bool {
        use std::sync::atomic::Ordering;
        self.finished.load(Ordering::Relaxed)
            || (self.frames.load(Ordering::Relaxed) == 0 && self.played() > Duration::from_secs(12))
    }

    fn stop(mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        for p in &mut self.procs {
            let _ = p.kill();
            let _ = p.wait();
        }
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
        let last = VIDEO_ID_BASE + self.frames.load(std::sync::atomic::Ordering::Relaxed);
        out(format!(
            "\x1b_Ga=d,d=I,i={},q=2\x1b\\\x1b_Ga=d,d=I,i={last},q=2\x1b\\",
            last.saturating_sub(1)
        )
        .as_bytes());
    }
}

// ---------------------------------------------------------------- code highlighting

const KEYWORDS: &[&str] = &[
    "fn",
    "let",
    "mut",
    "pub",
    "use",
    "impl",
    "struct",
    "enum",
    "match",
    "if",
    "else",
    "for",
    "while",
    "loop",
    "return",
    "def",
    "class",
    "import",
    "from",
    "as",
    "in",
    "const",
    "var",
    "function",
    "async",
    "await",
    "export",
    "default",
    "new",
    "true",
    "false",
    "null",
    "None",
    "True",
    "False",
    "self",
    "this",
    "select",
    "SELECT",
    "FROM",
    "WHERE",
    "JOIN",
    "ON",
    "GROUP",
    "BY",
    "ORDER",
    "INSERT",
    "UPDATE",
    "type",
    "interface",
    "then",
    "fi",
    "do",
    "done",
    "echo",
    "sudo",
    "git",
    "cd",
    "export",
    "with",
    "try",
    "catch",
    "func",
    "package",
];

/// Small regex-free tokenizer: comments, strings, numbers, keywords, flags, $ prompt.
fn highlight(code: &str, width: usize) -> String {
    let code: String = code.replace('\t', "    ");
    let code = if code.chars().count() > width.saturating_sub(2) {
        format!(
            "{}…",
            code.chars()
                .take(width.saturating_sub(3))
                .collect::<String>()
        )
    } else {
        code
    };
    let pad = width.saturating_sub(code.chars().count() + 2);
    const BG: &str = "\x1b[48;5;235m";
    let c = |n: u8| format!("\x1b[38;5;{n}m");
    let mut out = format!("{BG} ");
    let chars: Vec<char> = code.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        let rest: String = chars[i..].iter().collect();
        if i == 0 && rest.starts_with("$ ") {
            out += &format!("{}$ {}", c(244), c(252));
            i += 2;
        } else if rest.starts_with("//")
            || (ch == '#' && (i == 0 || chars[i - 1] == ' '))
            || rest.starts_with("-- ")
        {
            out += &format!("{}{rest}", c(244));
            break;
        } else if ch == '"' || ch == '\'' || ch == '`' {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != ch {
                j += if chars[j] == '\\' { 2 } else { 1 };
            }
            let j = (j + 1).min(chars.len());
            out += &format!("{}{}", c(150), chars[i..j].iter().collect::<String>());
            i = j;
        } else if ch.is_ascii_digit() && (i == 0 || !chars[i - 1].is_alphanumeric()) {
            let mut j = i;
            while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '.') {
                j += 1;
            }
            out += &format!("{}{}", c(215), chars[i..j].iter().collect::<String>());
            i = j;
        } else if ch == '-'
            && (i == 0 || chars[i - 1] == ' ')
            && chars
                .get(i + 1)
                .is_some_and(|x| x.is_alphanumeric() || *x == '-')
        {
            let mut j = i;
            while j < chars.len() && chars[j] != ' ' && chars[j] != '=' {
                j += 1;
            }
            out += &format!("{}{}", c(117), chars[i..j].iter().collect::<String>());
            i = j;
        } else if ch.is_alphabetic() || ch == '_' {
            let mut j = i;
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            let color = if KEYWORDS.contains(&word.as_str()) {
                176
            } else if chars.get(j) == Some(&'(') {
                111
            } else if word.chars().next().is_some_and(|x| x.is_uppercase()) {
                180
            } else {
                252
            };
            out += &format!("{}{word}", c(color));
            i = j;
        } else {
            out += &format!("{}{ch}", c(248));
            i += 1;
        }
    }
    out + &format!("{BG}{}{RST}", " ".repeat(pad + 1))
}

// ---------------------------------------------------------------- look

fn accent(it: &Item) -> (u8, u8, u8) {
    match it.source.as_str() {
        "Hacker News" => (255, 122, 26),
        "Lobsters" => (230, 86, 76),
        "GitHub Trending" => (200, 210, 225),
        "Claude Code" => (217, 119, 87),
        "TLDR Tech" => (80, 150, 255),
        "TLDR AI" => (155, 120, 255),
        "Fireship" => (250, 180, 40),
        "Simon Willison" => (90, 200, 190),
        "Product Hunt" => (255, 97, 84),
        "DEV" => (59, 73, 223),
        "The Verge" => (230, 60, 230),
        "Ars Technica" => (255, 140, 60),
        "TechCrunch" => (10, 200, 120),
        "Show HN" => (255, 122, 26),
        "OpenAI" => (16, 163, 127),
        "Hugging Face" => (255, 210, 30),
        "Vercel" => (235, 235, 235),
        "Cloudflare" => (246, 130, 31),
        _ if it.kind == "video" => (255, 50, 50),
        "tldr" => (90, 220, 140),
        s if s.starts_with("r/") => (255, 90, 40),
        _ if it.kind == "tip" => (120, 220, 170),
        _ => (120, 180, 255),
    }
}

/// Card surface: the dark background lifted a little and tinted by the source's accent.
fn card_bg(it: &Item) -> (u8, u8, u8) {
    let (r, g, b) = accent(it);
    let mix = |base: f64, c: u8| (base + c as f64 * 0.025) as u8;
    (mix(26.0, r), mix(26.0, g), mix(29.0, b))
}

fn bg_sgr((r, g, b): (u8, u8, u8)) -> String {
    format!("\x1b[48;2;{r};{g};{b}m")
}

/// Keep the card colour under text: every reset falls back to the card background.
fn on_card(s: &str, bg: (u8, u8, u8)) -> String {
    let sgr = bg_sgr(bg);
    format!("{sgr}{}", s.replace(RST, &format!("{RST}{sgr}")))
}

/// Minimal RGBA PNG encoder (stored deflate blocks); the terminal takes f=100 PNGs but not raw f=32.
fn png_rgba(w: usize, h: usize, px: &[u8]) -> Vec<u8> {
    fn crc(data: &[u8]) -> u32 {
        let mut c = 0xffff_ffffu32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xedb8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
        !c
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        out.extend((data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend(&body);
        out.extend(crc(&body).to_be_bytes());
    }
    let mut raw = Vec::with_capacity((w * 4 + 1) * h);
    for row in px.chunks(w * 4) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65535).collect();
    for (i, b) in blocks.iter().enumerate() {
        z.push((i + 1 == blocks.len()) as u8);
        z.extend((b.len() as u16).to_le_bytes());
        z.extend((!(b.len() as u16)).to_le_bytes());
        z.extend_from_slice(b);
    }
    let (mut a, mut bb) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65521;
        bb = (bb + a) % 65521;
    }
    z.extend(((bb << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend((w as u32).to_be_bytes());
    ihdr.extend((h as u32).to_be_bytes());
    ihdr.extend([8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// Anti-aliased quarter-circle corner as a cell-sized RGBA kitty image.
fn corner(cw: f64, ch: f64, bg: (u8, u8, u8), right: bool, bottom: bool, id: u32) -> Vec<u8> {
    let (w, h) = (cw.round().max(2.0) as usize, ch.round().max(2.0) as usize);
    let r = cw.min(ch) * 0.9;
    let mut px = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            // distance into the corner, measured from the rounded edge's centre
            let fx = if right {
                x as f64 + 0.5
            } else {
                w as f64 - x as f64 - 0.5
            };
            let fy = if bottom {
                y as f64 + 0.5
            } else {
                h as f64 - y as f64 - 0.5
            };
            let (dx, dy) = (
                (fx - (w as f64 - r)).max(0.0),
                (fy - (h as f64 - r)).max(0.0),
            );
            let a = (r - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0);
            px.extend_from_slice(&[bg.0, bg.1, bg.2, (a * 255.0) as u8]);
        }
    }
    let mut out = Vec::new();
    out.extend(kitty(
        &png_rgba(w, h, &px),
        &format!("a=T,f=100,i={id},p=1,c=1,r=1,C=1,q=2"),
    ));
    out
}

/// The four rounded corners, with fixed image ids so redrawing replaces them.
/// mpv deletes images when a video starts, so they're redrawn while it plays.
fn corners(cols: usize, rows: usize, bg: (u8, u8, u8), t: &Term) -> Vec<u8> {
    let mut b = b"\x1b7".to_vec();
    for (id, (r, c, right, bottom)) in [
        (1, 2, false, false),
        (1, cols - 1, true, false),
        (rows - 1, 2, false, true),
        (rows - 1, cols - 1, true, true),
    ]
    .into_iter()
    .enumerate()
    {
        b.extend(format!("\x1b[{r};{c}H").into_bytes());
        b.extend(corner(t.cw, t.ch, bg, right, bottom, 901 + id as u32));
    }
    b.extend(b"\x1b8");
    b
}

/// Fill the card area (1-cell margin) and stamp rounded corners on it.
fn card(cols: usize, rows: usize, bg: (u8, u8, u8), t: &Term) -> Vec<u8> {
    let sgr = bg_sgr(bg);
    let inner = cols.saturating_sub(2);
    let mut s = String::new();
    for r in 1..rows {
        // corner cells stay terminal-coloured; the corner image paints the curve
        let (start, len) = if r == 1 || r == rows - 1 {
            (3, inner.saturating_sub(2))
        } else {
            (2, inner)
        };
        s += &format!("\x1b[{r};{start}H{sgr}{}{RST}", " ".repeat(len));
    }
    let mut b = s.into_bytes();
    b.extend(corners(cols, rows, bg, t));
    b
}

// ---------------------------------------------------------------- pane

const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RST: &str = "\x1b[0m";

fn ago(ts: f64) -> String {
    let d = (now() - ts).max(0.0);
    if d < 3600.0 {
        format!("{}m", (d / 60.0).max(1.0) as u64)
    } else if d < 86400.0 {
        format!("{}h", (d / 3600.0) as u64)
    } else {
        format!("{}d", (d / 86400.0) as u64)
    }
}

struct Pane {
    key: String,
    cfg: Config,
    queue: Vec<Item>,
    i: usize,
    prep: Arc<Mutex<HashMap<usize, Prep>>>,
    player: Option<Player>,
    elapsed: f64,
    /// Everything stops, video included: `space`, or the person is in another tab.
    paused: bool,
    manual_pause: bool,
    /// The person is in the feed: cards stop moving on their own, videos keep playing.
    browsing: bool,
    focused: bool,
    /// The card was drawn before its picture was ready; draw it again once it is.
    awaiting_picture: bool,
    link_row: usize, // the link sits right under the content, not pinned to the bottom
    /// Screen rows (1-based) whose click opens the card's link: the title and the picture or
    /// video. The link row itself is added at click time (the footer may move it).
    opens: Vec<std::ops::Range<usize>>,
    /// The mouse pointer is a hand (over something a click opens).
    hand: bool,
}

impl Pane {
    /// Get card `idx` ready (its picture downloaded and converted) on a thread of its own.
    fn prepare(&self, idx: usize) {
        if let Some(job) = claim(&self.prep, &self.queue, idx) {
            std::thread::spawn(job);
        }
    }

    /// Get cards `from..` ready in the background, PREP_WORKERS at a time, so browsing with n/p
    /// finds their pictures done. Cards already claimed (the neighbours) are skipped.
    fn warm(&self, from: usize) {
        let next = Arc::new(std::sync::atomic::AtomicUsize::new(from));
        let queue = Arc::new(self.queue.clone());
        for _ in 0..PREP_WORKERS {
            let (next, queue, prep) = (next.clone(), queue.clone(), self.prep.clone());
            std::thread::spawn(move || {
                loop {
                    let idx = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if idx >= queue.len() {
                        break;
                    }
                    if let Some(job) = claim(&prep, &queue, idx) {
                        job();
                    }
                }
            });
        }
    }

    /// The picture for card `idx` if it's ready, waiting at most `ms` for it.
    fn picture(&self, idx: usize, ms: u64) -> (Option<(PathBuf, u32, u32)>, bool) {
        let t0 = now();
        loop {
            match self.prep.lock().unwrap().get(&idx) {
                Some(Prep::Ready(r)) => return (r.clone(), true),
                None => return (None, true),
                _ => {}
            }
            if (now() - t0) * 1000.0 >= ms as f64 {
                return (None, false);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Set the mouse pointer's shape (OSC 22), only when it changes.
    fn set_hand(&mut self, hand: bool) {
        if hand != self.hand {
            self.hand = hand;
            out(if hand { POINTER_HAND } else { POINTER_DEFAULT });
            self.footer();
        }
    }

    fn open_link(&self) {
        let u = &self.queue[self.i].url;
        if u.starts_with("https://") || u.starts_with("http://") {
            let _ = Command::new(OPENER).arg(u).spawn();
        }
    }

    /// Whether a click on screen row `row` (0-based, as the terminal reports it) opens the link.
    fn opens_at(&self, row: u16) -> bool {
        let row = row as usize + 1;
        let rows = term().rows as usize;
        row == self.link_row.min(rows - 5) || self.opens.iter().any(|r| r.contains(&row))
    }

    fn picture_ready(&self, idx: usize) -> bool {
        matches!(self.prep.lock().unwrap().get(&idx), Some(Prep::Ready(_)))
    }

    fn draw(&mut self) {
        if let Some(p) = self.player.take() {
            p.stop();
        }
        let t = term();
        let it = self.queue[self.i].clone();
        let (cols, rows) = (t.cols as usize, t.rows as usize);
        let width = cols.saturating_sub(8).max(20);
        let (ar, ag, ab) = accent(&it);
        let fg = |r: u8, g: u8, b: u8| format!("\x1b[38;2;{r};{g};{b}m");
        // Don't hold the card back for its picture: a brief wait covers pictures already on disk;
        // otherwise draw the text now and the picture when it lands (the run loop redraws).
        // A video card's picture is only its poster, so the clip doesn't wait for it either.
        let (img, ready) = self.picture(self.i, PICTURE_WAIT_MS);
        self.awaiting_picture = !ready && it.video.is_none();
        let has_media = img.is_some() || it.video.is_some();

        // header: source pill · age ............ KIND
        let kind = match it.kind.as_str() {
            "tip" => "TIP",
            "release" => "RELEASE",
            "product" => "PRODUCT",
            _ if it.video.is_some() => "▶ VIDEO",
            _ => "NEWS",
        };
        let pill = format!(
            "\x1b[48;2;{ar};{ag};{ab}m\x1b[38;2;12;12;12m{BOLD} {} {RST}",
            it.source
        );
        let left_w = it.source.chars().count() + 2 + 3 + ago(it.ts).len();
        let gap = width.saturating_sub(left_w + kind.chars().count());
        let mut lines = vec![
            crate::logo::badge(),
            String::new(),
            format!(
                "{pill} {DIM}· {}{RST}{}{}{kind}{RST}",
                ago(it.ts),
                " ".repeat(gap),
                fg(ar, ag, ab)
            ),
            String::new(),
        ];
        let title_at = lines.len() + 2; // lines[n] is drawn on row n + 2
        for l in textwrap::wrap(&it.title, width).into_iter().take(3) {
            lines.push(format!("{BOLD}\x1b[38;2;245;245;245m{l}{RST}"));
        }
        let mut body: Vec<String> = Vec::new();
        // paragraphs: drop link-only / sponsor lines, and leave a blank line between paragraphs
        static JUNK: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
        let junk = JUNK.get_or_init(|| Regex::new(r"(?i)^\W*(https?://\S+|.*\b(sponsor|sponsored|patreon|discord|twitch|twitter|merch|newsletter)\b.*)$").unwrap());
        let paras: Vec<&str> = it
            .body
            .lines()
            .map(str::trim_end)
            .filter(|l| l.starts_with('\x01') || !(l.trim().is_empty() || junk.is_match(l)))
            .collect();
        for (pi, para) in paras.iter().enumerate() {
            let para = *para;
            let is_code = para.starts_with('\x01');
            let prev_code = pi > 0 && paras[pi - 1].starts_with('\x01');
            let meta = para.starts_with('▲') || para.starts_with('★') || para.starts_with("r/");
            if pi > 0 && !(is_code && prev_code) && !para.starts_with('·') && !meta {
                body.push(String::new());
            }
            if let Some(code) = para.strip_prefix('\x01') {
                body.push(highlight(code, width));
                continue;
            }
            let opts = textwrap::Options::new(width).subsequent_indent(if para.starts_with('·') {
                "  "
            } else {
                ""
            });
            let w = textwrap::wrap(para, opts);
            if w.is_empty() {
                body.push(String::new());
            }
            for l in w {
                let l = l.into_owned();
                body.push(if meta {
                    format!("{}{l}{RST}", fg(ar / 2 + 60, ag / 2 + 60, ab / 2 + 60))
                } else if let Some(rest) = l.strip_prefix('·') {
                    format!("{}•{RST}\x1b[38;5;250m{rest}{RST}", fg(ar, ag, ab))
                } else {
                    format!("\x1b[38;5;250m{l}{RST}")
                });
            }
        }
        let max_body = if has_media {
            6
        } else {
            rows.saturating_sub(12).max(3)
        };
        if body.len() > max_body {
            body.truncate(max_body - 1);
            body.push(format!("{DIM}…{RST}"));
        }
        if !body.is_empty() {
            lines.push(String::new());
            lines.extend(body);
        }

        let bg = card_bg(&it);
        let mut b = format!("{KITTY_CLEAR}{RST}\x1b[2J").into_bytes();
        b.extend(card(cols, rows, bg, &t));
        for (n, l) in lines.iter().enumerate() {
            b.extend(format!("\x1b[{};5H{}{RST}", n + 2, on_card(l, bg)).into_bytes());
        }
        out(&b);

        let top = lines.len() as u16 + 3;
        self.link_row = lines.len() + 3;
        let title_end = title_at + textwrap::wrap(&it.title, width).len().min(3);
        self.opens.clear();
        self.opens.push(title_at..title_end);
        let avail = (rows as u16).saturating_sub(top + 5);
        if avail >= 4 {
            let mw = width as u16;
            if let Some((path, iw, ih)) = &img {
                let (c, r) = fit(*iw as f64, *ih as f64, mw, avail, &t);
                if let Ok(data) = std::fs::read(path) {
                    self.link_row = (top + r) as usize + 1;
                    self.opens.push(top as usize..(top + r) as usize);
                    let left = 5 + (mw - c) / 2;
                    let mut b = format!("\x1b[{top};{left}H").into_bytes();
                    b.extend(kitty(&data, &format!("a=T,f=100,c={c},r={r},C=1,q=2")));
                    out(&b);
                }
            }
            if let Some(v) = &it.video {
                let (c, r) = fit(16.0, 9.0, mw, avail, &t);
                self.link_row = (top + r) as usize + 1;
                self.opens.push(top as usize..(top + r) as usize);
                self.player = Player::start(v, top, 5 + (mw - c) / 2, c, r, &t, &self.cfg);
                if let Some(p) = &mut self.player {
                    p.set_paused(self.paused);
                }
            }
        }
        self.footer();
    }

    fn dwell_done(&mut self) -> bool {
        let video_running = self.player.as_mut().is_some_and(|p| !p.done());
        if video_running {
            self.elapsed >= self.cfg.video_max as f64 + 5.0
        } else {
            self.elapsed >= self.cfg.dwell
        }
    }

    fn footer(&mut self) {
        let t = term();
        let (cols, rows) = (t.cols as usize, t.rows as usize);
        let width = cols.saturating_sub(8).max(20);
        let it = &self.queue[self.i];
        let (ar, ag, ab) = accent(it);
        let video_running = self.player.as_mut().is_some_and(|p| !p.done());
        let total = if video_running {
            self.cfg.video_max as f64
        } else {
            self.cfg.dwell
        };
        let blank = " ".repeat(width);

        // link
        let shown = it
            .url
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_start_matches("www.");
        // With the pointer over something a click opens, the link lights up and says so: the
        // pointer itself may not change (not every host passes OSC 22 on).
        const CUE: &str = "  ← click to open";
        let room = width.saturating_sub(4);
        let cue = self.hand && room > CUE.chars().count() + 12;
        let shown = crate::fetch::truncate(
            shown,
            if cue {
                room - CUE.chars().count()
            } else {
                room
            },
        );
        let link = if self.hand {
            format!(
                "{BOLD}\x1b[38;2;255;255;255m↗ \x1b[4m\x1b]8;;{}\x1b\\{shown}\x1b]8;;\x1b\\{RST}{}",
                it.url,
                if cue {
                    format!("\x1b[38;2;{ar};{ag};{ab}m{CUE}{RST}")
                } else {
                    String::new()
                }
            )
        } else {
            format!(
                "\x1b[38;2;{ar};{ag};{ab}m↗ \x1b[4m\x1b]8;;{}\x1b\\{shown}\x1b]8;;\x1b\\{RST}",
                it.url
            )
        };

        // position dots (window of 9 around current)
        let n = self.queue.len();
        let lo = self.i.saturating_sub(4).min(n.saturating_sub(9));
        let dots: String = (lo..(lo + 9).min(n))
            .map(|k| {
                if k == self.i {
                    format!("\x1b[38;2;{ar};{ag};{ab}m●{RST}")
                } else {
                    format!("\x1b[38;5;240m·{RST}")
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        let counter = format!("{DIM}{}/{}{RST}", self.i + 1, n);

        // gradient progress bar or pause notice
        // bar spans exactly the dots + counter row below it
        let bar_w = (self.queue.len().min(9) * 2 - 1)
            + 2
            + format!("{}/{}", self.i + 1, self.queue.len()).len();
        let status = if self.manual_pause {
            format!("\x1b[38;5;221m⏸  paused · space to resume{RST}")
        } else if self.paused {
            format!("\x1b[38;5;221m⏸  paused · resumes when you're back on the agent{RST}")
        } else if self.browsing {
            format!(
                "\x1b[38;2;{ar};{ag};{ab}m●{RST}{DIM}  browsing · cards move on again when you leave{RST}"
            )
        } else {
            let filled = ((self.elapsed / total).min(1.0) * bar_w as f64) as usize;
            let mut b = String::new();
            for k in 0..bar_w {
                if k < filled {
                    let f = 0.45 + 0.55 * (k as f64 / bar_w as f64);
                    b += &format!(
                        "\x1b[38;2;{};{};{}m━",
                        (ar as f64 * f) as u8,
                        (ag as f64 * f) as u8,
                        (ab as f64 * f) as u8
                    );
                } else {
                    b += "\x1b[38;5;237m━";
                }
            }
            b + RST
        };
        // the dots and counter take the left of the row; the key hint gets what's left of it
        let used = 4 + bar_w + 3; // left margin, dots + counter (as wide as the bar), a gap
        let (keys, keys_w) = key_hint(cols.saturating_sub(used + 3));
        let bg = card_bg(it);
        let s = format!(
            "\x1b7\x1b[{r1};5H{link}\x1b[{r2};5H{blank}\x1b[{r2};5H{status}\x1b[{r3};5H{blank}\x1b[{r3};5H{dots}  {counter}\x1b[{r3};{kc}H{keys}{RST}",
            r1 = self.link_row.min(rows - 5),
            r2 = rows - 3,
            r3 = rows - 2,
            kc = (cols - 3).saturating_sub(keys_w),
        );
        out(format!("{}{RST}\x1b8", on_card(&s, bg)).as_bytes());
        if self.player.is_some() {
            out(&corners(cols, rows, bg, &t));
        }
    }

    /// Returns false when the queue is exhausted.
    fn go(&mut self, idx: isize) -> bool {
        mark_seen(&self.queue[self.i].id);
        if idx < 0 {
            return true;
        }
        let idx = idx as usize;
        if idx >= self.queue.len() {
            // batch done: pull the next batch of unseen cards, else refresh in background and close
            let have: std::collections::HashSet<String> =
                self.queue.iter().map(|x| x.id.clone()).collect();
            let more: Vec<Item> = build_queue(&self.cfg)
                .into_iter()
                .filter(|x| !have.contains(&x.id))
                .collect();
            if more.is_empty() {
                crate::spawn_fetch();
                return false;
            }
            let from = self.queue.len();
            self.queue.extend(more);
            self.warm(from);
        }
        self.i = idx;
        self.elapsed = 0.0;
        self.prepare(idx);
        self.prepare(idx + 1);
        self.draw();
        true
    }

    fn run(&mut self) {
        self.prepare(0);
        self.prepare(1);
        self.warm(2);
        self.draw();
        let mut last = now();
        let mut last_footer = 0.0;
        let mut paused_since: Option<f64> = None;
        let mut size = crossterm::terminal::size().ok();
        loop {
            // Not every host delivers SIGWINCH to the pane's program (Herdr's split resize
            // doesn't), so watch the size too: a card drawn for another width is garbage.
            let now_size = crossterm::terminal::size().ok();
            if now_size != size {
                size = now_size;
                self.draw();
            }
            if event::poll(Duration::from_millis(200)).unwrap_or(false) {
                match event::read() {
                    Ok(Event::Key(k)) => match k.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => break,
                        KeyCode::Char('n' | 'l' | 'j') | KeyCode::Right | KeyCode::Down => {
                            if !self.go(self.i as isize + 1) {
                                break;
                            }
                        }
                        KeyCode::Char('p' | 'h' | 'k') | KeyCode::Left | KeyCode::Up => {
                            self.go(self.i as isize - 1);
                        }
                        KeyCode::Char(' ') => self.manual_pause = !self.manual_pause,
                        KeyCode::Char('o') | KeyCode::Enter => self.open_link(),
                        _ => {}
                    },
                    // a click on the title, the picture or the link opens it; the wheel browses
                    Ok(Event::Mouse(m)) => match m.kind {
                        MouseEventKind::Down(MouseButton::Left) if self.opens_at(m.row) => {
                            self.open_link()
                        }
                        MouseEventKind::ScrollDown => {
                            if !self.go(self.i as isize + 1) {
                                break;
                            }
                        }
                        MouseEventKind::ScrollUp => {
                            self.go(self.i as isize - 1);
                        }
                        // a hand over what a click opens, the usual arrow elsewhere
                        MouseEventKind::Moved => self.set_hand(self.opens_at(m.row)),
                        _ => {}
                    },
                    Ok(Event::Resize(..)) => {
                        size = crossterm::terminal::size().ok();
                        self.draw();
                    }
                    Ok(Event::FocusGained) => self.focused = true,
                    Ok(Event::FocusLost) => self.focused = false,
                    _ => {}
                }
            }
            let st = read_state(&self.key);
            if st.status == "closed" {
                break;
            }
            // paused: nothing moves (space, or another tab). browsing: the person is in the feed
            // (Herdr says so through the state; a terminal that reports focus says so too), so
            // cards wait for n/p but the video plays on.
            self.paused = self.manual_pause || st.status == "paused";
            self.browsing = !self.paused && (self.focused || st.status == "browsing");
            if let Some(p) = &mut self.player {
                p.set_paused(self.paused);
            }
            if self.awaiting_picture && self.picture_ready(self.i) {
                self.draw();
            }
            let t = now();
            if self.paused {
                let since = *paused_since.get_or_insert(t);
                if t - since > self.cfg.idle_close_minutes * 60.0 {
                    break;
                }
            } else {
                paused_since = None;
                if !self.browsing {
                    self.elapsed += t - last;
                }
            }
            last = t;
            if !self.paused && !self.browsing && self.dwell_done() && !self.go(self.i as isize + 1)
            {
                break;
            }
            if t - last_footer > 0.5 {
                self.footer();
                last_footer = t;
            }
        }
    }
}

/// Mouse pointer shapes (OSC 22, as Ghostty, kitty and others take it).
const POINTER_HAND: &[u8] = b"\x1b]22;pointer\x1b\\";
const POINTER_DEFAULT: &[u8] = b"\x1b]22;default\x1b\\";

/// Threads getting a batch's pictures ready in the background.
const PREP_WORKERS: usize = 3;
/// How long a card waits for its picture before it's drawn without it (filled in later).
const PICTURE_WAIT_MS: u64 = 120;

/// Mark card `idx` as being prepared and return the work, or None if someone already has it.
fn claim(
    prep: &Arc<Mutex<HashMap<usize, Prep>>>,
    queue: &[Item],
    idx: usize,
) -> Option<impl FnOnce() + Send + 'static> {
    let it = queue.get(idx)?;
    {
        let mut m = prep.lock().unwrap();
        if m.contains_key(&idx) {
            return None;
        }
        m.insert(idx, Prep::Pending);
    }
    let (url, video, prep) = (it.image.clone(), it.video.clone(), prep.clone());
    Some(move || {
        if let Some(v) = video {
            // fetch the clip ahead so it starts the moment its card shows
            crate::fetch::prefetch_video(&v);
        }
        let r = url.and_then(|u| prep_image(&u));
        prep.lock().unwrap().insert(idx, Prep::Ready(r));
    })
}

/// The system's "open this link in the browser" command.
const OPENER: &str = if cfg!(target_os = "macos") {
    "open"
} else {
    "xdg-open"
};

/// The keys, spelled out as much as `room` allows: the styled hint and its width in cells.
fn key_hint(room: usize) -> (String, usize) {
    // browsing matters more than pausing (clicking the feed pauses it anyway), so it goes last
    const HINTS: [&[(&str, &str)]; 5] = [
        &[
            ("n", "next"),
            ("p", "prev"),
            ("space", "pause"),
            ("o", "open"),
            ("q", "close"),
        ],
        &[
            ("n/p", "browse"),
            ("space", "pause"),
            ("o", "open"),
            ("q", "close"),
        ],
        &[("n/p", "browse"), ("o", "open"), ("q", "close")],
        &[("n/p", "browse"), ("q", "close")],
        &[("q", "close")],
    ];
    for hint in HINTS {
        let width = hint
            .iter()
            .map(|(k, l)| k.chars().count() + 1 + l.len())
            .sum::<usize>()
            + 3 * (hint.len() - 1);
        if width <= room {
            let styled = hint
                .iter()
                .map(|(k, l)| format!("\x1b[38;5;250m{k}{RST}{DIM} {l}{RST}"))
                .collect::<Vec<_>>()
                .join(&format!("{DIM} · {RST}"));
            return (styled, width);
        }
    }
    (String::new(), 0)
}

/// Big gradient wordmark for a moment while the first card's media loads.
fn splash() {
    let t = term();
    let (cols, rows) = (t.cols as usize, t.rows as usize);
    let w = crate::logo::BIG[0].chars().count();
    let mut s = format!("{RST}\x1b[2J");
    if cols >= w + 2 {
        let top = rows.saturating_sub(8) / 2;
        let left = (cols - w) / 2 + 1;
        for (i, l) in crate::logo::BIG.iter().enumerate() {
            s += &format!(
                "\x1b[{};{left}H{}",
                top + i + 1,
                crate::logo::paint(l, true)
            );
        }
        let tag = "short reads while your agent works";
        s += &format!(
            "\x1b[{};{}H{DIM}{tag}{RST}",
            top + 8,
            (cols - tag.len()) / 2 + 1
        );
    } else {
        s += &format!(
            "\x1b[{};{}H{}",
            rows / 2,
            cols.saturating_sub(9) / 2 + 1,
            crate::logo::badge()
        );
    }
    out(s.as_bytes());
    std::thread::sleep(Duration::from_millis(1100));
}

pub fn run(key: &str) {
    let cfg = config();
    let queue = build_queue(&cfg);
    if queue.is_empty() {
        println!("No unseen cards — run `herdr-feed fetch` to refresh.");
        std::thread::sleep(Duration::from_secs(2));
        return;
    }
    let _ = crossterm::terminal::enable_raw_mode();
    // 1004: focus in/out reports; 1003 + 1006: mouse clicks, wheel and motion, SGR-encoded (motion
    // lets the pointer turn into a hand over links; Shift still selects text in most terminals)
    out(b"\x1b[?1049h\x1b[?25l\x1b[?1004h\x1b[?1003h\x1b[?1006h");
    let mut pane = Pane {
        key: key.into(),
        cfg,
        queue,
        i: 0,
        prep: Arc::new(Mutex::new(HashMap::new())),
        player: None,
        elapsed: 0.0,
        paused: false,
        manual_pause: false,
        browsing: false,
        focused: false,
        awaiting_picture: false,
        opens: Vec::new(),
        hand: false,
        link_row: 0,
    };
    splash();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pane.run()));
    if let Some(p) = pane.player.take() {
        p.stop();
    }
    mark_seen(&pane.queue[pane.i].id);
    out(POINTER_DEFAULT);
    out(format!("{KITTY_CLEAR}\x1b[?1006l\x1b[?1003l\x1b[?1004l\x1b[?1049l\x1b[?25h").as_bytes());
    let _ = crossterm::terminal::disable_raw_mode();
    if r.is_err() {
        log("pane panicked");
    }
}
