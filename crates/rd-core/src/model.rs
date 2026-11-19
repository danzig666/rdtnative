//! The Reddit "thing" shapes rdtnative reads. Parsed field by field from `serde_json::Value` so a
//! missing or renamed field degrades to a default instead of failing a whole listing.

use crate::RdError;
use serde_json::Value;

pub(crate) fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_string()
}

pub(crate) fn opt_s(v: &Value, k: &str) -> Option<String> {
    v[k].as_str().filter(|x| !x.is_empty()).map(str::to_string)
}

pub(crate) fn i(v: &Value, k: &str) -> i64 {
    v[k].as_i64().or_else(|| v[k].as_f64().map(|f| f as i64)).unwrap_or(0)
}

pub(crate) fn b(v: &Value, k: &str) -> bool {
    v[k].as_bool().unwrap_or(false)
}

/// A page of things plus the cursors for the next/previous page.
#[derive(Debug, Clone, PartialEq)]
pub struct Listing<T> {
    pub items: Vec<T>,
    pub after: Option<String>,
    pub before: Option<String>,
}

/// Parses `{"kind":"Listing","data":{"children":[{"kind":..,"data":..}],"after":..}}`, keeping
/// the children of `kind` and turning each with `f`.
pub fn parse_listing<T>(v: &Value, kind: &str, f: impl Fn(&Value) -> T) -> Result<Listing<T>, RdError> {
    if v["kind"] != "Listing" {
        return Err(RdError::Parse(format!("expected a Listing, got {}", v["kind"])));
    }
    let d = &v["data"];
    let items = d["children"]
        .as_array()
        .map(|c| c.iter().filter(|t| t["kind"] == kind).map(|t| f(&t["data"])).collect())
        .unwrap_or_default();
    Ok(Listing { items, after: opt_s(d, "after"), before: opt_s(d, "before") })
}

#[derive(Debug, Clone, PartialEq)]
pub struct Post {
    pub id: String,
    /// Fullname, `t3_<id>`.
    pub name: String,
    pub subreddit: String,
    pub title: String,
    pub author: String,
    pub score: i64,
    pub upvote_ratio: f32,
    pub num_comments: i64,
    pub created_utc: i64,
    pub permalink: String,
    pub url: String,
    pub domain: String,
    pub is_self: bool,
    /// Markdown body of a self post (with `raw_json=1`, not HTML-escaped).
    pub selftext: String,
    pub over_18: bool,
    pub spoiler: bool,
    pub stickied: bool,
    pub locked: bool,
    pub link_flair_text: Option<String>,
    /// `true` up, `false` down, `None` no vote (signed in only).
    pub likes: Option<bool>,
    pub saved: bool,
    /// `image`, `hosted:video`, `rich:video`, `link`, `self`, … (absent on many posts).
    pub post_hint: Option<String>,
    pub is_video: bool,
    pub is_gallery: bool,
    /// Kept whole for the media resolver (Phase 2): `preview`, `media`, `media_metadata`,
    /// `gallery_data`, `secure_media`, `crosspost_parent_list`.
    pub raw: Value,
}

impl Post {
    pub fn from_json(d: &Value) -> Post {
        Post {
            id: s(d, "id"),
            name: s(d, "name"),
            subreddit: s(d, "subreddit"),
            title: s(d, "title"),
            author: s(d, "author"),
            score: i(d, "score"),
            upvote_ratio: d["upvote_ratio"].as_f64().unwrap_or(0.0) as f32,
            num_comments: i(d, "num_comments"),
            created_utc: i(d, "created_utc"),
            permalink: s(d, "permalink"),
            url: s(d, "url"),
            domain: s(d, "domain"),
            is_self: b(d, "is_self"),
            selftext: s(d, "selftext"),
            over_18: b(d, "over_18"),
            spoiler: b(d, "spoiler"),
            stickied: b(d, "stickied"),
            locked: b(d, "locked"),
            link_flair_text: flair(d, "link_flair_richtext", "link_flair_text"),
            likes: d["likes"].as_bool(),
            saved: b(d, "saved"),
            post_hint: opt_s(d, "post_hint"),
            is_video: b(d, "is_video"),
            is_gallery: b(d, "is_gallery"),
            raw: d.clone(),
        }
    }
}

/// A flair's text: from its richtext parts when there are any (so `:emoji:` codes are left
/// out), else the plain text.
pub(crate) fn flair(d: &Value, rich: &str, plain: &str) -> Option<String> {
    if let Some(parts) = d[rich].as_array().filter(|a| !a.is_empty()) {
        let t: String = parts.iter().filter(|p| p["e"] == "text").filter_map(|p| p["t"].as_str()).collect::<String>();
        let t = t.trim().to_string();
        return (!t.is_empty()).then_some(t);
    }
    opt_s(d, plain).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Reddit's stand-in for an NSFW (18+) subreddit's posts when the app may not show them (signed
/// out, or an account that does not allow mature content): one post by u/redditmaturecontent,
/// "Go to Reddit to view mature content".
pub fn is_mature_gate(p: &Post) -> bool {
    p.author.eq_ignore_ascii_case("redditmaturecontent") || p.subreddit.eq_ignore_ascii_case("u_redditmaturecontent")
}

pub fn parse_posts(v: &Value) -> Result<Listing<Post>, RdError> {
    parse_listing(v, "t3", Post::from_json)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Subreddit {
    pub name: String,
    pub display_name: String,
    pub title: String,
    pub subscribers: i64,
    pub over18: bool,
    pub icon: Option<String>,
    pub public_description: String,
}

impl Subreddit {
    pub fn from_json(d: &Value) -> Subreddit {
        Subreddit {
            name: s(d, "name"),
            display_name: s(d, "display_name"),
            title: s(d, "title"),
            subscribers: i(d, "subscribers"),
            over18: b(d, "over18"),
            icon: opt_s(d, "community_icon").or_else(|| opt_s(d, "icon_img")),
            public_description: s(d, "public_description"),
        }
    }
}

pub fn parse_subreddits(v: &Value) -> Result<Listing<Subreddit>, RdError> {
    parse_listing(v, "t5", Subreddit::from_json)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let p = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    #[test]
    fn parses_a_front_page() {
        let l = parse_posts(&fixture("listing_hot.json")).unwrap();
        assert_eq!(l.items.len(), 3);
        assert_eq!(l.after.as_deref(), Some("t3_1abc003"));
        assert_eq!(l.before, None);
        let p = &l.items[0];
        assert_eq!((p.id.as_str(), p.name.as_str(), p.subreddit.as_str()), ("1abc001", "t3_1abc001", "pics"));
        assert_eq!(p.post_hint.as_deref(), Some("image"));
        assert!(p.raw["preview"]["images"][0]["source"]["url"].as_str().unwrap().contains("&s="));
        assert_eq!(l.items[1].likes, Some(true));
        assert!(l.items[1].is_video);
        assert!(l.items[2].is_self && l.items[2].over_18);
        assert!(!l.items.iter().any(is_mature_gate));
        let gate = serde_json::json!({"kind":"Listing","data":{"children":[{"kind":"t3","data":{"id":"14m5j89","subreddit":"u_redditmaturecontent","author":"redditmaturecontent","title":"Go to Reddit to view mature content","is_self":true,"over_18":false}}]}});
        assert!(parse_posts(&gate).unwrap().items.iter().all(is_mature_gate));
        assert_eq!(l.items[2].selftext, "Some **markdown** & text");
    }

    #[test]
    fn rejects_non_listings_and_skips_foreign_kinds() {
        assert!(parse_posts(&serde_json::json!({"kind": "t3"})).is_err());
        let mixed = serde_json::json!({"kind":"Listing","data":{"children":[
            {"kind":"t5","data":{"display_name":"rust"}},{"kind":"t3","data":{"id":"x"}}],"after":null}});
        let l = parse_posts(&mixed).unwrap();
        assert_eq!(l.items.len(), 1);
        assert_eq!(parse_subreddits(&mixed).unwrap().items[0].display_name, "rust");
    }
}
