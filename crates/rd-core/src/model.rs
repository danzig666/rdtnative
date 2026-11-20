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
    parse_listing_with(v, |k, d| (k == kind).then(|| f(d)))
}

/// [`parse_listing`] for listings that mix kinds (a user's overview: posts and comments): `f`
/// gets each child's kind and data and keeps what it returns.
pub fn parse_listing_with<T>(v: &Value, f: impl Fn(&str, &Value) -> Option<T>) -> Result<Listing<T>, RdError> {
    if v["kind"] != "Listing" {
        return Err(RdError::Parse(format!("expected a Listing, got {}", v["kind"])));
    }
    let d = &v["data"];
    let items = d["children"]
        .as_array()
        .map(|c| c.iter().filter_map(|t| f(t["kind"].as_str().unwrap_or_default(), &t["data"])).collect())
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

/// A subreddit's info page (`/r/x/about`): the fields the lists show, plus what only this page
/// needs. Kept apart from [`Subreddit`], which fills lists of up to a thousand subscriptions and
/// should not carry a sidebar each.
#[derive(Debug, Clone, PartialEq)]
pub struct SubInfo {
    pub sub: Subreddit,
    /// The old-style sidebar, markdown.
    pub sidebar: String,
    pub created_utc: i64,
    /// People online now (0 when Reddit leaves it out).
    pub active: i64,
    /// The signed-in user joined it.
    pub subscribed: bool,
}

/// Parses `{"kind":"t5","data":{...}}`. Anything else (Reddit answers a missing subreddit with a
/// search listing) is an error.
pub fn parse_sub_info(v: &Value) -> Result<SubInfo, RdError> {
    if v["kind"] != "t5" {
        return Err(RdError::Parse(format!("expected a subreddit, got {}", v["kind"])));
    }
    let d = &v["data"];
    Ok(SubInfo {
        sub: Subreddit::from_json(d),
        sidebar: s(d, "description"),
        created_utc: i(d, "created_utc"),
        active: d["active_user_count"].as_i64().or_else(|| d["accounts_active"].as_i64()).unwrap_or(0),
        subscribed: b(d, "user_is_subscriber"),
    })
}

/// One of a subreddit's rules.
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub short_name: String,
    /// Markdown; often empty.
    pub description: String,
    /// What it applies to: `all`, `link` (posts) or `comment`.
    pub kind: String,
}

/// Parses `/r/x/about/rules` (`{"rules":[...],"site_rules":[...]}`, not a Listing), in Reddit's
/// order. A reply without rules gives none.
pub fn parse_rules(v: &Value) -> Vec<Rule> {
    let mut rules: Vec<(i64, Rule)> = v["rules"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|r| (i(r, "priority"), Rule { short_name: s(r, "short_name"), description: s(r, "description"), kind: s(r, "kind") }))
                .filter(|(_, r)| !r.short_name.trim().is_empty())
                .collect()
        })
        .unwrap_or_default();
    rules.sort_by_key(|(p, _)| *p);
    rules.into_iter().map(|(_, r)| r).collect()
}

/// A user's profile (`/user/x/about`).
#[derive(Debug, Clone, PartialEq)]
pub struct UserInfo {
    pub name: String,
    pub created_utc: i64,
    /// Post karma.
    pub link_karma: i64,
    pub comment_karma: i64,
    pub icon: Option<String>,
    /// The profile's own description.
    pub bio: String,
    /// Reddit suspended the account: only the name comes back.
    pub suspended: bool,
}

/// Parses `{"kind":"t2","data":{...}}`.
pub fn parse_user(v: &Value) -> Result<UserInfo, RdError> {
    if v["kind"] != "t2" {
        return Err(RdError::Parse(format!("expected a user, got {}", v["kind"])));
    }
    let d = &v["data"];
    let profile = &d["subreddit"];
    Ok(UserInfo {
        name: s(d, "name"),
        created_utc: i(d, "created_utc"),
        link_karma: i(d, "link_karma"),
        comment_karma: i(d, "comment_karma"),
        icon: opt_s(d, "icon_img").or_else(|| opt_s(profile, "icon_img")),
        bio: s(profile, "public_description"),
        suspended: b(d, "is_suspended"),
    })
}

/// A custom feed (multireddit): several subreddits under one name, owned by a user.
#[derive(Debug, Clone, PartialEq)]
pub struct Multi {
    /// The name in its address (`/user/{owner}/m/{name}`).
    pub name: String,
    /// The name the owner gave it (the address name when they gave none).
    pub display_name: String,
    pub owner: String,
    /// `/user/{owner}/m/{name}/`.
    pub path: String,
    pub subs: Vec<String>,
    pub icon: Option<String>,
    /// Markdown.
    pub description: String,
    pub over_18: bool,
}

impl Multi {
    pub fn from_json(d: &Value) -> Multi {
        let path = s(d, "path");
        // The owner is in the address too, for replies that leave the field out.
        let from_path = path.split('/').filter(|p| !p.is_empty()).collect::<Vec<_>>();
        let owner = opt_s(d, "owner").or_else(|| match from_path.as_slice() {
            ["user" | "u", owner, "m", ..] => Some(owner.to_string()),
            _ => None,
        });
        let name = s(d, "name");
        Multi {
            display_name: opt_s(d, "display_name").unwrap_or_else(|| name.clone()),
            name,
            owner: owner.unwrap_or_default(),
            path,
            subs: d["subreddits"].as_array().map(|a| a.iter().filter_map(|x| x["name"].as_str().map(str::to_string)).collect()).unwrap_or_default(),
            icon: opt_s(d, "icon_url"),
            description: s(d, "description_md"),
            over_18: b(d, "over_18"),
        }
    }

    /// The feed of its posts.
    pub fn feed(&self) -> crate::listing::Feed {
        crate::listing::Feed::Custom { owner: self.owner.clone(), name: self.name.clone() }
    }
}

/// Parses `/api/multi/mine`: a JSON array of `{"kind":"LabeledMulti","data":{...}}`, not a
/// Listing.
pub fn parse_multis(v: &Value) -> Result<Vec<Multi>, RdError> {
    let a = v.as_array().ok_or_else(|| RdError::Parse("custom feeds: expected an array".into()))?;
    Ok(a.iter().filter(|t| t["kind"] == "LabeledMulti").map(|t| Multi::from_json(&t["data"])).filter(|m| !m.name.is_empty() && !m.owner.is_empty()).collect())
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

    #[test]
    fn parses_custom_feeds() {
        let m = parse_multis(&fixture("multi_mine.json")).unwrap();
        assert_eq!(m.len(), 2, "other kinds are skipped");
        let tech = &m[0];
        assert_eq!((tech.name.as_str(), tech.display_name.as_str(), tech.owner.as_str()), ("tech", "Tech news", "alice"));
        assert_eq!(tech.subs, ["rust", "programming", "linux"]);
        assert_eq!(tech.icon.as_deref(), Some("https://www.redditstatic.com/custom_feed_icons/abc.png"));
        assert_eq!(tech.feed(), crate::listing::Feed::Custom { owner: "alice".into(), name: "tech".into() });
        // No display name: the address name; no owner field: the owner from the address.
        let pics = &m[1];
        assert_eq!((pics.display_name.as_str(), pics.owner.as_str(), pics.icon.as_deref()), ("pics_feed", "bob", None));
        assert!(pics.over_18 && pics.subs.is_empty());
        assert!(parse_multis(&serde_json::json!({"kind": "Listing"})).is_err());
    }

    #[test]
    fn parses_a_subreddits_about_and_rules() {
        let about = serde_json::json!({"kind":"t5","data":{"display_name":"rust","title":"The Rust language","subscribers":350000,
            "active_user_count":1200,"created_utc":1291139521.0,"over18":false,"user_is_subscriber":true,
            "public_description":"A place for all things Rust","description":"# Rules\n\n* Be civil",
            "community_icon":"https://styles.redditmedia.com/t5_2s7lj/icon.png?width=256&s=x"}});
        let a = parse_sub_info(&about).unwrap();
        assert_eq!((a.sub.display_name.as_str(), a.sub.subscribers, a.active), ("rust", 350000, 1200));
        assert_eq!((a.created_utc, a.subscribed), (1291139521, true));
        assert_eq!(a.sidebar, "# Rules\n\n* Be civil");
        assert!(a.sub.icon.as_deref().unwrap().contains("&s=x"));
        // Reddit answers a subreddit that does not exist with search results.
        assert!(parse_sub_info(&serde_json::json!({"kind":"Listing","data":{"children":[]}})).is_err());

        let rules = serde_json::json!({"rules":[
            {"kind":"comment","short_name":"Be civil","description":"No **insults**.","priority":1},
            {"kind":"all","short_name":"On topic","description":"","priority":0},
            {"kind":"link","short_name":"  ","priority":2}],"site_rules":["Spam"]});
        let r = parse_rules(&rules);
        assert_eq!(r.iter().map(|r| r.short_name.as_str()).collect::<Vec<_>>(), ["On topic", "Be civil"]);
        assert_eq!((r[1].kind.as_str(), r[1].description.as_str()), ("comment", "No **insults**."));
        assert!(parse_rules(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn parses_a_users_profile() {
        let u = serde_json::json!({"kind":"t2","data":{"name":"alice","created_utc":1400000000.0,"link_karma":1520,"comment_karma":30411,
            "total_karma":31931,"icon_img":"https://styles.redditmedia.com/t5_x/styles/profileIcon_y.png?width=256&s=z",
            "subreddit":{"public_description":"Hello there","icon_img":"https://example.com/other.png"}}});
        let p = parse_user(&u).unwrap();
        assert_eq!((p.name.as_str(), p.created_utc, p.link_karma, p.comment_karma), ("alice", 1400000000, 1520, 30411));
        assert!(p.icon.as_deref().unwrap().contains("profileIcon_y"));
        assert_eq!((p.bio.as_str(), p.suspended), ("Hello there", false));
        let suspended = parse_user(&serde_json::json!({"kind":"t2","data":{"name":"gone","is_suspended":true}})).unwrap();
        assert!(suspended.suspended && suspended.icon.is_none() && suspended.created_utc == 0);
        assert!(parse_user(&serde_json::json!({"kind":"t5","data":{}})).is_err());
    }
}
