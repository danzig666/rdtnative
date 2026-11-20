//! HLS master playlists (v.redd.it's `HLSPlaylist.m3u8`): the heights a video offers, and a copy
//! reduced to a height cap. The TV's media service takes no bitrate or size limit, so a capped
//! video is played from such a copy, written to a local file (after ytnative's `filter_hls`).

/// Whether `url` names an HLS playlist (`.m3u8`, before any query).
pub fn is_hls(url: &str) -> bool {
    url.split(['?', '#']).next().unwrap_or("").to_ascii_lowercase().ends_with(".m3u8")
}

/// The variant heights `master` offers (`RESOLUTION=WxH`), highest first, each once.
pub fn heights(master: &str) -> Vec<u32> {
    let mut out: Vec<u32> = master.lines().filter(|l| l.starts_with("#EXT-X-STREAM-INF")).filter_map(height).collect();
    out.sort_unstable_by(|a, b| b.cmp(a));
    out.dedup();
    out
}

/// `master` (read from `url`) for a player that must stay at or below `max_height`: the variants
/// that tall or smaller, the media groups (audio) they use and the playlist-wide tags, with every
/// address made absolute so the copy plays from a local file. An address without a query gets the
/// master's (v.redd.it signs its playlists that way). When no variant fits, the whole master with
/// absolute addresses: it plays somehow, where an empty one never would.
pub fn filter(master: &str, url: &str, max_height: u32) -> String {
    let base = reqwest::Url::parse(url).ok();
    let abs = |uri: &str| absolute(base.as_ref(), uri);
    let lines: Vec<&str> = master.lines().map(str::trim_end).filter(|l| !l.is_empty()).collect();
    let fits = |l: &str| height(l).is_some_and(|h| h <= max_height);
    let kept: Vec<&str> = lines.iter().copied().filter(|l| l.starts_with("#EXT-X-STREAM-INF") && fits(l)).collect();
    if kept.is_empty() {
        return lines.iter().map(|l| absolute_line(l, &abs)).collect::<Vec<_>>().join("\n") + "\n";
    }
    // The groups the kept variants name (AUDIO="...", SUBTITLES="...", ...).
    let groups: Vec<&str> = kept
        .iter()
        .flat_map(|l| attrs(l).into_iter().filter(|(k, _)| matches!(*k, "AUDIO" | "VIDEO" | "SUBTITLES" | "CLOSED-CAPTIONS")).map(|(_, v)| v))
        .collect();
    let mut out = Vec::new();
    let mut skip_uri = false;
    for l in lines {
        if l.starts_with("#EXT-X-STREAM-INF") {
            skip_uri = !fits(l);
            if !skip_uri {
                out.push(l.to_string());
            }
            continue;
        }
        if !l.starts_with('#') {
            if !std::mem::take(&mut skip_uri) {
                out.push(abs(l));
            }
            continue;
        }
        if l.starts_with("#EXT-X-I-FRAME-STREAM-INF") && !fits(l) {
            continue;
        }
        if l.starts_with("#EXT-X-MEDIA:") && !attrs(l).iter().any(|(k, v)| *k == "GROUP-ID" && groups.contains(v)) {
            continue;
        }
        out.push(absolute_line(l, &abs));
    }
    out.join("\n") + "\n"
}

/// A variant's height, from its `RESOLUTION=WxH`.
fn height(line: &str) -> Option<u32> {
    let (_, r) = attrs(line).into_iter().find(|(k, _)| *k == "RESOLUTION")?;
    r.split(['x', 'X']).nth(1)?.trim().parse().ok().filter(|h| *h > 0)
}

/// A tag's attributes (`#EXT-X-STREAM-INF:A=1,B="x,y"`) as (name, value without its quotes).
fn attrs(line: &str) -> Vec<(&str, &str)> {
    let Some((_, mut rest)) = line.split_once(':') else { return Vec::new() };
    let mut out = Vec::new();
    while let Some((name, after)) = rest.split_once('=') {
        let (value, next) = match after.strip_prefix('"') {
            Some(q) => match q.find('"') {
                Some(end) => (&q[..end], &q[end + 1..]),
                None => (q, ""),
            },
            None => after.split_once(',').map_or((after, ""), |(v, n)| (v, n)),
        };
        out.push((name.trim(), value));
        rest = next.strip_prefix(',').unwrap_or(next);
    }
    out
}

/// A playlist line with an address: a URI line made absolute, or a tag whose `URI="..."` is.
fn absolute_line(line: &str, abs: &impl Fn(&str) -> String) -> String {
    if !line.starts_with('#') {
        return abs(line);
    }
    let mut out = String::new();
    let mut rest = line;
    while let Some(i) = rest.find("URI=\"") {
        let start = i + 5;
        let Some(len) = rest[start..].find('"') else { break };
        // Only the URI attribute itself (after ':' or ','), not a name that ends in "URI".
        let own = i > 0 && matches!(rest.as_bytes()[i - 1], b':' | b',');
        out.push_str(&rest[..start]);
        let value = &rest[start..start + len];
        out.push_str(&if own { abs(value) } else { value.to_string() });
        rest = &rest[start + len..];
    }
    out.push_str(rest);
    out
}

/// `uri` against the master's address; one on the master's host without a query gets the master's.
fn absolute(base: Option<&reqwest::Url>, uri: &str) -> String {
    let Some(base) = base else { return uri.to_string() };
    let Ok(mut u) = base.join(uri) else { return uri.to_string() };
    if u.query().is_none() && u.host_str() == base.host_str() {
        u.set_query(base.query());
    }
    u.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://v.redd.it/abc/HLSPlaylist.m3u8?a=1&v=1&f=sd";
    const MASTER: &str = "#EXTM3U
#EXT-X-VERSION:6
#EXT-X-INDEPENDENT-SEGMENTS
#EXT-X-MEDIA:URI=\"HLS_AUDIO_160_K.m3u8\",TYPE=AUDIO,GROUP-ID=\"160\",LANGUAGE=\"en\",NAME=\"Audio\",DEFAULT=YES,AUTOSELECT=YES
#EXT-X-MEDIA:URI=\"HLS_AUDIO_64_K.m3u8\",TYPE=AUDIO,GROUP-ID=\"64\",LANGUAGE=\"en\",NAME=\"Audio\",DEFAULT=YES,AUTOSELECT=YES

#EXT-X-STREAM-INF:RESOLUTION=1920x1080,CODECS=\"avc1.4d4028,mp4a.40.2\",AUDIO=\"160\",BANDWIDTH=5000000,FRAME-RATE=30.000
HLS_1080.m3u8
#EXT-X-STREAM-INF:RESOLUTION=1280x720,CODECS=\"avc1.4d401f,mp4a.40.2\",AUDIO=\"64\",BANDWIDTH=2500000,FRAME-RATE=30.000
HLS_720.m3u8
#EXT-X-STREAM-INF:RESOLUTION=854x480,CODECS=\"avc1.4d401e,mp4a.40.2\",AUDIO=\"64\",BANDWIDTH=1200000,FRAME-RATE=30.000
HLS_480.m3u8
#EXT-X-STREAM-INF:RESOLUTION=640x360,CODECS=\"avc1.4d401e,mp4a.40.2\",AUDIO=\"64\",BANDWIDTH=800000,FRAME-RATE=30.000
HLS_360.m3u8?x=2
#EXT-X-I-FRAME-STREAM-INF:RESOLUTION=1920x1080,URI=\"HLS_1080_iframes.m3u8\"
#EXT-X-I-FRAME-STREAM-INF:RESOLUTION=640x360,URI=\"HLS_360_iframes.m3u8\"
";

    #[test]
    fn finds_hls_addresses() {
        assert!(is_hls(URL));
        assert!(is_hls("https://v.redd.it/abc/HLS_720.M3U8"));
        assert!(!is_hls("https://v.redd.it/abc/DASHPlaylist.mpd?a=1"));
        assert!(!is_hls("https://v.redd.it/abc/DASH_720.mp4?source=m3u8"));
    }

    #[test]
    fn heights_highest_first() {
        assert_eq!(heights(MASTER), [1080, 720, 480, 360]);
        let twice = format!("{MASTER}#EXT-X-STREAM-INF:BANDWIDTH=1,RESOLUTION=1280x720\nother.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=9\nnoresolution.m3u8\n");
        assert_eq!(heights(&twice), [1080, 720, 480, 360]);
        assert!(heights("#EXTM3U\n#EXTINF:4.0,\nseg0.ts\n").is_empty());
    }

    #[test]
    fn keeps_variants_within_the_cap_and_their_audio() {
        let f = filter(MASTER, URL, 720);
        assert!(!f.contains("HLS_1080.m3u8"), "{f}");
        assert!(f.contains("\nhttps://v.redd.it/abc/HLS_720.m3u8?a=1&v=1&f=sd\n"), "{f}");
        assert!(f.contains("\nhttps://v.redd.it/abc/HLS_480.m3u8?a=1&v=1&f=sd\n"), "{f}");
        // A variant with its own query keeps it.
        assert!(f.contains("\nhttps://v.redd.it/abc/HLS_360.m3u8?x=2\n"), "{f}");
        // Only the audio group the kept variants use, its address made absolute.
        assert!(f.contains("URI=\"https://v.redd.it/abc/HLS_AUDIO_64_K.m3u8?a=1&v=1&f=sd\""), "{f}");
        assert!(!f.contains("HLS_AUDIO_160_K"), "{f}");
        assert!(!f.contains("HLS_1080_iframes") && f.contains("HLS_360_iframes.m3u8?a=1"), "{f}");
        assert!(f.starts_with("#EXTM3U\n#EXT-X-VERSION:6\n"));
        // Every variant tag is followed by its address.
        let lines: Vec<&str> = f.lines().collect();
        for (i, _) in lines.iter().enumerate().filter(|(_, l)| l.starts_with("#EXT-X-STREAM-INF")) {
            assert!(lines[i + 1].starts_with("https://"), "{f}");
        }
        assert_eq!(heights(&f), [720, 480, 360]);
        let f = filter(MASTER, URL, 1080);
        assert_eq!(heights(&f), [1080, 720, 480, 360]);
        assert!(f.contains("HLS_AUDIO_160_K") && f.contains("HLS_AUDIO_64_K"));
    }

    #[test]
    fn nothing_fits_plays_the_whole_master() {
        let f = filter(MASTER, URL, 240);
        assert_eq!(heights(&f), [1080, 720, 480, 360]);
        assert!(f.contains("\nhttps://v.redd.it/abc/HLS_1080.m3u8?a=1&v=1&f=sd\n"), "{f}");
        assert!(f.contains("URI=\"https://v.redd.it/abc/HLS_AUDIO_160_K.m3u8?a=1&v=1&f=sd\""), "{f}");
        assert!(!f.lines().any(|l| !l.starts_with('#') && !l.starts_with("https://")), "{f}");
    }

    #[test]
    fn absolute_addresses_and_other_hosts_stay() {
        let m = "#EXTM3U\n#EXT-X-STREAM-INF:RESOLUTION=640x360,AUDIO=\"a\"\nhttps://cdn.example/v/360.m3u8\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",URI=\"/root/audio.m3u8\"\n";
        let f = filter(m, URL, 720);
        assert!(f.contains("\nhttps://cdn.example/v/360.m3u8\n"), "{f}");
        assert!(f.contains("URI=\"https://v.redd.it/root/audio.m3u8?a=1&v=1&f=sd\""), "{f}");
        assert_eq!(attrs("#X:A=1,B=\"x,y\",C=z"), [("A", "1"), ("B", "x,y"), ("C", "z")]);
        assert_eq!(absolute_line("#X:KEYURI=\"k\",URI=\"u\"", &|u| format!("<{u}>")), "#X:KEYURI=\"k\",URI=\"<u>\"");
    }
}
