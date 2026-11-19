//! What a post shows besides its title: the media resolver. Reads the post JSON Reddit returns
//! (`preview`, `media`, `secure_media`, `media_metadata`, `gallery_data`,
//! `crosspost_parent_list`) and the link URL, and classifies it into something the TV can draw or
//! play. Imgur pages (albums, galleries, extension-less links) need one call to Imgur's API with
//! the user's Imgur client ID: [`imgur_api`] builds it and [`parse_imgur`] reads the answer.

use crate::model::Post;
use serde_json::Value;

/// One picture (or short silent clip) with its size, when known.
#[derive(Debug, Clone, PartialEq)]
pub struct Pic {
    pub url: String,
    pub w: u32,
    pub h: u32,
}

/// One gallery or album entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// The full image (or the poster frame of an animated item).
    pub pic: Pic,
    /// An MP4 for animated items (Reddit GIFs, Imgur gifv); `None` for stills.
    pub mp4: Option<String>,
    pub caption: Option<String>,
    /// A link the gallery item points to (Reddit's `outbound_url`).
    pub link: Option<String>,
}

/// A playable video.
#[derive(Debug, Clone, PartialEq)]
pub struct Video {
    /// v.redd.it HLS playlist (video + audio).
    pub hls: Option<String>,
    /// v.redd.it DASH manifest (video + audio).
    pub dash: Option<String>,
    /// A progressive MP4 (v.redd.it's video-only fallback, an Imgur mp4, a Reddit GIF's mp4).
    pub mp4: Option<String>,
    pub w: u32,
    pub h: u32,
    pub duration: u32,
    /// Loops without sound (GIFs and gifv).
    pub gif: bool,
    /// The still shown before it plays.
    pub poster: Option<Pic>,
}

/// Media on another service, resolved through its API before it can be shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImgurRef {
    /// Imgur picture or clip, album, gallery post.
    Image(String),
    Album(String),
    Gallery(String),
    /// A RedGIFs clip (needs a temporary token).
    Redgifs(String),
    /// A Streamable video.
    Streamable(String),
}

impl ImgurRef {
    /// The service's name, for badges.
    pub fn service(&self) -> &'static str {
        match self {
            ImgurRef::Redgifs(_) => "REDGIFS",
            ImgurRef::Streamable(_) => "STREAMABLE",
            _ => "IMGUR",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Media {
    /// A text post (or a link with nothing to show).
    None,
    Image(Pic),
    Gallery(Vec<Item>),
    Video(Video),
    /// An Imgur page: resolve with [`imgur_api`] + [`parse_imgur`] into `Image`/`Gallery`/`Video`.
    Imgur(ImgurRef),
    YouTube { id: String, url: String },
    /// Any other web page (shown as a card with its preview and a QR code).
    Link { url: String, preview: Option<Pic> },
}

impl Media {
    pub fn kind(&self) -> &'static str {
        match self {
            Media::None => "text",
            Media::Image(_) => "image",
            Media::Gallery(_) => "gallery",
            Media::Video(v) if v.gif => "gif",
            Media::Video(_) => "video",
            Media::Imgur(_) => "imgur",
            Media::YouTube { .. } => "youtube",
            Media::Link { .. } => "link",
        }
    }
}

fn u(v: &Value) -> u32 {
    v.as_u64().or_else(|| v.as_f64().map(|f| f as u64)).unwrap_or(0) as u32
}

/// Reddit escapes `&` in media URLs as `&amp;` unless `raw_json=1` was asked; undo it either way.
pub fn unescape(s: &str) -> String {
    s.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'")
}

fn pic(v: &Value) -> Option<Pic> {
    let url = v["url"].as_str().or_else(|| v["u"].as_str()).or_else(|| v["gif"].as_str())?;
    if url.is_empty() {
        return None;
    }
    let w = u(&v["width"]).max(u(&v["x"]));
    let h = u(&v["height"]).max(u(&v["y"]));
    Some(Pic { url: unescape(url), w, h })
}

/// The post the media really lives on: a crosspost carries it on its parent.
pub fn source(post: &Post) -> &Value {
    let parent = &post.raw["crosspost_parent_list"][0];
    if parent.is_object() {
        parent
    } else {
        &post.raw
    }
}

/// All sizes Reddit offers for the post's preview image (the source last). For posts on
/// reddit.com itself, external-preview.redd.it (which often fails) is read from preview.redd.it.
fn preview_sizes(d: &Value) -> Vec<Pic> {
    let img = &d["preview"]["images"][0];
    let mut out: Vec<Pic> = img["resolutions"].as_array().map(|a| a.iter().filter_map(pic).collect()).unwrap_or_default();
    if let Some(src) = pic(&img["source"]) {
        out.push(src);
    }
    if d["domain"].as_str().is_some_and(|dm| dm == "reddit.com" || dm.ends_with(".reddit.com")) {
        for p in &mut out {
            p.url = p.url.replace("://external-preview.redd.it/", "://preview.redd.it/");
        }
    }
    out
}

/// The smallest preview at least `want_w` px wide (or the largest there is). Falls back to the
/// gallery's first item, then to Reddit's small `thumbnail`. `None` for posts without a picture.
pub fn preview(post: &Post, want_w: u32) -> Option<Pic> {
    let d = source(post);
    let sizes = preview_sizes(d);
    if !sizes.is_empty() {
        return pick(&sizes, want_w);
    }
    // Galleries carry no `preview`: their first item's sizes.
    if let Some(first) = gallery_ids(d).first() {
        let m = &d["media_metadata"][first.as_str()];
        let mut sizes: Vec<Pic> = m["p"].as_array().map(|a| a.iter().filter_map(pic).collect()).unwrap_or_default();
        if let Some(s) = pic(&m["s"]) {
            sizes.push(s);
        }
        if !sizes.is_empty() {
            return pick(&sizes, want_w);
        }
    }
    let t = d["thumbnail"].as_str().unwrap_or_default();
    if t.starts_with("http") {
        return Some(Pic { url: unescape(t), w: u(&d["thumbnail_width"]), h: u(&d["thumbnail_height"]) });
    }
    None
}

fn pick(sizes: &[Pic], want_w: u32) -> Option<Pic> {
    sizes.iter().find(|p| p.w >= want_w).or_else(|| sizes.iter().max_by_key(|p| p.w)).cloned()
}

fn gallery_ids(d: &Value) -> Vec<String> {
    d["gallery_data"]["items"]
        .as_array()
        .map(|a| a.iter().filter(|i| !i["is_deleted"].as_bool().unwrap_or(false)).filter_map(|i| i["media_id"].as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

fn gallery(d: &Value) -> Vec<Item> {
    let captions: Vec<(Option<String>, Option<String>)> = d["gallery_data"]["items"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|i| {
                    (
                        i["caption"].as_str().filter(|s| !s.is_empty()).map(unescape),
                        i["outbound_url"].as_str().filter(|s| !s.is_empty()).map(unescape),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    gallery_ids(d)
        .iter()
        .enumerate()
        .filter_map(|(k, id)| {
            let m = &d["media_metadata"][id.as_str()];
            let s = &m["s"];
            let mp4 = s["mp4"].as_str().map(unescape);
            // Reddit marks some items "failed" or "unprocessed" although the upload is there:
            // the original file on i.redd.it (its type from `m`, "image/png").
            let fallback = || {
                let ext = m["m"].as_str().and_then(|t| t.split('/').nth(1)).filter(|e| !e.is_empty()).unwrap_or("jpg");
                let ext = if ext == "jpeg" { "jpg" } else { ext };
                Some(Pic { url: format!("https://i.redd.it/{id}.{ext}"), w: 0, h: 0 })
            };
            let still = pic(s).or_else(|| m["p"].as_array().and_then(|a| a.last()).and_then(pic)).or_else(fallback)?;
            let (caption, link) = captions.get(k).cloned().unwrap_or_default();
            Some(Item { pic: still, mp4, caption, link })
        })
        .collect()
}

fn reddit_video(rv: &Value, poster: Option<Pic>) -> Option<Video> {
    if !rv.is_object() {
        return None;
    }
    let hls = rv["hls_url"].as_str().map(unescape);
    let dash = rv["dash_url"].as_str().map(unescape);
    let mp4 = rv["fallback_url"].as_str().map(unescape);
    if hls.is_none() && dash.is_none() && mp4.is_none() {
        return None;
    }
    Some(Video {
        hls,
        dash,
        mp4,
        w: u(&rv["width"]),
        h: u(&rv["height"]),
        duration: u(&rv["duration"]),
        gif: rv["is_gif"].as_bool().unwrap_or(false),
        poster,
    })
}

/// The YouTube video ID of a youtube.com / youtu.be URL.
pub fn youtube_id(url: &str) -> Option<String> {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.trim_start_matches("www.").trim_start_matches("m.");
    let id = match host {
        "youtu.be" => path.split(['?', '&', '#', '/']).next().unwrap_or(""),
        "youtube.com" | "music.youtube.com" => {
            if let Some(q) = path.strip_prefix("watch?").or_else(|| path.split_once("watch?").map(|x| x.1)) {
                q.split('&').find_map(|kv| kv.strip_prefix("v=")).unwrap_or("")
            } else if let Some(p) = path.strip_prefix("shorts/").or_else(|| path.strip_prefix("embed/")).or_else(|| path.strip_prefix("live/")) {
                p.split(['?', '&', '#', '/']).next().unwrap_or("")
            } else {
                ""
            }
        }
        _ => "",
    };
    (id.len() >= 6 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')).then(|| id.to_string())
}

/// Classifies an Imgur URL: a direct file (`i.imgur.com/x.jpg`, `.gifv`, `.mp4`) needs no API.
fn imgur(url: &str) -> Option<Media> {
    let rest = url.split("://").nth(1)?;
    let (host, path) = rest.split_once('/')?;
    let host = host.trim_start_matches("www.").trim_start_matches("m.");
    if host != "imgur.com" && host != "i.imgur.com" {
        return None;
    }
    let path = path.split(['?', '#']).next().unwrap_or("").trim_end_matches('/');
    let seg: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    // Imgur slugs end with the ID after the last '-' ("cute-cat-AbC12").
    let id_of = |s: &str| s.rsplit('-').next().unwrap_or(s).to_string();
    match seg.as_slice() {
        ["a", s] => Some(Media::Imgur(ImgurRef::Album(id_of(s)))),
        ["gallery", s] | ["t", _, s] => Some(Media::Imgur(ImgurRef::Gallery(id_of(s)))),
        [file] => {
            let (id, ext) = file.rsplit_once('.').map(|(a, b)| (a, b.to_ascii_lowercase())).unwrap_or((file, String::new()));
            if id.is_empty() {
                return None;
            }
            match ext.as_str() {
                "gifv" | "mp4" | "webm" | "gif" => Some(Media::Video(Video {
                    hls: None,
                    dash: None,
                    mp4: Some(format!("https://i.imgur.com/{id}.mp4")),
                    w: 0,
                    h: 0,
                    duration: 0,
                    gif: true,
                    poster: Some(Pic { url: format!("https://i.imgur.com/{id}h.jpg"), w: 0, h: 0 }),
                })),
                "jpg" | "jpeg" | "png" | "webp" => Some(Media::Image(Pic { url: format!("https://i.imgur.com/{id}.{ext}"), w: 0, h: 0 })),
                _ => Some(Media::Imgur(ImgurRef::Image(id.to_string()))),
            }
        }
        _ => None,
    }
}

fn is_image_url(url: &str) -> bool {
    let p = url.split(['?', '#']).next().unwrap_or("").to_ascii_lowercase();
    [".jpg", ".jpeg", ".png", ".webp", ".bmp"].iter().any(|e| p.ends_with(e))
}

/// What the post shows. Imgur pages come back as [`Media::Imgur`] to be resolved.
pub fn resolve(post: &Post) -> Media {
    let d = source(post);
    let url = unwrap_media_url(&unescape(d["url_overridden_by_dest"].as_str().or_else(|| d["url"].as_str()).unwrap_or(&post.url)));
    let poster = preview(post, 1280);

    if d["is_gallery"].as_bool().unwrap_or(false) {
        let items = gallery(d);
        if !items.is_empty() {
            return Media::Gallery(items);
        }
    }
    // v.redd.it.
    for rv in [&d["secure_media"]["reddit_video"], &d["media"]["reddit_video"]] {
        if let Some(v) = reddit_video(rv, poster.clone()) {
            return Media::Video(v);
        }
    }
    // The real host first: Reddit's own preview clip of an Imgur / RedGIFs / Streamable post is
    // small and can lack sound.
    if let Some(id) = youtube_id(&url) {
        return Media::YouTube { id, url };
    }
    if let Some(m) = imgur(&url) {
        return m;
    }
    if let Some(r) = other_service(&url) {
        return Media::Imgur(r);
    }
    if let Some(v) = reddit_video(&d["preview"]["reddit_video_preview"], poster.clone()) {
        return Media::Video(v);
    }
    let mp4 = &d["preview"]["images"][0]["variants"]["mp4"]["source"];
    if mp4.is_object() && url.to_ascii_lowercase().contains(".gif") {
        if let Some(p) = pic(mp4) {
            return Media::Video(Video { hls: None, dash: None, mp4: Some(p.url), w: p.w, h: p.h, duration: 0, gif: true, poster });
        }
    }
    if d["is_self"].as_bool().unwrap_or(post.is_self) {
        return Media::None;
    }
    let host = url.split("://").nth(1).and_then(|r| r.split('/').next()).unwrap_or("");
    if host == "i.redd.it" || is_image_url(&url) || d["post_hint"] == "image" {
        // The full picture; its size from the preview's source.
        let src = preview_sizes(d).pop();
        let (w, h) = src.map_or((0, 0), |p| (p.w, p.h));
        return Media::Image(Pic { url, w, h });
    }
    if url.is_empty() || url.contains("reddit.com/r/") && url.contains("/comments/") {
        return Media::None;
    }
    Media::Link { url, preview: poster }
}

/// The Imgur API URL for `r`; send it with `Authorization: Client-ID <id>`.
pub fn imgur_api(r: &ImgurRef) -> String {
    match r {
        ImgurRef::Image(id) => format!("https://api.imgur.com/3/image/{id}"),
        ImgurRef::Album(id) => format!("https://api.imgur.com/3/album/{id}"),
        ImgurRef::Gallery(id) => format!("https://api.imgur.com/3/gallery/{id}"),
        ImgurRef::Redgifs(id) => format!("https://api.redgifs.com/v2/gifs/{id}"),
        ImgurRef::Streamable(id) => format!("https://api.streamable.com/videos/{id}"),
    }
}

/// RedGIFs' temporary token (send as `Authorization: Bearer <token>` to the gif API).
pub const REDGIFS_TOKEN_URL: &str = "https://api.redgifs.com/v2/auth/temporary";

/// The clip in RedGIFs' answer: the HD MP4, else the SD one; its poster.
pub fn parse_redgifs(v: &Value) -> Option<Media> {
    let g = &v["gif"];
    let mp4 = g["urls"]["hd"].as_str().or_else(|| g["urls"]["sd"].as_str())?.to_string();
    let poster = g["urls"]["poster"].as_str().or_else(|| g["urls"]["thumbnail"].as_str()).map(|p| Pic { url: p.to_string(), w: u(&g["width"]), h: u(&g["height"]) });
    Some(Media::Video(Video {
        hls: None,
        dash: None,
        mp4: Some(mp4),
        w: u(&g["width"]),
        h: u(&g["height"]),
        duration: g["duration"].as_f64().unwrap_or(0.0) as u32,
        gif: !g["hasAudio"].as_bool().unwrap_or(false),
        poster,
    }))
}

/// Streamable's answer: its MP4 (protocol-relative URLs get https).
pub fn parse_streamable(v: &Value) -> Option<Media> {
    let f = &v["files"]["mp4"];
    let f = if f.is_object() { f } else { &v["files"]["mp4-mobile"] };
    let url = f["url"].as_str()?;
    let url = if url.starts_with("//") { format!("https:{url}") } else { url.to_string() };
    let thumb = v["thumbnail_url"].as_str().map(|t| Pic { url: if t.starts_with("//") { format!("https:{t}") } else { t.to_string() }, w: 0, h: 0 });
    Some(Media::Video(Video { hls: None, dash: None, mp4: Some(url), w: u(&f["width"]), h: u(&f["height"]), duration: f["duration"].as_f64().unwrap_or(0.0) as u32, gif: false, poster: thumb }))
}

/// RedGIFs and Streamable links.
fn other_service(url: &str) -> Option<ImgurRef> {
    let rest = url.split("://").nth(1)?;
    let (host, path) = rest.split_once('/')?;
    let host = host.trim_start_matches("www.").trim_start_matches("v3.").to_ascii_lowercase();
    let seg: Vec<&str> = path.split(['?', '#']).next().unwrap_or("").split('/').filter(|s| !s.is_empty()).collect();
    let clean = |s: &str| s.split('.').next().unwrap_or(s).split('-').next().unwrap_or(s).to_ascii_lowercase();
    match (host.as_str(), seg.as_slice()) {
        ("redgifs.com", ["watch" | "ifr", id, ..]) | ("i.redgifs.com", ["i", id, ..]) => Some(ImgurRef::Redgifs(clean(id))),
        ("streamable.com", [id]) | ("streamable.com", ["e" | "o", id, ..]) => Some(ImgurRef::Streamable(id.to_string())),
        _ => None,
    }
}

/// `reddit.com/media?url=<encoded>`: the address it wraps (Reddit now wraps many links this way).
pub fn unwrap_media_url(url: &str) -> String {
    let mut cur = url.to_string();
    for _ in 0..4 {
        let rest = cur.split("://").nth(1).unwrap_or(&cur);
        let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
        let path_only = path.split('?').next().unwrap_or("");
        if !(host.ends_with("reddit.com") && path_only.trim_end_matches('/') == "media") {
            break;
        }
        let Some(q) = path.split_once('?').map(|x| x.1) else { break };
        let Some(v) = q.split('&').find_map(|kv| kv.strip_prefix("url=")) else { break };
        cur = percent_decode(v);
    }
    cur
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn imgur_item(d: &Value) -> Option<Item> {
    let link = d["link"].as_str()?.to_string();
    let mp4 = d["mp4"].as_str().filter(|m| !m.is_empty());
    let animated = d["animated"].as_bool().unwrap_or(false) || mp4.is_some();
    let id = d["id"].as_str().unwrap_or_default();
    let still = if animated && !id.is_empty() { format!("https://i.imgur.com/{id}h.jpg") } else { link.clone() };
    Some(Item {
        pic: Pic { url: still, w: u(&d["width"]), h: u(&d["height"]) },
        mp4: if animated { mp4.map(str::to_string).or(Some(link)) } else { None },
        caption: d["description"].as_str().or_else(|| d["title"].as_str()).filter(|s| !s.is_empty()).map(str::to_string),
        link: None,
    })
}

/// Reads Imgur's answer (an image, an album, or a gallery entry that is either) into media.
pub fn parse_imgur(v: &Value) -> Option<Media> {
    let d = &v["data"];
    let items: Vec<Item> = match d["images"].as_array() {
        Some(a) => a.iter().filter_map(imgur_item).collect(),
        None => imgur_item(d).into_iter().collect(),
    };
    match items.len() {
        0 => None,
        1 if d["images"].as_array().is_none_or(|a| a.len() == 1) => {
            let it = items.into_iter().next()?;
            Some(match it.mp4 {
                Some(mp4) => Media::Video(Video { hls: None, dash: None, mp4: Some(mp4), w: it.pic.w, h: it.pic.h, duration: 0, gif: true, poster: Some(it.pic) }),
                None => Media::Image(it.pic),
            })
        }
        _ => Some(Media::Gallery(items)),
    }
}

/// A link's site name for its card ("nytimes.com").
pub fn host(url: &str) -> String {
    url.split("://").nth(1).and_then(|r| r.split(['/', '?', '#']).next()).unwrap_or(url).trim_start_matches("www.").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn post(d: Value) -> Post {
        Post::from_json(&d)
    }

    #[test]
    fn picks_the_smallest_preview_wide_enough() {
        let p = post(json!({"url":"https://i.redd.it/a.jpg","post_hint":"image","preview":{"images":[{
            "source":{"url":"https://p/src.jpg?a=1&amp;s=2","width":3000,"height":2000},
            "resolutions":[{"url":"https://p/108","width":108,"height":72},{"url":"https://p/640","width":640,"height":426},{"url":"https://p/960","width":960,"height":640}]}]}}));
        assert_eq!(preview(&p, 500).unwrap().url, "https://p/640");
        assert_eq!(preview(&p, 2000).unwrap().url, "https://p/src.jpg?a=1&s=2");
        assert_eq!(preview(&p, 4000).unwrap().w, 3000);
        assert_eq!(resolve(&p), Media::Image(Pic { url: "https://i.redd.it/a.jpg".into(), w: 3000, h: 2000 }));
    }

    #[test]
    fn galleries_keep_order_captions_and_skip_failed_items() {
        let p = post(json!({"is_gallery":true,"url":"https://www.reddit.com/gallery/x",
            "gallery_data":{"items":[{"media_id":"b","caption":"second?"},{"media_id":"bad"},{"media_id":"a","outbound_url":"https://x.y"}]},
            "media_metadata":{
                "a":{"status":"valid","s":{"u":"https://preview.redd.it/a.jpg?x=1&amp;y=2","x":800,"y":600},"p":[{"u":"https://p/a108","x":108,"y":81}]},
                "b":{"status":"valid","s":{"gif":"https://i.redd.it/b.gif","mp4":"https://i.redd.it/b.mp4","x":400,"y":300}},
                "bad":{"status":"failed"}}}));
        let Media::Gallery(items) = resolve(&p) else { panic!("{:?}", resolve(&p)) };
        assert_eq!(items.len(), 3, "a failed item falls back to i.redd.it");
        assert_eq!(items[1].pic.url, "https://i.redd.it/bad.jpg");
        let items: Vec<_> = items.into_iter().filter(|i| !i.pic.url.contains("bad")).collect();
        assert_eq!(items[0].pic.url, "https://i.redd.it/b.gif");
        assert_eq!(items[0].mp4.as_deref(), Some("https://i.redd.it/b.mp4"));
        assert_eq!(items[0].caption.as_deref(), Some("second?"));
        assert_eq!(items[1].pic.url, "https://preview.redd.it/a.jpg?x=1&y=2");
        assert_eq!(items[1].link.as_deref(), Some("https://x.y"));
        assert_eq!(preview(&p, 100).unwrap().url, "https://i.redd.it/b.gif");
    }

    #[test]
    fn reddit_video_and_crossposts() {
        let v = json!({"reddit_video":{"hls_url":"https://v.redd.it/x/HLSPlaylist.m3u8?a=1&amp;b=2","dash_url":"https://v.redd.it/x/DASHPlaylist.mpd","fallback_url":"https://v.redd.it/x/DASH_720.mp4","width":1280,"height":720,"duration":31,"is_gif":false}});
        let p = post(json!({"url":"https://v.redd.it/x","is_video":true,"secure_media":v}));
        let Media::Video(vid) = resolve(&p) else { panic!() };
        assert_eq!(vid.hls.as_deref(), Some("https://v.redd.it/x/HLSPlaylist.m3u8?a=1&b=2"));
        assert_eq!((vid.w, vid.h, vid.duration, vid.gif), (1280, 720, 31, false));
        let x = post(json!({"url":"/r/a/comments/x","crosspost_parent_list":[{"url":"https://v.redd.it/x","secure_media":v}]}));
        assert_eq!(resolve(&x).kind(), "video");
    }

    #[test]
    fn links_youtube_and_text() {
        assert_eq!(youtube_id("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=1").as_deref(), Some("dQw4w9WgXcQ"));
        assert_eq!(youtube_id("https://youtu.be/dQw4w9WgXcQ?si=x").as_deref(), Some("dQw4w9WgXcQ"));
        assert_eq!(youtube_id("https://m.youtube.com/shorts/abcdefghijk").as_deref(), Some("abcdefghijk"));
        assert_eq!(youtube_id("https://example.com/watch?v=x"), None);
        assert_eq!(resolve(&post(json!({"url":"https://youtu.be/dQw4w9WgXcQ"}))).kind(), "youtube");
        assert_eq!(resolve(&post(json!({"is_self":true,"url":"https://www.reddit.com/r/a/comments/x/t/"}))), Media::None);
        let Media::Link { url, .. } = resolve(&post(json!({"url":"https://example.com/news?id=1&amp;b=2"}))) else { panic!() };
        assert_eq!(url, "https://example.com/news?id=1&b=2");
        assert_eq!(host("https://www.example.com/a?b"), "example.com");
    }

    #[test]
    fn imgur_links() {
        let m = |u: &str| resolve(&post(json!({ "url": u })));
        assert_eq!(m("https://imgur.com/a/AbC12"), Media::Imgur(ImgurRef::Album("AbC12".into())));
        assert_eq!(m("https://imgur.com/gallery/funny-cat-XyZ9"), Media::Imgur(ImgurRef::Gallery("XyZ9".into())));
        assert_eq!(m("https://imgur.com/QwErT"), Media::Imgur(ImgurRef::Image("QwErT".into())));
        assert_eq!(m("https://i.imgur.com/QwErT.jpg"), Media::Image(Pic { url: "https://i.imgur.com/QwErT.jpg".into(), w: 0, h: 0 }));
        let Media::Video(v) = m("https://i.imgur.com/QwErT.gifv") else { panic!() };
        assert_eq!(v.mp4.as_deref(), Some("https://i.imgur.com/QwErT.mp4"));
        assert!(v.gif);
        assert_eq!(imgur_api(&ImgurRef::Album("A".into())), "https://api.imgur.com/3/album/A");
    }

    #[test]
    fn redgifs_streamable_and_media_wrappers() {
        let m = |u: &str| resolve(&post(json!({ "url": u })));
        assert_eq!(m("https://www.redgifs.com/watch/SomeFunnyClip"), Media::Imgur(ImgurRef::Redgifs("somefunnyclip".into())));
        assert_eq!(m("https://i.redgifs.com/i/abcdef.jpg"), Media::Imgur(ImgurRef::Redgifs("abcdef".into())));
        assert_eq!(m("https://streamable.com/x1y2z3"), Media::Imgur(ImgurRef::Streamable("x1y2z3".into())));
        assert_eq!(unwrap_media_url("https://www.reddit.com/media?url=https%3A%2F%2Fi.redd.it%2Fa.png"), "https://i.redd.it/a.png");
        assert_eq!(m("https://www.reddit.com/media?url=https%3A%2F%2Fi.redd.it%2Fa.png"), Media::Image(Pic { url: "https://i.redd.it/a.png".into(), w: 0, h: 0 }));
        assert_eq!(unwrap_media_url("https://example.com/media?url=x"), "https://example.com/media?url=x");
        let rg = json!({"gif":{"urls":{"hd":"https://media.redgifs.com/A.mp4","sd":"https://media.redgifs.com/A-mobile.mp4","poster":"https://media.redgifs.com/A-poster.jpg"},"width":1080,"height":1920,"duration":12.5,"hasAudio":true}});
        let Some(Media::Video(v)) = parse_redgifs(&rg) else { panic!() };
        assert_eq!((v.mp4.as_deref(), v.duration, v.gif), (Some("https://media.redgifs.com/A.mp4"), 12, false));
        let st = json!({"files":{"mp4":{"url":"//cdn.streamable.com/video/mp4/x.mp4","width":1280,"height":720,"duration":30.2}},"thumbnail_url":"//cdn/x.jpg"});
        let Some(Media::Video(v)) = parse_streamable(&st) else { panic!() };
        assert_eq!(v.mp4.as_deref(), Some("https://cdn.streamable.com/video/mp4/x.mp4"));
    }

    #[test]
    fn failed_gallery_items_use_the_original_upload() {
        let p = post(json!({"is_gallery":true,"gallery_data":{"items":[{"media_id":"abc"},{"media_id":"def"}]},
            "media_metadata":{"abc":{"status":"failed"},"def":{"status":"unprocessed","m":"image/png"}}}));
        let Media::Gallery(items) = resolve(&p) else { panic!() };
        assert_eq!(items.iter().map(|i| i.pic.url.as_str()).collect::<Vec<_>>(), ["https://i.redd.it/abc.jpg", "https://i.redd.it/def.png"]);
    }

    #[test]
    fn real_host_before_the_preview_clip_and_empty_imgur_mp4() {
        let p = post(json!({"url":"https://i.imgur.com/AbC.gifv","preview":{"reddit_video_preview":{"fallback_url":"https://v.redd.it/x/DASH_240.mp4","is_gif":true}}}));
        let Media::Video(v) = resolve(&p) else { panic!() };
        assert_eq!(v.mp4.as_deref(), Some("https://i.imgur.com/AbC.mp4"));
        let one = json!({"data":{"id":"g","link":"https://i.imgur.com/g.gif","type":"image/gif","mp4":""}});
        assert_eq!(parse_imgur(&one), Some(Media::Image(Pic { url: "https://i.imgur.com/g.gif".into(), w: 0, h: 0 })));
    }

    #[test]
    fn imgur_api_answers() {
        let album = json!({"data":{"images":[
            {"id":"a","link":"https://i.imgur.com/a.jpg","width":10,"height":20,"description":"hi"},
            {"id":"b","link":"https://i.imgur.com/b.mp4","animated":true,"mp4":"https://i.imgur.com/b.mp4"}]}});
        let Some(Media::Gallery(items)) = parse_imgur(&album) else { panic!() };
        assert_eq!(items[0].caption.as_deref(), Some("hi"));
        assert_eq!(items[1].pic.url, "https://i.imgur.com/bh.jpg");
        assert_eq!(items[1].mp4.as_deref(), Some("https://i.imgur.com/b.mp4"));
        let one = json!({"data":{"id":"c","link":"https://i.imgur.com/c.png","width":5,"height":6}});
        assert_eq!(parse_imgur(&one), Some(Media::Image(Pic { url: "https://i.imgur.com/c.png".into(), w: 5, h: 6 })));
        assert_eq!(parse_imgur(&json!({"data":{"error":"x"}})), None);
    }
}
