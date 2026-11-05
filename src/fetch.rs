use crate::{Item, Store, cache_dir, log, now, write_json};
use anyhow::{Result, anyhow};
use regex::Regex;
use serde_json::Value;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

pub const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) herdr-feed/0.1";

fn agent() -> &'static ureq::Agent {
    static A: OnceLock<ureq::Agent> = OnceLock::new();
    A.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(12))
            .user_agent(UA)
            .build()
    })
}

pub fn get(url: &str) -> Result<String> {
    Ok(agent().get(url).call()?.into_string()?)
}
pub fn get_bytes(url: &str, max: u64) -> Result<Vec<u8>> {
    let mut v = Vec::new();
    agent()
        .get(url)
        .call()?
        .into_reader()
        .take(max)
        .read_to_end(&mut v)?;
    Ok(v)
}
fn jget(url: &str) -> Result<Value> {
    Ok(serde_json::from_str(&get(url)?)?)
}

fn re(p: &str) -> Regex {
    Regex::new(p).unwrap()
}

/// Code lines are carried in the body prefixed with \x01; the pane highlights them.
pub fn mark_code_fences(md: &str) -> String {
    let mut out = Vec::new();
    let mut inside = false;
    for l in md.lines() {
        if l.trim_start().starts_with("```") {
            inside = !inside;
            continue;
        }
        out.push(if inside {
            format!("\x01{l}")
        } else {
            l.to_string()
        });
    }
    out.join("\n")
}

fn mark_pre(s: &str) -> String {
    static R: OnceLock<(Regex, Regex)> = OnceLock::new();
    let (pre, tag) = R.get_or_init(|| (re(r"(?is)<pre[^>]*>(.*?)</pre>"), re(r"<[^>]+>")));
    pre.replace_all(s, |c: &regex::Captures| {
        let inner = html_escape::decode_html_entities(&tag.replace_all(&c[1], "")).to_string();
        let lines: Vec<String> = inner
            .trim_matches('\n')
            .lines()
            // re-escape what the tag stripping and the single decode in strip_html would eat
            .map(|l| format!("\x01{}", l.replace('&', "&amp;").replace('<', "&lt;")))
            .collect();
        format!("\n{}\n", lines.join("\n"))
    })
    .to_string()
}

/// Remote text goes straight to the terminal: drop control characters (escape sequences) except
/// newlines and the \x01 code-line marker.
pub fn clean(s: &str) -> String {
    s.chars()
        .filter(|&c| !c.is_control() || c == '\n' || c == '\x01')
        .collect()
}

/// Only plain http(s) links survive (they're opened, fetched and handed to yt-dlp/ffmpeg).
pub fn safe_url(u: &str) -> String {
    let u = clean(u.trim());
    if (u.starts_with("https://") || u.starts_with("http://")) && !u.contains(char::is_whitespace) {
        u
    } else {
        String::new()
    }
}

pub fn strip_html(s: &str, limit: usize) -> String {
    let s = &mark_pre(s);
    static R: OnceLock<[Regex; 6]> = OnceLock::new();
    let r = R.get_or_init(|| {
        [
            re(r"(?is)<script.*?</script>"),
            re(r"(?is)<style.*?</style>"),
            re(r"(?i)<br\s*/?>|</p>|</li>|</h\d>"),
            re(r"<[^>]+>"),
            re(r"[ \t\u{a0}]+"),
            re(r"\n\s*\n+"),
        ]
    });
    let s = r[0].replace_all(s, " ");
    let s = r[1].replace_all(&s, " ");
    let s = r[2].replace_all(&s, "\n");
    let s = r[3].replace_all(&s, "");
    let s = html_escape::decode_html_entities(&s).to_string();
    // collapse runs of spaces in prose only: code lines (\x01) keep their indentation
    let s = s
        .lines()
        .map(|l| {
            if l.starts_with('\x01') {
                l.trim_end().to_string()
            } else {
                r[4].replace_all(l, " ").trim().to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let s = r[5].replace_all(&s, "\n").trim().to_string();
    truncate(&clean(&s), limit)
}

pub fn truncate(s: &str, limit: usize) -> String {
    if s.chars().count() <= limit {
        return s.to_string();
    }
    let cut: String = s.chars().take(limit).collect();
    let cut = match cut.rfind(' ') {
        Some(i) if i > limit / 2 => cut[..i].to_string(),
        _ => cut,
    };
    format!("{cut}…")
}

fn parse_date(s: &str) -> Option<f64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .or_else(|_| chrono::DateTime::parse_from_rfc2822(s))
        .ok()
        .map(|d| d.timestamp() as f64)
}

pub fn yt_id(url: &str) -> Option<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| re(r"(?:youtube\.com/(?:watch\?v=|shorts/)|youtu\.be/)([\w-]{11})"))
        .captures(url)
        .map(|c| c[1].to_string())
}

#[allow(clippy::too_many_arguments)]
fn item(
    source: &str,
    title: &str,
    url: &str,
    body: String,
    image: Option<String>,
    video: Option<String>,
    kind: &str,
    ts: Option<f64>,
    uid: Option<String>,
) -> Item {
    let id = uid.unwrap_or_else(|| {
        let key = format!("{source}|{}", if url.is_empty() { title } else { url });
        sha1_smol::Sha1::from(key).digest().to_string()[..16].to_string()
    });
    // YouTube links are rebuilt from the video id, so nothing else of the raw url reaches yt-dlp
    let canon = |u: &str| yt_id(u).map(|id| format!("https://www.youtube.com/watch?v={id}"));
    let video = video
        .map(|v| canon(&v).unwrap_or_else(|| safe_url(&v)))
        .or_else(|| canon(url))
        .filter(|v| !v.is_empty());
    let image = image.map(|i| safe_url(&i)).filter(|i| !i.is_empty());
    Item {
        id,
        source: clean(source),
        title: strip_html(title, 160),
        url: safe_url(url),
        body: clean(&body),
        image,
        video,
        kind: kind.into(),
        ts: ts.unwrap_or_else(now),
    }
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}
fn n(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(|x| x.as_i64()).unwrap_or(0)
}

// ---------------------------------------------------------------- sources

fn src_hn() -> Result<Vec<Item>> {
    let ids: Vec<u64> = serde_json::from_value(jget(
        "https://hacker-news.firebaseio.com/v0/topstories.json",
    )?)?;
    let out: Vec<Option<Item>> = std::thread::scope(|sc| {
        let hs: Vec<_> = ids
            .into_iter()
            .take(20)
            .map(|id| {
                sc.spawn(move || {
                    let d = jget(&format!(
                        "https://hacker-news.firebaseio.com/v0/item/{id}.json"
                    ))
                    .ok()?;
                    let mut url = s(&d, "url");
                    if url.is_empty() {
                        url = format!("https://news.ycombinator.com/item?id={id}");
                    }
                    let mut body =
                        format!("▲ {} · {} comments", n(&d, "score"), n(&d, "descendants"));
                    let text = s(&d, "text");
                    if !text.is_empty() {
                        body = format!("{}\n{body}", strip_html(&text, 300));
                    }
                    Some(item(
                        "Hacker News",
                        &s(&d, "title"),
                        &url,
                        body,
                        None,
                        None,
                        "news",
                        d.get("time").and_then(|t| t.as_f64()),
                        None,
                    ))
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().ok().flatten()).collect()
    });
    Ok(out
        .into_iter()
        .flatten()
        .filter(|x| !x.title.is_empty())
        .collect())
}

fn src_lobsters() -> Result<Vec<Item>> {
    let v = jget("https://lobste.rs/hottest.json")?;
    let arr = v.as_array().ok_or(anyhow!("not array"))?;
    Ok(arr
        .iter()
        .take(20)
        .map(|d| {
            let mut url = s(d, "url");
            if url.is_empty() {
                url = s(d, "short_id_url");
            }
            let tags: Vec<String> = d["tags"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|t| t.as_str())
                        .map(|t| format!("#{t}"))
                        .collect()
                })
                .unwrap_or_default();
            let desc = strip_html(&s(d, "description"), 300);
            let meta = format!("▲ {} · {}", n(d, "score"), tags.join(" "));
            let body = if desc.is_empty() {
                meta
            } else {
                format!("{desc}\n{meta}")
            };
            item(
                "Lobsters",
                &s(d, "title"),
                &url,
                body,
                None,
                None,
                "news",
                None,
                None,
            )
        })
        .collect())
}

fn child<'a>(n: roxmltree::Node<'a, 'a>, name: &str) -> Option<roxmltree::Node<'a, 'a>> {
    n.children()
        .find(|c| c.is_element() && c.tag_name().name() == name)
}
fn child_text(n: roxmltree::Node, name: &str) -> String {
    child(n, name)
        .and_then(|c| c.text())
        .unwrap_or("")
        .trim()
        .to_string()
}

pub fn parse_feed(xml: &str, source: &str, kind: &str, limit: usize) -> Result<Vec<Item>> {
    let doc = roxmltree::Document::parse(xml)?; // DTDs rejected by default (no XXE)
    let img_re = re(r#"<img[^>]+src="([^"]+)""#);
    let mut out = Vec::new();
    for e in doc
        .descendants()
        .filter(|n| n.is_element() && matches!(n.tag_name().name(), "item" | "entry"))
        .take(limit)
    {
        let title = child_text(e, "title");
        let mut link = child_text(e, "link");
        if link.is_empty() {
            link = child(e, "link")
                .and_then(|l| l.attribute("href"))
                .unwrap_or("")
                .to_string();
        }
        let mut desc = String::new();
        for k in ["description", "summary", "content", "encoded"] {
            desc = child_text(e, k);
            if !desc.is_empty() {
                break;
            }
        }
        if desc.is_empty()
            && let Some(d) = e
                .descendants()
                .find(|d| d.tag_name().name() == "description")
        {
            desc = d.text().unwrap_or("").to_string();
        }
        let mut image = e
            .descendants()
            .filter(|d| d.is_element())
            .find(|d| {
                let nm = d.tag_name().name();
                d.attribute("url").is_some()
                    && (nm == "thumbnail"
                        || (nm == "content" && d.attribute("medium").is_none_or(|m| m == "image"))
                        || (nm == "enclosure"
                            && d.attribute("type").is_some_and(|t| t.starts_with("image"))))
            })
            .and_then(|d| d.attribute("url"))
            .map(String::from);
        if image.is_none() {
            image = img_re
                .captures(&desc)
                .map(|c| html_escape::decode_html_entities(&c[1]).to_string());
        }
        let ts = ["published", "updated", "pubDate"]
            .iter()
            .find_map(|k| parse_date(&child_text(e, k)));
        out.push(item(
            source,
            &title,
            &link,
            strip_html(&desc, 420),
            image,
            None,
            kind,
            ts,
            None,
        ));
    }
    Ok(out)
}

const FEEDS: &[(&str, &str, &str)] = &[
    // news
    ("TLDR Tech", "https://tldr.tech/api/rss/tech", "news"),
    ("TLDR AI", "https://tldr.tech/api/rss/ai", "news"),
    ("Show HN", "https://hnrss.org/show?points=50", "news"),
    (
        "The Verge",
        "https://www.theverge.com/rss/index.xml",
        "news",
    ),
    (
        "Ars Technica",
        "https://feeds.arstechnica.com/arstechnica/index",
        "news",
    ),
    ("TechCrunch", "https://techcrunch.com/feed/", "news"),
    ("9to5Mac", "https://9to5mac.com/feed/", "news"),
    ("Hackaday", "https://hackaday.com/blog/feed/", "news"),
    ("Changelog", "https://changelog.com/news/feed", "news"),
    (
        "JavaScript Weekly",
        "https://cprss.s3.amazonaws.com/javascriptweekly.com.xml",
        "news",
    ),
    (
        "This Week in Rust",
        "https://this-week-in-rust.org/rss.xml",
        "news",
    ),
    // products
    (
        "Product Hunt",
        "https://www.producthunt.com/feed",
        "product",
    ),
    // company / eng blogs
    (
        "Simon Willison",
        "https://simonwillison.net/atom/everything/",
        "news",
    ),
    ("GitHub Blog", "https://github.blog/feed/", "news"),
    ("OpenAI", "https://openai.com/news/rss.xml", "news"),
    (
        "Hugging Face",
        "https://huggingface.co/blog/feed.xml",
        "news",
    ),
    ("Vercel", "https://vercel.com/atom", "news"),
    ("Cloudflare", "https://blog.cloudflare.com/rss/", "news"),
    ("Rust Blog", "https://blog.rust-lang.org/feed.xml", "news"),
    ("Latent Space", "https://www.latent.space/feed", "news"),
    ("Smashing", "https://www.smashingmagazine.com/feed/", "tip"),
    ("Josh Comeau", "https://www.joshwcomeau.com/rss.xml", "tip"),
    (
        "Simon's TIL",
        "https://til.simonwillison.net/tils/feed.atom",
        "tip",
    ),
    (
        "Claude Code",
        "https://github.com/anthropics/claude-code/releases.atom",
        "release",
    ),
    // video
    (
        "Fireship",
        "https://www.youtube.com/feeds/videos.xml?channel_id=UCsBjURrPoezykLs9EqgamOA",
        "video",
    ),
    (
        "Beyond Fireship",
        "https://www.youtube.com/feeds/videos.xml?channel_id=UC2Xd-TjJByJyK2w1zNwY0zQ",
        "video",
    ),
    (
        "ThePrimeTime",
        "https://www.youtube.com/feeds/videos.xml?channel_id=UCUyeluBRhGPCW4rPe_UvBZQ",
        "video",
    ),
    (
        "Theo",
        "https://www.youtube.com/feeds/videos.xml?channel_id=UCbRP3c757lWg9M-U7TyEkXA",
        "video",
    ),
    (
        "Two Minute Papers",
        "https://www.youtube.com/feeds/videos.xml?channel_id=UCbfYPyITQ-7l4upoX8nvctg",
        "video",
    ),
    (
        "Computerphile",
        "https://www.youtube.com/feeds/videos.xml?channel_id=UC9-y-6csu5WGm29I7JiwpnA",
        "video",
    ),
    (
        "Low Level",
        "https://www.youtube.com/feeds/videos.xml?channel_id=UC6biysICWOJ-C3P4Tyeggzg",
        "video",
    ),
    (
        "Linus Tech Tips",
        "https://www.youtube.com/feeds/videos.xml?channel_id=UCXuqSBlHAE6Xw-yeJA0Tunw",
        "video",
    ),
    (
        "Web Dev Simplified",
        "https://www.youtube.com/feeds/videos.xml?channel_id=UCFbNIlppjAuEX4znoulh0Cw",
        "video",
    ),
    (
        "Kurzgesagt",
        "https://www.youtube.com/feeds/videos.xml?channel_id=UCsXVk37bltHxD1rDPwtNM8Q",
        "video",
    ),
];

fn src_feeds() -> Result<Vec<Item>> {
    Ok(std::thread::scope(|sc| {
        let hs: Vec<_> = FEEDS
            .iter()
            .map(|(name, url, kind)| {
                sc.spawn(move || {
                    let lim = match *kind {
                        "release" => 4,
                        "video" => 5,
                        _ => 8,
                    };
                    match get(url).and_then(|x| parse_feed(&x, name, kind, lim)) {
                        Ok(v) => v,
                        Err(e) => {
                            log(&format!("feed fail {name}: {e}"));
                            vec![]
                        }
                    }
                })
            })
            .collect();
        hs.into_iter()
            .flat_map(|h| h.join().unwrap_or_default())
            .collect()
    }))
}

const SUBS: &[&str] = &[
    "programming",
    "ClaudeAI",
    "LocalLLaMA",
    "commandline",
    "rust",
    "webdev",
    "neovim",
    "selfhosted",
    "MachineLearning",
];

fn src_reddit() -> Result<Vec<Item>> {
    // JSON 403s without OAuth and RSS allows ~1 req per window, so fetch all
    // subs in one combined feed and split them by each entry's category.
    let vid = re(r"https://v\.redd\.it/\w+");
    let url = format!(
        "https://www.reddit.com/r/{}/top/.rss?t=day&limit=60",
        SUBS.join("+")
    );
    let xml = get(&url)?;
    let doc = roxmltree::Document::parse(&xml)?;
    let meta: Vec<(String, String)> = doc
        .descendants()
        .filter(|n| n.tag_name().name() == "entry")
        .map(|e| {
            let sub = child(e, "category")
                .and_then(|c| c.attribute("label"))
                .unwrap_or("r/reddit")
                .to_string();
            (sub, child_text(e, "content"))
        })
        .collect();
    let mut items = parse_feed(&xml, "reddit", "news", 60)?;
    for (it, (sub, c)) in items.iter_mut().zip(meta) {
        if let Some(m) = vid.find(&html_escape::decode_html_entities(&c)) {
            // yt-dlp needs a reddit login; the HLS playlist doesn't (and has audio)
            it.video = Some(format!("{}/HLSPlaylist.m3u8", m.as_str()));
        }
        let b = it
            .body
            .split("submitted by")
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        it.body = if b.is_empty() {
            sub.clone()
        } else {
            format!("{b}\n{sub}")
        };
        it.source = sub;
    }
    Ok(items)
}

fn src_trending() -> Result<Vec<Item>> {
    let page = get("https://github.com/trending?since=daily")?;
    let art = re(r#"(?s)<article class="Box-row">(.*?)</article>"#);
    let name = re(r#"<h2[^>]*>\s*<a[^>]+href="/([^"]+)""#);
    let desc = re(r#"(?s)<p class="col-9[^"]*">(.*?)</p>"#);
    let stars = re(r"(?s)([\d,]+)\s*stars today");
    let lang = re(r#"itemprop="programmingLanguage">([^<]+)<"#);
    Ok(art
        .captures_iter(&page)
        .take(15)
        .filter_map(|a| {
            let a = &a[1];
            let repo = name.captures(a)?[1].to_string();
            let d = desc
                .captures(a)
                .map(|c| strip_html(&c[1], 300))
                .unwrap_or_default();
            let meta: Vec<String> = [
                lang.captures(a).map(|c| c[1].to_string()),
                stars.captures(a).map(|c| format!("★ {} today", &c[1])),
            ]
            .into_iter()
            .flatten()
            .collect();
            let body = if meta.is_empty() {
                d
            } else {
                format!("{d}\n{}", meta.join(" · "))
            };
            Some(item(
                "GitHub Trending",
                &repo,
                &format!("https://github.com/{repo}"),
                body,
                Some(format!("https://opengraph.githubassets.com/1/{repo}")),
                None,
                "news",
                None,
                None,
            ))
        })
        .collect())
}

fn git_sync(name: &str, url: &str) -> std::path::PathBuf {
    let path = cache_dir().join("repos").join(name);
    if path.join(".git").exists() {
        let fh = path.join(".git/FETCH_HEAD");
        let age = std::fs::metadata(if fh.exists() { &fh } else { &path })
            .and_then(|m| m.modified())
            .map(|t| t.elapsed().unwrap_or_default().as_secs())
            .unwrap_or(u64::MAX);
        if age > 3 * 86400 {
            let _ = Command::new("git")
                .args(["-C"])
                .arg(&path)
                .args(["pull", "-q", "--depth", "1"])
                .output();
        }
    } else {
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        let _ = Command::new("git")
            .args(["clone", "-q", "--depth", "1", url])
            .arg(&path)
            .output();
    }
    path
}

fn md_files(dir: &Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "md"))
                .collect()
        })
        .unwrap_or_default()
}

fn pick<T>(mut v: Vec<T>, k: usize) -> Vec<T> {
    fastrand::shuffle(&mut v);
    v.truncate(k);
    v
}

fn src_tldr_pages() -> Result<Vec<Item>> {
    let root = git_sync("tldr", "https://github.com/tldr-pages/tldr.git");
    let mut files = md_files(&root.join("pages/common"));
    files.extend(md_files(&root.join("pages/osx")));
    let ex = re(r"- (.+?)\n\n`(.+?)`");
    Ok(pick(files, 8)
        .into_iter()
        .filter_map(|f| {
            let t = std::fs::read_to_string(&f).ok()?;
            let name = f.file_stem()?.to_string_lossy().to_string();
            let desc: Vec<&str> = t
                .lines()
                .filter(|l| l.starts_with("> ") && !l.contains("More information"))
                .map(|l| &l[2..])
                .collect();
            let exs: Vec<String> = ex
                .captures_iter(&t)
                .take(3)
                .map(|c| format!("· {}\n\x01$ {}", c[1].trim_end_matches(':'), &c[2]))
                .collect();
            let body = format!("{}\n{}", desc.join(" "), exs.join("\n"));
            Some(item(
                "tldr",
                &name,
                &format!("https://tldr.inbrowser.app/pages/common/{name}"),
                body.trim().to_string(),
                None,
                None,
                "tip",
                None,
                Some(format!("tldr-{name}")),
            ))
        })
        .collect())
}

fn src_til() -> Result<Vec<Item>> {
    let root = git_sync("til", "https://github.com/jbranchaud/til.git");
    let mut files = Vec::new();
    for e in std::fs::read_dir(&root)?.flatten() {
        let p = e.path();
        if p.is_dir() && !e.file_name().to_string_lossy().starts_with('.') {
            files.extend(md_files(&p));
        }
    }
    let link = re(r"\[([^\]]+)\]\([^)]+\)");
    Ok(pick(files, 8)
        .into_iter()
        .filter_map(|f| {
            let t = std::fs::read_to_string(&f).ok()?;
            let mut lines = t.lines();
            let title = lines.next()?.trim_start_matches('#').trim().to_string();
            let rest: String = lines.collect::<Vec<_>>().join("\n");
            let body = mark_code_fences(&link.replace_all(rest.trim(), "$1"));
            let body = if body.chars().count() > 420 {
                format!("{}\n…", truncate(&body, 400))
            } else {
                body
            };
            let rel = f.strip_prefix(&root).ok()?.to_string_lossy().to_string();
            let topic = f.parent()?.file_name()?.to_string_lossy().to_string();
            Some(item(
                &format!("TIL · {topic}"),
                &title,
                &format!("https://github.com/jbranchaud/til/blob/master/{rel}"),
                body,
                None,
                None,
                "tip",
                None,
                Some(format!("til-{rel}")),
            ))
        })
        .collect())
}

fn og_image(url: &str) -> Option<String> {
    let resp = agent()
        .get(url)
        .timeout(Duration::from_secs(5))
        .call()
        .ok()?;
    if !resp.content_type().contains("html") {
        return None;
    }
    let mut buf = Vec::new();
    resp.into_reader()
        .take(200_000)
        .read_to_end(&mut buf)
        .ok()?;
    let page = String::from_utf8_lossy(&buf);
    static R: OnceLock<[Regex; 2]> = OnceLock::new();
    let r = R.get_or_init(|| {
        [
            re(r#"<meta[^>]+(?:property|name)=["'](?:og:image|twitter:image)["'][^>]+content=["']([^"']+)"#),
            re(r#"<meta[^>]+content=["']([^"']+)["'][^>]+(?:property|name)=["'](?:og:image|twitter:image)"#),
        ]
    });
    let img = html_escape::decode_html_entities(&r.iter().find_map(|r| r.captures(&page))?[1])
        .to_string();
    Some(if img.starts_with("//") {
        format!("https:{img}")
    } else if img.starts_with('/') {
        let origin = re(r"^https?://[^/]+").find(url)?.as_str().to_string();
        format!("{origin}{img}")
    } else {
        img
    })
}

fn src_devto() -> Result<Vec<Item>> {
    let v = jget("https://dev.to/api/articles?top=1&per_page=15")?;
    Ok(v.as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|a| {
            let tags = a["tag_list"]
                .as_array()
                .map(|t| {
                    t.iter()
                        .filter_map(|x| x.as_str())
                        .map(|x| format!("#{x}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            let body = format!(
                "{}\n▲ {} · {}",
                s(a, "description"),
                n(a, "positive_reactions_count"),
                tags
            );
            let img = a["cover_image"]
                .as_str()
                .or(a["social_image"].as_str())
                .map(String::from);
            item(
                "DEV",
                &s(a, "title"),
                &s(a, "url"),
                body,
                img,
                None,
                "news",
                None,
                None,
            )
        })
        .collect())
}

type Source = (&'static str, fn() -> Result<Vec<Item>>);
const SOURCES: &[Source] = &[
    ("hn", src_hn),
    ("lobsters", src_lobsters),
    ("feeds", src_feeds),
    ("reddit", src_reddit),
    ("trending", src_trending),
    ("devto", src_devto),
    ("tldr", src_tldr_pages),
    ("til", src_til),
];

pub fn fetch_all() {
    let lock = cache_dir().join("fetch.lock");
    if let Ok(m) = std::fs::metadata(&lock).and_then(|m| m.modified())
        && m.elapsed().unwrap_or_default().as_secs() < 300
    {
        println!("fetch already running");
        return;
    }
    let _ = std::fs::write(&lock, b"");
    let mut items: Vec<Item> = std::thread::scope(|sc| {
        let hs: Vec<_> = SOURCES
            .iter()
            .map(|(name, f)| (name, sc.spawn(f)))
            .collect();
        hs.into_iter()
            .flat_map(|(name, h)| match h.join() {
                Ok(Ok(v)) => {
                    log(&format!("ok {name} {}", v.len()));
                    println!("{name:<14} {}", v.len());
                    v
                }
                Ok(Err(e)) => {
                    log(&format!("fail {name}: {e}"));
                    println!("{name:<14} FAIL {e}");
                    vec![]
                }
                Err(_) => vec![],
            })
            .collect()
    });
    // og:image for plain links so most cards get a picture
    let need: Vec<usize> = (0..items.len())
        .filter(|&i| {
            items[i].image.is_none()
                && items[i].video.is_none()
                && items[i].kind == "news"
                && !items[i].url.is_empty()
        })
        .collect();
    let found: Vec<(usize, Option<String>)> = std::thread::scope(|sc| {
        need.chunks(need.len().div_ceil(12).max(1))
            .map(|chunk| {
                let urls: Vec<(usize, String)> =
                    chunk.iter().map(|&i| (i, items[i].url.clone())).collect();
                sc.spawn(move || {
                    urls.into_iter()
                        .map(|(i, u)| (i, og_image(&u)))
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_default())
            .collect()
    });
    for (i, img) in found {
        // og:image is page-supplied: hold it to the same rule as every other image URL
        items[i].image = img.map(|u| safe_url(&u)).filter(|u| !u.is_empty());
    }
    for it in items.iter_mut() {
        if let (None, Some(id)) = (&it.image, it.video.as_deref().and_then(yt_id)) {
            it.image = Some(format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg"));
        }
    }
    if !items.is_empty() {
        println!("total {}", items.len());
        write_json(
            &cache_dir().join("items.json"),
            &Store {
                fetched: now(),
                items: items.clone(),
            },
        );
    }
    let _ = std::fs::remove_file(lock);
    // clips after the item list: cards show right away, videos join as their clip lands
    if !items.is_empty() {
        prefetch_all(&items);
    }
}

// ---------------------------------------------------------------- video prefetch

/// Where a clip of `url` is cached once prefetched (written as .part, renamed when complete).
pub fn video_cache(url: &str) -> std::path::PathBuf {
    let dir = cache_dir().join("media");
    let _ = std::fs::create_dir_all(&dir);
    dir.join(format!(
        "{}.mp4",
        &sha1_smol::Sha1::from(url).digest().to_string()[..20]
    ))
}

/// A link ffmpeg can read itself (a file or an HLS playlist); anything else goes through yt-dlp.
pub fn is_direct_video(url: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| re(r"(?i)\.(mp4|webm|mov|gif|m3u8)(\?|$)"))
        .is_match(url)
}

/// Download the first seconds of a video (YouTube through yt-dlp, the rest through ffmpeg).
pub fn prefetch_video(url: &str) {
    let path = video_cache(url);
    let part = path.with_extension("part.mp4");
    if path.exists() || part.exists() {
        return;
    }
    let secs = crate::config().video_max + 5;
    let ok = if is_direct_video(url) {
        Command::new("ffmpeg")
            .args([
                "-loglevel",
                "error",
                "-y",
                "-t",
                &secs.to_string(),
                "-i",
                url,
                "-an",
                "-c",
                "copy",
            ])
            .arg(&part)
            .stdin(Stdio::null())
            .output()
    } else {
        Command::new("yt-dlp")
            .args([
                "-q",
                "--no-warnings",
                "--no-part",
                "-f",
                "18/b[height<=480]/b",
                "--download-sections",
            ])
            .arg(format!("*0-{secs}"))
            .args(["--force-overwrites", "-o"])
            .arg(&part)
            .arg("--")
            .arg(url)
            .stdin(Stdio::null())
            .output()
    }
    .map(|o| o.status.success())
    .unwrap_or(false);
    if ok && part.exists() {
        let _ = std::fs::rename(&part, &path);
    } else {
        let _ = std::fs::remove_file(&part);
        log(&format!("prefetch failed {url}"));
    }
}

/// A partial clip untouched this long belongs to a download that died.
const PART_STALE_SECS: u64 = 15 * 60;

/// Fetch up to `video_prefetch` clips the cache doesn't have yet and drop clips no item uses.
fn prefetch_all(items: &[Item]) {
    let urls: Vec<String> = items.iter().filter_map(|x| x.video.clone()).collect();
    let keep: std::collections::HashSet<std::path::PathBuf> =
        urls.iter().map(|u| video_cache(u)).collect();
    if let Ok(rd) = std::fs::read_dir(cache_dir().join("media")) {
        for e in rd.flatten() {
            let p = e.path();
            if !p.extension().is_some_and(|x| x == "mp4") || keep.contains(&p) {
                continue;
            }
            // a .part.mp4 may be another fetch's (or the pane's) download in progress: only
            // clear it once it's clearly abandoned
            let partial = p.to_string_lossy().ends_with(".part.mp4");
            let age = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .map_or(0, |d| d.as_secs());
            if !partial || age > PART_STALE_SECS {
                let _ = std::fs::remove_file(p);
            }
        }
    }
    // yt-dlp's per-video startup (~7s) dominates, so clips download PARALLEL at a time, up to
    // `video_prefetch` unseen ones per refresh; each refresh tops them up. Uncached video cards
    // simply wait in the cache.
    const PARALLEL: usize = 8;
    let seen = crate::load_seen();
    let todo: Vec<&String> = items
        .iter()
        .filter(|x| !seen.contains(&x.id))
        .filter_map(|x| x.video.as_ref())
        .filter(|u| !video_cache(u).exists())
        .take(crate::config().video_prefetch)
        .collect();
    for batch in todo.chunks(PARALLEL) {
        std::thread::scope(|sc| {
            for u in batch {
                sc.spawn(move || prefetch_video(u));
            }
        });
    }
    let ready = urls.iter().filter(|u| video_cache(u).exists()).count();
    println!("videos         {ready}/{} cached", urls.len());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_blocks_keep_indentation_and_literal_entities() {
        let html = "<p>Try   this:</p><pre>if x:\n    return &amp;lt;</pre>";
        let s = strip_html(html, 500);
        assert_eq!(s, "Try this:\n\x01if x:\n\x01    return &lt;");
    }

    #[test]
    fn prose_still_collapses_spaces_and_decodes_once() {
        assert_eq!(
            strip_html("a&nbsp;&nbsp; b &amp;amp; c", 500),
            "a b &amp; c"
        );
    }

    #[test]
    fn only_absolute_http_image_urls_survive() {
        assert_eq!(safe_url("images/hero.png"), "");
        assert_eq!(safe_url("data:image/png;base64,xx"), "");
        assert_eq!(safe_url("https://x.dev/a b.png"), "");
        assert_eq!(safe_url(" https://x.dev/a.png "), "https://x.dev/a.png");
    }

    #[test]
    fn direct_video_links_skip_yt_dlp() {
        assert!(is_direct_video("https://v.redd.it/x/HLSPlaylist.m3u8"));
        assert!(is_direct_video("https://x.dev/clip.MP4?t=1"));
        assert!(!is_direct_video("https://www.youtube.com/watch?v=abc"));
    }
}
