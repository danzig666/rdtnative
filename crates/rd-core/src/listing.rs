//! Where a feed comes from (front page, popular, all, a subreddit, a user, search, saved) and how
//! it is sorted: the API path and query for each page, plus the requests behind the signed-in
//! actions (vote, save, hide, subscribe, reply) and a few small helpers the screens share.

/// Listing sorts. `Best` is the signed-in front page's default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sort {
    Best,
    Hot,
    New,
    Top,
    Rising,
    Controversial,
}

impl Sort {
    pub const ALL: [Sort; 6] = [Sort::Best, Sort::Hot, Sort::New, Sort::Top, Sort::Rising, Sort::Controversial];

    pub fn key(self) -> &'static str {
        match self {
            Sort::Best => "best",
            Sort::Hot => "hot",
            Sort::New => "new",
            Sort::Top => "top",
            Sort::Rising => "rising",
            Sort::Controversial => "controversial",
        }
    }
    pub fn from_key(k: &str) -> Option<Sort> {
        Sort::ALL.into_iter().find(|s| s.key() == k)
    }
    /// The sort's name in `feed` (search calls Hot "Relevance").
    pub fn label_in(self, feed: &Feed) -> &'static str {
        match (self, feed) {
            (Sort::Hot, Feed::Search { .. }) => "Relevance",
            _ => self.label(),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Sort::Best => "Best",
            Sort::Hot => "Hot",
            Sort::New => "New",
            Sort::Top => "Top",
            Sort::Rising => "Rising",
            Sort::Controversial => "Controversial",
        }
    }
    /// Top and Controversial take a time range.
    pub fn timed(self) -> bool {
        matches!(self, Sort::Top | Sort::Controversial)
    }
}

/// Time ranges for Top / Controversial (and search).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Time {
    Hour,
    Day,
    Week,
    Month,
    Year,
    All,
}

impl Time {
    pub const ALL: [Time; 6] = [Time::Hour, Time::Day, Time::Week, Time::Month, Time::Year, Time::All];
    pub fn key(self) -> &'static str {
        match self {
            Time::Hour => "hour",
            Time::Day => "day",
            Time::Week => "week",
            Time::Month => "month",
            Time::Year => "year",
            Time::All => "all",
        }
    }
    pub fn from_key(k: &str) -> Option<Time> {
        Time::ALL.into_iter().find(|s| s.key() == k)
    }
    pub fn label(self) -> &'static str {
        match self {
            Time::Hour => "Past hour",
            Time::Day => "Today",
            Time::Week => "This week",
            Time::Month => "This month",
            Time::Year => "This year",
            Time::All => "All time",
        }
    }
}

/// What a user page lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UserTab {
    Submitted,
    Comments,
    Saved,
    Upvoted,
    Hidden,
}

impl UserTab {
    pub fn key(self) -> &'static str {
        match self {
            UserTab::Submitted => "submitted",
            UserTab::Comments => "comments",
            UserTab::Saved => "saved",
            UserTab::Upvoted => "upvoted",
            UserTab::Hidden => "hidden",
        }
    }
}

/// A feed's source.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Feed {
    /// The front page (subscriptions when signed in, Reddit's default set otherwise).
    Front,
    Popular,
    All,
    Sub(String),
    /// Several subreddits at once ("a+b+c").
    Multi(Vec<String>),
    User(String, UserTab),
    /// Post search, optionally inside one subreddit.
    Search { q: String, sub: Option<String> },
    /// A subreddit's posts by a domain ("youtube.com").
    Domain(String),
}

impl Feed {
    /// The page title.
    pub fn title(&self) -> String {
        match self {
            Feed::Front => "Home".into(),
            Feed::Popular => "Popular".into(),
            Feed::All => "All".into(),
            Feed::Sub(s) => format!("r/{s}"),
            Feed::Multi(subs) => subs.iter().map(|s| format!("r/{s}")).collect::<Vec<_>>().join(" + "),
            Feed::User(u, t) => match t {
                UserTab::Submitted => format!("u/{u}"),
                UserTab::Comments => format!("u/{u} \u{b7} comments"),
                UserTab::Saved => "Saved".into(),
                UserTab::Upvoted => "Upvoted".into(),
                UserTab::Hidden => "Hidden".into(),
            },
            Feed::Search { q, sub: None } => format!("\u{201c}{q}\u{201d}"),
            Feed::Search { q, sub: Some(s) } => format!("\u{201c}{q}\u{201d} in r/{s}"),
            Feed::Domain(d) => d.clone(),
        }
    }

    /// The sort a feed opens with.
    pub fn default_sort(&self, signed_in: bool) -> Sort {
        match self {
            Feed::Front if signed_in => Sort::Best,
            Feed::User(..) => Sort::New,
            _ => Sort::Hot,
        }
    }

    /// The sorts this feed offers.
    pub fn sorts(&self, signed_in: bool) -> Vec<Sort> {
        match self {
            Feed::Front if signed_in => vec![Sort::Best, Sort::Hot, Sort::New, Sort::Top, Sort::Rising, Sort::Controversial],
            Feed::Search { .. } => vec![Sort::Hot, Sort::New, Sort::Top],
            Feed::User(_, UserTab::Submitted | UserTab::Comments) => vec![Sort::Hot, Sort::New, Sort::Top, Sort::Controversial],
            Feed::User(..) => vec![],
            _ => vec![Sort::Hot, Sort::New, Sort::Top, Sort::Rising, Sort::Controversial],
        }
    }

    /// The subreddit a feed belongs to (for search in it, subscribe, sidebar info).
    pub fn sub(&self) -> Option<&str> {
        match self {
            Feed::Sub(s) => Some(s),
            Feed::Search { sub: Some(s), .. } => Some(s),
            _ => None,
        }
    }

    /// (path, query) for one page.
    pub fn request(&self, sort: Sort, time: Time, after: Option<&str>, limit: u32) -> (String, Vec<(String, String)>) {
        let mut q: Vec<(String, String)> = vec![("limit".into(), limit.to_string())];
        let sorted = |base: &str| if base.is_empty() { format!("/{}", sort.key()) } else { format!("{base}/{}", sort.key()) };
        let path = match self {
            Feed::Front => sorted(""),
            Feed::Popular => sorted("/r/popular"),
            Feed::All => sorted("/r/all"),
            Feed::Sub(s) => sorted(&format!("/r/{s}")),
            Feed::Multi(subs) => sorted(&format!("/r/{}", subs.join("+"))),
            Feed::Domain(d) => sorted(&format!("/domain/{d}")),
            Feed::User(u, tab) => {
                if matches!(tab, UserTab::Submitted | UserTab::Comments) {
                    q.push(("sort".into(), sort.key().into()));
                }
                q.push(("type".into(), "links".into()));
                format!("/user/{u}/{}", tab.key())
            }
            Feed::Search { q: text, sub } => {
                q.push(("q".into(), text.clone()));
                q.push(("sort".into(), match sort {
                    Sort::New => "new",
                    Sort::Top => "top",
                    _ => "relevance",
                }
                .into()));
                q.push(("type".into(), "link".into()));
                // NSFW results come too; the app's NSFW setting hides or blurs them.
                q.push(("include_over_18".into(), "on".into()));
                match sub {
                    Some(s) => {
                        q.push(("restrict_sr".into(), "on".into()));
                        format!("/r/{s}/search")
                    }
                    None => "/search".into(),
                }
            }
        };
        if sort.timed() || matches!(self, Feed::Search { .. }) && sort == Sort::Top {
            q.push(("t".into(), time.key().into()));
        }
        if let Some(a) = after {
            q.push(("after".into(), a.into()));
        }
        (path, q)
    }
}

/// Comment sorts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CommentSort {
    Confidence,
    Top,
    New,
    Controversial,
    Old,
    Qa,
}

impl CommentSort {
    pub const ALL: [CommentSort; 6] = [CommentSort::Confidence, CommentSort::Top, CommentSort::New, CommentSort::Controversial, CommentSort::Old, CommentSort::Qa];
    pub fn key(self) -> &'static str {
        match self {
            CommentSort::Confidence => "confidence",
            CommentSort::Top => "top",
            CommentSort::New => "new",
            CommentSort::Controversial => "controversial",
            CommentSort::Old => "old",
            CommentSort::Qa => "qa",
        }
    }
    pub fn from_key(k: &str) -> Option<CommentSort> {
        CommentSort::ALL.into_iter().find(|s| s.key() == k)
    }
    pub fn label(self) -> &'static str {
        match self {
            CommentSort::Confidence => "Best",
            CommentSort::Top => "Top",
            CommentSort::New => "New",
            CommentSort::Controversial => "Controversial",
            CommentSort::Old => "Old",
            CommentSort::Qa => "Q&A",
        }
    }
}

/// `/comments/<id>` with its query.
pub fn thread_request(post_id: &str, sort: CommentSort, focus_comment: Option<&str>) -> (String, Vec<(String, String)>) {
    let path = match focus_comment {
        Some(c) => format!("/comments/{post_id}/_/{c}"),
        None => format!("/comments/{post_id}"),
    };
    let mut q = vec![("sort".into(), sort.key().into()), ("limit".into(), "200".into()), ("depth".into(), "10".into())];
    if focus_comment.is_some() {
        q.push(("context".into(), "3".into()));
    }
    (path, q)
}

/// A vote: 1 up, 0 clear, -1 down.
pub fn vote(fullname: &str, dir: i8) -> (&'static str, Vec<(String, String)>) {
    ("/api/vote", vec![("id".into(), fullname.into()), ("dir".into(), dir.clamp(-1, 1).to_string())])
}

pub fn save(fullname: &str, on: bool) -> (&'static str, Vec<(String, String)>) {
    (if on { "/api/save" } else { "/api/unsave" }, vec![("id".into(), fullname.into())])
}

pub fn hide(fullname: &str, on: bool) -> (&'static str, Vec<(String, String)>) {
    (if on { "/api/hide" } else { "/api/unhide" }, vec![("id".into(), fullname.into())])
}

pub fn subscribe(sub: &str, on: bool) -> (&'static str, Vec<(String, String)>) {
    ("/api/subscribe", vec![("action".into(), if on { "sub" } else { "unsub" }.into()), ("sr_name".into(), sub.into())])
}

pub fn reply(parent_fullname: &str, text: &str) -> (&'static str, Vec<(String, String)>) {
    ("/api/comment", vec![("api_type".into(), "json".into()), ("thing_id".into(), parent_fullname.into()), ("text".into(), text.into())])
}

pub fn mark_read(fullnames: &[String]) -> (&'static str, Vec<(String, String)>) {
    ("/api/read_message", vec![("id".into(), fullnames.join(","))])
}

/// "now", "5m", "3h", "2d", "4mo", "1y": a post's or comment's age.
pub fn age(created_utc: i64, now: i64) -> String {
    let s = (now - created_utc).max(0);
    match s {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m", s / 60),
        3600..=86_399 => format!("{}h", s / 3600),
        86_400..=2_591_999 => format!("{}d", s / 86_400),
        2_592_000..=31_535_999 => format!("{}mo", s / 2_592_000),
        _ => format!("{}y", s / 31_536_000),
    }
}

/// `1234567` → `1.2M`, `15234` → `15k`, `1234` → `1.2k`.
pub fn short_count(n: i64) -> String {
    let a = n.unsigned_abs() as f64;
    let sign = if n < 0 { "-" } else { "" };
    if a >= 1_000_000.0 {
        format!("{sign}{:.1}M", a / 1_000_000.0)
    } else if a >= 10_000.0 {
        format!("{sign}{:.0}k", a / 1000.0)
    } else if a >= 1000.0 {
        format!("{sign}{:.1}k", a / 1000.0)
    } else {
        n.to_string()
    }
}

/// Reads a reddit.com link into something the app can open itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RedditLink {
    Sub(String),
    User(String),
    /// (post id, optional comment id)
    Post(String, Option<String>),
    /// A share link (`/r/x/s/<code>`): Reddit redirects it to the real address (see
    /// [`share_target`]).
    Share(String),
}

/// Where a share link points, from the `Location` of Reddit's redirect (follow at most a few).
pub fn share_target(location: &str) -> Option<RedditLink> {
    parse_reddit_link(location).filter(|l| !matches!(l, RedditLink::Share(_)))
}

pub fn parse_reddit_link(url: &str) -> Option<RedditLink> {
    let unwrapped = crate::media::unwrap_media_url(url);
    let url = unwrapped.as_str();
    let rest = url.split("://").nth(1).unwrap_or(url);
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.to_ascii_lowercase();
    if host == "redd.it" {
        let id = path.split(['/', '?', '#']).next()?;
        return (!id.is_empty()).then(|| RedditLink::Post(id.to_string(), None));
    }
    if !(host.ends_with("reddit.com") || host.is_empty()) {
        return None;
    }
    let seg: Vec<&str> = path.split(['?', '#']).next()?.split('/').filter(|s| !s.is_empty()).collect();
    match seg.as_slice() {
        // Share links: /r/x/s/<code>, /u/x/s/<code>, /s/<code>.
        [.., "s", code] if code.len() >= 5 => Some(RedditLink::Share(url.to_string())),
        ["r" | "u" | "user", _, "comments", id, _, c, ..] => Some(RedditLink::Post(id.to_string(), Some(c.to_string()))),
        ["r" | "u" | "user", _, "comments", id, ..] | ["comments", id, ..] | ["gallery", id, ..] => Some(RedditLink::Post(id.to_string(), None)),
        ["r", sub, ..] => Some(RedditLink::Sub(sub.to_string())),
        ["u" | "user", name, ..] => Some(RedditLink::User(name.to_string())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(v: &[(String, String)], k: &str) -> Option<String> {
        v.iter().find(|(a, _)| a == k).map(|(_, b)| b.clone())
    }

    #[test]
    fn feed_requests() {
        let (p, qs) = Feed::Front.request(Sort::Best, Time::Day, None, 25);
        assert_eq!((p.as_str(), q(&qs, "limit").as_deref(), q(&qs, "t")), ("/best", Some("25"), None));
        let (p, qs) = Feed::Sub("pics".into()).request(Sort::Top, Time::Week, Some("t3_x"), 50);
        assert_eq!(p, "/r/pics/top");
        assert_eq!((q(&qs, "t").as_deref(), q(&qs, "after").as_deref()), (Some("week"), Some("t3_x")));
        let (p, qs) = Feed::Search { q: "rust lang".into(), sub: Some("programming".into()) }.request(Sort::Hot, Time::All, None, 25);
        assert_eq!((p.as_str(), q(&qs, "restrict_sr").as_deref(), q(&qs, "sort").as_deref()), ("/r/programming/search", Some("on"), Some("relevance")));
        let (p, _) = Feed::User("spez".into(), UserTab::Saved).request(Sort::New, Time::All, None, 25);
        assert_eq!(p, "/user/spez/saved");
        assert_eq!(Feed::Multi(vec!["a".into(), "b".into()]).request(Sort::New, Time::All, None, 5).0, "/r/a+b/new");
        assert_eq!(Feed::Front.default_sort(false), Sort::Hot);
        assert_eq!(Feed::Sub("x".into()).title(), "r/x");
    }

    #[test]
    fn helpers() {
        assert_eq!(age(1000, 1030), "now");
        assert_eq!(age(0, 7200), "2h");
        assert_eq!(age(0, 86_400 * 3), "3d");
        assert_eq!(age(0, 86_400 * 400), "1y");
        assert_eq!(short_count(15234), "15k");
        assert_eq!(vote("t3_a", 5).1[1].1, "1");
        assert_eq!(thread_request("abc", CommentSort::Top, None).0, "/comments/abc");
        assert_eq!(parse_reddit_link("https://www.reddit.com/r/rust/comments/abc/title/def/"), Some(RedditLink::Post("abc".into(), Some("def".into()))));
        assert_eq!(parse_reddit_link("https://old.reddit.com/r/rust/"), Some(RedditLink::Sub("rust".into())));
        assert_eq!(parse_reddit_link("https://reddit.com/u/spez"), Some(RedditLink::User("spez".into())));
        assert_eq!(parse_reddit_link("https://redd.it/xyz"), Some(RedditLink::Post("xyz".into(), None)));
        assert_eq!(parse_reddit_link("https://example.com/r/x"), None);
        assert_eq!(parse_reddit_link("https://www.reddit.com/r/pics/s/AbCdE12345"), Some(RedditLink::Share("https://www.reddit.com/r/pics/s/AbCdE12345".into())));
        assert_eq!(parse_reddit_link("https://www.reddit.com/user/bob/comments/xyz/title/"), Some(RedditLink::Post("xyz".into(), None)));
        assert_eq!(parse_reddit_link("https://www.reddit.com/gallery/g1"), Some(RedditLink::Post("g1".into(), None)));
        assert_eq!(parse_reddit_link("https://www.reddit.com/media?url=https%3A%2F%2Fwww.reddit.com%2Fr%2Frust%2F"), Some(RedditLink::Sub("rust".into())));
        assert_eq!(share_target("https://www.reddit.com/r/pics/comments/abc/t/?share_id=x"), Some(RedditLink::Post("abc".into(), None)));
    }
}
