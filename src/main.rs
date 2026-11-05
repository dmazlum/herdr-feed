//! herdr-feed: short tech news / tips / video side pane beside the active coding agent in Herdr.
//! Builds on rexfeed (https://github.com/alpcanaydin/rexfeed) by Alpcan Aydın, MIT.

mod fetch;
mod layout;
mod logo;
mod pane;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const HELP: &str = "herdr-feed: tech news / tips side pane beside the active agent in Herdr

USAGE:
  herdr-feed fetch                      refresh the cache from all sources
  herdr-feed ready                      exit 0 when there are unseen cards (starts a
                                        background fetch when the cache is stale)
  herdr-feed pane [--key KEY] [--driven]
                                        run the side pane; --driven leaves its state to
                                        the caller (the Herdr mod)
  herdr-feed state KEY running|browsing|paused|closed
                                        drive an open pane (the Herdr mod calls this)
  herdr-feed resize AGENT FEED          give FEED `pane_ratio`% of the row it shares
                                        with AGENT (terminal addresses, e.g. t47 t4g)
  herdr-feed list                       print cached items
";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Item {
    pub id: String,
    pub source: String,
    pub title: String,
    pub url: String,
    pub body: String,
    pub image: Option<String>,
    pub video: Option<String>,
    pub kind: String,
    pub ts: f64,
}

#[derive(Serialize, Deserialize, Default)]
pub struct Store {
    pub fetched: f64,
    pub items: Vec<Item>,
}

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    pub dwell: f64,
    pub video_max: u32,
    pub queue_size: usize,
    pub refresh_minutes: f64,
    pub idle_close_minutes: f64,
    pub video_muted: bool,
    /// The feed's share of the row it splits with the agent, in percent.
    pub pane_ratio: u32,
    /// Video clips to download ahead per refresh.
    pub video_prefetch: usize,
    /// Every n-th card is a video (0: no rule). The first card is one.
    pub video_every: usize,
    /// Favour cards with pictures: with `video_every`, the others alternate image and text;
    /// without it, two cards with media, then one text card.
    pub media_first: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            dwell: 10.0,
            video_max: 20,
            queue_size: 30,
            refresh_minutes: 45.0,
            idle_close_minutes: 30.0,
            video_muted: true,
            pane_ratio: 38,
            video_prefetch: 20,
            video_every: 3,
            media_first: true,
        }
    }
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
}
pub fn cache_dir() -> PathBuf {
    let p = home().join(".cache/herdr-feed");
    let _ = std::fs::create_dir_all(&p);
    p
}
pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

pub fn config() -> Config {
    std::fs::read_to_string(home().join(".config/herdr-feed/config.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn log(msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(cache_dir().join("log.txt"))
    {
        let _ = writeln!(f, "{:.0} {}", now(), msg);
    }
}

pub fn write_json<T: Serialize>(path: &PathBuf, v: &T) {
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, serde_json::to_vec(v).unwrap()).is_ok() {
        let _ = std::fs::rename(tmp, path);
    }
}

pub fn load_store() -> Store {
    std::fs::read(cache_dir().join("items.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn load_seen() -> std::collections::HashSet<String> {
    std::fs::read(cache_dir().join("seen.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<String>>(&b).ok())
        .unwrap_or_default()
        .into_iter()
        .collect()
}

pub fn mark_seen(id: &str) {
    let mut v: Vec<String> = std::fs::read(cache_dir().join("seen.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    if !v.iter().any(|x| x == id) {
        v.push(id.to_string());
    }
    if v.len() > 5000 {
        v.drain(..v.len() - 5000);
    }
    write_json(&cache_dir().join("seen.json"), &v);
}

/// Unseen items, round-robin across sources so cards stay varied.
pub fn build_queue(cfg: &Config) -> Vec<Item> {
    let seen = load_seen();
    let mut by_src: Vec<(String, std::collections::VecDeque<Item>)> = Vec::new();
    // HERDR_FEED_ONLY=video (or tip, news…) narrows the queue; handy for trying one card type
    let only = std::env::var("HERDR_FEED_ONLY").ok();
    for it in load_store()
        .items
        .into_iter()
        .filter(|x| !seen.contains(&x.id))
        // a video card waits until its clip is cached, so it always starts instantly
        .filter(|x| {
            x.video
                .as_deref()
                .is_none_or(|v| crate::fetch::video_cache(v).exists())
        })
        .filter(|x| match only.as_deref() {
            Some("video") => x.video.is_some(),
            Some(k) => x.kind == k,
            None => true,
        })
    {
        match by_src.iter_mut().find(|(s, _)| *s == it.source) {
            Some((_, q)) => q.push_back(it),
            None => by_src.push((it.source.clone(), [it].into())),
        }
    }
    fastrand::shuffle(&mut by_src);
    // HERDR_FEED_LEAD=video: start with a (cached) video card, e.g. for demos
    let lead = (std::env::var("HERDR_FEED_LEAD").as_deref() == Ok("video"))
        .then(|| {
            by_src.iter_mut().find_map(|(_, q)| {
                q.iter()
                    .position(|x| x.video.is_some())
                    .and_then(|i| q.remove(i))
            })
        })
        .flatten();
    // round-robin across sources, so whatever kind a slot asks for comes from a varied mix
    let mut all: Vec<Item> = Vec::new();
    while by_src.iter().any(|(_, q)| !q.is_empty()) {
        for (_, q) in by_src.iter_mut() {
            if let Some(x) = q.pop_front() {
                all.push(x);
            }
        }
    }
    let mut out: Vec<Item> = lead.into_iter().collect();
    let room = cfg.queue_size.saturating_sub(out.len());
    out.extend(arrange(all, room, cfg.video_every, cfg.media_first));
    out
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Slot {
    Video,
    Image,
    Text,
    /// Image or video, whichever there is (media_first without video_every).
    Media,
    /// Next image or text card in round-robin order (media_first off).
    Any,
}

/// What kind of card slot `n` of a batch asks for.
///
/// With `video_every` k > 0, every k-th card from the first is a video; the rest alternate image,
/// text when `media_first`, or follow round-robin order when not. With k = 0 and `media_first`,
/// two cards with media, then one without.
fn slot_kind(n: usize, video_every: usize, media_first: bool) -> Slot {
    if video_every > 0 {
        if n.is_multiple_of(video_every) {
            return Slot::Video;
        }
        let j = n - (n / video_every + 1); // index among the non-video slots
        return match (media_first, j % 2) {
            (false, _) => Slot::Any,
            (true, 0) => Slot::Image,
            (true, _) => Slot::Text,
        };
    }
    match (media_first, n % 3) {
        (false, _) => Slot::Any,
        (true, 2) => Slot::Text,
        (true, _) => Slot::Media,
    }
}

/// Fill `size` slots from `items` (already round-robin across sources) by `slot_kind`. A slot
/// whose kind has run out takes the next card of another kind, so a batch is never short.
fn arrange(items: Vec<Item>, size: usize, video_every: usize, media_first: bool) -> Vec<Item> {
    use std::collections::VecDeque;
    let (mut video, mut image, mut text) = (VecDeque::new(), VecDeque::new(), VecDeque::new());
    for (i, it) in items.into_iter().enumerate() {
        match (it.video.is_some(), it.image.is_some()) {
            (true, _) => video.push_back((i, it)),
            (false, true) => image.push_back((i, it)),
            (false, false) => text.push_back((i, it)),
        }
    }
    let earlier =
        |a: &VecDeque<(usize, Item)>, b: &VecDeque<(usize, Item)>| match (a.front(), b.front()) {
            (Some((x, _)), Some((y, _))) => x < y,
            (Some(_), None) => true,
            _ => false,
        };
    let mut out = Vec::with_capacity(size);
    for n in 0..size {
        let order: [&mut VecDeque<(usize, Item)>; 3] = match slot_kind(n, video_every, media_first)
        {
            Slot::Video => [&mut video, &mut image, &mut text],
            Slot::Image => [&mut image, &mut text, &mut video],
            Slot::Text => [&mut text, &mut image, &mut video],
            Slot::Media if earlier(&video, &image) => [&mut video, &mut image, &mut text],
            Slot::Media => [&mut image, &mut video, &mut text],
            Slot::Any if earlier(&text, &image) => [&mut text, &mut image, &mut video],
            Slot::Any => [&mut image, &mut text, &mut video],
        };
        match order.into_iter().find_map(|q| q.pop_front()) {
            Some((_, it)) => out.push(it),
            None => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, media: bool) -> Item {
        Item {
            id: id.into(),
            source: "s".into(),
            title: id.into(),
            url: String::new(),
            body: String::new(),
            image: media.then(|| "https://x/i.png".into()),
            video: None,
            kind: "news".into(),
            ts: 0.0,
        }
    }
    fn ids(v: &[Item]) -> String {
        v.iter()
            .map(|x| x.id.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn video(id: &str) -> Item {
        Item {
            video: Some(format!("https://youtu.be/{id}")),
            ..item(id, true)
        }
    }

    #[test]
    fn every_third_card_is_a_video_then_image_then_text() {
        let mut v = Vec::new();
        for k in 0..4 {
            v.extend([
                item(&format!("t{k}"), false),
                item(&format!("i{k}"), true),
                video(&format!("v{k}")),
            ]);
        }
        assert_eq!(ids(&arrange(v, 9, 3, true)), "v0 i0 t0 v1 i1 t1 v2 i2 t2");
    }

    #[test]
    fn a_kind_that_runs_out_is_filled_by_the_others() {
        let v = vec![
            video("v0"),
            item("i0", true),
            item("i1", true),
            item("t0", false),
            item("t1", false),
        ];
        // no second video: its slot takes the next image; no images left: text fills in
        assert_eq!(ids(&arrange(v, 5, 3, true)), "v0 i0 t0 i1 t1");
        let none = vec![item("t0", false), item("t1", false)];
        assert_eq!(ids(&arrange(none, 5, 3, true)), "t0 t1");
    }

    #[test]
    fn without_video_every_two_media_cards_then_one_text_card() {
        let v = vec![
            item("t1", false),
            item("m1", true),
            item("t2", false),
            item("m2", true),
            video("m3"),
            item("m4", true),
        ];
        assert_eq!(ids(&arrange(v, 6, 0, true)), "m1 m2 t1 m3 m4 t2");
    }

    #[test]
    fn media_first_off_keeps_round_robin_order_around_the_videos() {
        let v = vec![
            item("t0", false),
            item("i0", true),
            video("v0"),
            item("t1", false),
        ];
        assert_eq!(ids(&arrange(v, 4, 3, false)), "v0 t0 i0 t1");
        let v = vec![item("t0", false), item("i0", true), item("t1", false)];
        assert_eq!(ids(&arrange(v, 3, 0, false)), "t0 i0 t1");
    }
}

// ---------------------------------------------------------------- per-pane state

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct PaneState {
    /// running | browsing (cards wait, video plays) | paused | closed. Written by whoever drives the pane (the Herdr mod, through
    /// `herdr-feed state`); the pane itself only reads it, so the two never race on the file.
    pub status: String,
}

fn state_path(key: &str) -> PathBuf {
    let d = cache_dir().join("state");
    let _ = std::fs::create_dir_all(&d);
    let safe: String = key
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    d.join(format!("{safe}.json"))
}
pub fn read_state(key: &str) -> PaneState {
    std::fs::read(state_path(key))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}
pub fn update_state(key: &str, f: impl FnOnce(&mut PaneState)) {
    let mut s = read_state(key);
    f(&mut s);
    write_json(&state_path(key), &s);
}

/// Start `herdr-feed fetch` detached, so the caller returns at once.
pub fn spawn_fetch() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = Command::new(exe)
            .arg("fetch")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn();
    }
}

fn arg_after(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

use std::os::unix::process::CommandExt;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        Some("fetch") => fetch::fetch_all(),
        Some("ready") => {
            let cfg = config();
            if now() - load_store().fetched > cfg.refresh_minutes * 60.0 {
                spawn_fetch();
            }
            std::process::exit(if build_queue(&cfg).is_empty() { 1 } else { 0 });
        }
        Some("pane") => {
            let key = arg_after(&args, "--key").unwrap_or_else(|| "manual".into());
            // --driven: the mod set the state before opening us and keeps writing it
            if !args.iter().any(|a| a == "--driven") {
                update_state(&key, |s| s.status = "running".into());
            }
            pane::run(&key);
        }
        Some("state") => {
            let (Some(key), Some(status)) = (args.get(1), args.get(2)) else {
                eprint!("{HELP}");
                std::process::exit(2);
            };
            if !matches!(
                status.as_str(),
                "running" | "browsing" | "paused" | "closed"
            ) {
                eprintln!("herdr-feed: unknown state {status}");
                std::process::exit(2);
            }
            update_state(key, |s| s.status = status.clone());
        }
        Some("resize") => {
            let (Some(agent), Some(feed)) = (args.get(1), args.get(2)) else {
                eprint!("{HELP}");
                std::process::exit(2);
            };
            let share = config().pane_ratio.clamp(10, 90) as f64 / 100.0;
            if let Err(e) = layout::resize(agent, feed, share) {
                log(&format!("resize: {e}"));
                eprintln!("herdr-feed: {e}");
                std::process::exit(1);
            }
        }
        Some("list") => {
            for x in load_store().items {
                println!(
                    "{:<16} {}{}{}",
                    x.source.chars().take(16).collect::<String>(),
                    if x.image.is_some() { "🖼 " } else { "" },
                    if x.video.is_some() { "▶ " } else { "" },
                    x.title
                );
            }
        }
        _ => print!("{HELP}"),
    }
}
