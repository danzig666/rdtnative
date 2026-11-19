//! The user's settings: one table of options (key, label, help, values) that both the saved file
//! and the Settings screens are built from, plus the lists the app keeps (favourite subreddits,
//! search history, filters, posts already read). Saved as JSON in the app's data folder.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet, VecDeque};

/// One option: its key, label, help line and values as (stored value, label). The first value is
/// the default unless `default` names another.
pub struct Opt {
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub values: &'static [(&'static str, &'static str)],
    pub default: &'static str,
}

const ON_OFF: &[(&str, &str)] = &[("on", "On"), ("off", "Off")];
const OFF_ON: &[(&str, &str)] = &[("off", "Off"), ("on", "On")];

pub const OPTS: &[Opt] = &[
    // Display
    Opt {
        key: "text_scale",
        label: "Text size",
        help: "Size of all text. Bigger for a small or far-away TV, smaller to see more at once.",
        values: &[
            ("70", "70% \u{b7} tiny"),
            ("80", "80% \u{b7} small"),
            ("90", "90%"),
            ("100", "100% \u{b7} standard"),
            ("110", "110%"),
            ("120", "120% \u{b7} large"),
            ("135", "135%"),
            ("150", "150% \u{b7} extra large"),
            ("175", "175% \u{b7} huge"),
        ],
        default: "100",
    },
    Opt {
        key: "content_scale",
        label: "Post and comment text",
        help: "Size of post bodies and comments relative to the interface text.",
        values: &[("80", "80%"), ("90", "90%"), ("100", "100%"), ("110", "110%"), ("120", "120%"), ("135", "135%"), ("150", "150%")],
        default: "100",
    },
    Opt {
        key: "font",
        label: "Font",
        help: "The typeface of the whole app.",
        values: &[("inter", "Inter (default)"), ("sans", "DejaVu Sans"), ("roboto", "Roboto"), ("serif", "Serif"), ("mono", "Monospace")],
        default: "inter",
    },
    Opt {
        key: "theme",
        label: "Theme",
        help: "Background colour. Black is best on OLED TVs.",
        values: &[("dark", "Dark"), ("black", "Black (OLED)"), ("midnight", "Midnight blue"), ("slate", "Slate"), ("warm", "Warm dark")],
        default: "dark",
    },
    Opt {
        key: "accent",
        label: "Accent colour",
        help: "Colour of focus marks, links and buttons.",
        values: &[
            ("orange", "Reddit orange"),
            ("blue", "Blue"),
            ("green", "Green"),
            ("purple", "Purple"),
            ("pink", "Pink"),
            ("teal", "Teal"),
            ("red", "Red"),
            ("yellow", "Yellow"),
        ],
        default: "orange",
    },
    Opt {
        key: "layout",
        label: "Feed layout",
        help: "Two panes: a list with a preview of the selected post beside it. Cards: a grid of pictures. List: rows with a thumbnail. Large: one big post at a time. Compact: two lines per post. Dense: one line per post, the most at once.",
        values: &[("split", "Two panes"), ("cards", "Cards"), ("list", "List"), ("large", "Large"), ("compact", "Compact"), ("dense", "Dense")],
        default: "split",
    },
    Opt {
        key: "columns",
        label: "Cards per row",
        help: "How many cards fit side by side in the Cards layout.",
        values: &[("2", "2"), ("3", "3"), ("4", "4"), ("5", "5"), ("6", "6")],
        default: "4",
    },
    Opt {
        key: "title_lines",
        label: "Title lines",
        help: "How many lines of a post's title the feed shows.",
        values: &[("1", "1"), ("2", "2"), ("3", "3"), ("4", "4")],
        default: "2",
    },
    Opt { key: "preview_text", label: "Text preview in the feed", help: "Show the start of text posts in the feed.", values: ON_OFF, default: "on" },
    Opt { key: "show_thumbs", label: "Pictures in the feed", help: "Off shows titles only, for slow networks.", values: ON_OFF, default: "on" },
    Opt { key: "clock", label: "Clock", help: "Show the time in the corner of the feed.", values: OFF_ON, default: "off" },
    Opt {
        key: "image_quality",
        label: "Picture quality",
        help: "Higher looks sharper on big TVs and downloads more.",
        values: &[("low", "Low"), ("medium", "Medium"), ("high", "High"), ("max", "Original")],
        default: "high",
    },
    // Content
    Opt {
        key: "nsfw",
        label: "NSFW posts",
        help: "Adult posts: hide them, show them blurred until opened, or show them.",
        values: &[("blur", "Blur"), ("hide", "Hide"), ("show", "Show")],
        default: "blur",
    },
    Opt { key: "spoilers", label: "Spoilers", help: "Blur pictures of posts marked as spoilers.", values: &[("blur", "Blur"), ("show", "Show")], default: "blur" },
    Opt { key: "dim_read", label: "Dim posts already opened", help: "Posts you opened before are drawn fainter.", values: ON_OFF, default: "on" },
    Opt { key: "hide_read", label: "Hide posts already opened", help: "Leave out posts you opened before.", values: OFF_ON, default: "off" },
    Opt { key: "show_flair", label: "Flairs", help: "Show post and user flairs.", values: ON_OFF, default: "on" },
    Opt { key: "show_scores", label: "Scores", help: "Show vote scores of posts and comments.", values: ON_OFF, default: "on" },
    Opt { key: "show_domain", label: "Link sites", help: "Show which site a link post points to.", values: ON_OFF, default: "on" },
    Opt {
        key: "video_stream",
        label: "Video stream",
        help: "How Reddit videos are played. HLS and DASH have sound; MP4 is the simplest and has no sound. Try another if a video does not play.",
        values: &[("hls", "HLS (default)"), ("dash", "DASH"), ("mp4", "MP4 (no sound)")],
        default: "hls",
    },
    Opt { key: "hide_stickied", label: "Hide pinned posts", help: "Leave out the moderators' pinned posts at the top of subreddits.", values: OFF_ON, default: "off" },
    // Browsing
    Opt {
        key: "start_page",
        label: "Start page",
        help: "The feed shown when the app starts.",
        values: &[("front", "Home"), ("popular", "Popular"), ("all", "All"), ("favorites", "Favourites")],
        default: "front",
    },
    Opt {
        key: "default_sort",
        label: "Default sort",
        help: "How feeds are sorted when opened.",
        values: &[("hot", "Hot"), ("best", "Best"), ("new", "New"), ("top", "Top"), ("rising", "Rising")],
        default: "hot",
    },
    Opt {
        key: "default_time",
        label: "Default time range",
        help: "For Top and Controversial.",
        values: &[("day", "Today"), ("hour", "Past hour"), ("week", "This week"), ("month", "This month"), ("year", "This year"), ("all", "All time")],
        default: "day",
    },
    Opt { key: "remember_sort", label: "Remember sort per subreddit", help: "Each subreddit keeps the sort you chose last.", values: ON_OFF, default: "on" },
    Opt {
        key: "comment_sort",
        label: "Comment sort",
        help: "How comments are ordered.",
        values: &[("confidence", "Best"), ("top", "Top"), ("new", "New"), ("controversial", "Controversial"), ("old", "Old"), ("qa", "Q&A")],
        default: "confidence",
    },
    Opt {
        key: "collapse_depth",
        label: "Collapse deep replies",
        help: "Replies deeper than this start collapsed (OK on a comment opens or closes it).",
        values: &[("0", "Never"), ("2", "Below level 2"), ("3", "Below level 3"), ("5", "Below level 5")],
        default: "0",
    },
    Opt { key: "collapse_mod", label: "Collapse moderator notes", help: "Pinned moderator and AutoModerator comments start collapsed.", values: ON_OFF, default: "on" },
    // Navigation
    Opt {
        key: "edge_scroll",
        label: "Pointer edge scrolling",
        help: "Rest the Magic Remote pointer at the top or bottom edge to scroll.",
        values: &[("normal", "Normal"), ("slow", "Slow"), ("fast", "Fast"), ("off", "Off")],
        default: "normal",
    },
    Opt {
        key: "wheel_rows",
        label: "Wheel step",
        help: "How far one notch of the Magic Remote wheel moves.",
        values: &[("1", "One item"), ("2", "Two items"), ("3", "Three items")],
        default: "1",
    },
    Opt { key: "smooth_scroll", label: "Smooth scrolling", help: "Glide to the next item instead of jumping.", values: ON_OFF, default: "on" },
    Opt { key: "left_sidebar", label: "Left opens the menu", help: "Left from the first column opens the side menu (BACK always does).", values: ON_OFF, default: "on" },
    Opt { key: "back_exit", label: "Leaving the app", help: "BACK on the side menu leaves the app.", values: &[("twice", "Press BACK twice"), ("once", "Press BACK once")], default: "twice" },
    Opt { key: "color_keys", label: "Colour buttons", help: "Red downvotes, green upvotes, yellow saves, blue opens the comments.", values: ON_OFF, default: "on" },
];

pub fn opt(key: &str) -> Option<&'static Opt> {
    OPTS.iter().find(|o| o.key == key)
}

/// A settings page: its title and the option keys on it.
pub struct Page {
    pub title: &'static str,
    pub icon: crate::ui::icon::Kind,
    pub keys: &'static [&'static str],
}

pub fn pages() -> [Page; 4] {
    use crate::ui::icon::Kind as K;
    [
        Page {
            title: "Display",
            icon: K::Film,
            keys: &["text_scale", "content_scale", "font", "theme", "accent", "layout", "columns", "title_lines", "preview_text", "show_thumbs", "image_quality", "clock"],
        },
        Page {
            title: "Content",
            icon: K::Shield,
            keys: &["nsfw", "spoilers", "dim_read", "hide_read", "hide_stickied", "show_flair", "show_scores", "show_domain", "video_stream"],
        },
        Page {
            title: "Feeds and comments",
            icon: K::List,
            keys: &["start_page", "default_sort", "default_time", "remember_sort", "comment_sort", "collapse_depth", "collapse_mod"],
        },
        Page { title: "Remote and navigation", icon: K::Gauge, keys: &["edge_scroll", "wheel_rows", "smooth_scroll", "left_sidebar", "back_exit", "color_keys"] },
    ]
}

/// The Imgur client ID used until the user enters their own.
pub const DEFAULT_IMGUR_ID: &str = "981c2f80e996ca3";

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct Prefs {
    #[serde(default)]
    values: BTreeMap<String, String>,
    /// Favourite subreddits, in the side menu (kept on the TV; works without an account).
    #[serde(default)]
    pub favorites: Vec<String>,
    #[serde(default)]
    pub history: Vec<String>,
    /// Subreddits whose posts are left out of every feed.
    #[serde(default)]
    pub filter_subs: Vec<String>,
    /// Posts whose title has one of these words are left out.
    #[serde(default)]
    pub filter_words: Vec<String>,
    /// The sort each subreddit was last shown with: "sort" or "sort:time".
    #[serde(default)]
    pub sub_sorts: BTreeMap<String, String>,
    #[serde(default)]
    pub imgur_client_id: Option<String>,
    /// The user confirmed being an adult (asked the first time NSFW is set to Show).
    #[serde(default)]
    pub adult_ok: bool,
}

impl Prefs {
    /// The value of `key`, or its default.
    pub fn get(&self, key: &str) -> &str {
        if let Some(v) = self.values.get(key) {
            return v;
        }
        opt(key).map_or("", |o| o.default)
    }
    pub fn on(&self, key: &str) -> bool {
        self.get(key) == "on"
    }
    pub fn num(&self, key: &str) -> i64 {
        self.get(key).parse().unwrap_or(0)
    }
    pub fn set(&mut self, key: &str, value: &str) {
        if opt(key).is_some_and(|o| o.default == value) {
            self.values.remove(key);
        } else {
            self.values.insert(key.to_string(), value.to_string());
        }
    }
    /// Label of the current value of `key`.
    pub fn label(&self, key: &str) -> &'static str {
        let v = self.get(key);
        opt(key).and_then(|o| o.values.iter().find(|(k, _)| *k == v)).map_or("", |(_, l)| *l)
    }
    pub fn imgur_id(&self) -> &str {
        self.imgur_client_id.as_deref().filter(|s| !s.is_empty()).unwrap_or(DEFAULT_IMGUR_ID)
    }
    pub fn is_favorite(&self, sub: &str) -> bool {
        self.favorites.iter().any(|f| f.eq_ignore_ascii_case(sub))
    }
    pub fn toggle_favorite(&mut self, sub: &str) -> bool {
        if let Some(i) = self.favorites.iter().position(|f| f.eq_ignore_ascii_case(sub)) {
            self.favorites.remove(i);
            false
        } else {
            self.favorites.push(sub.to_string());
            self.favorites.sort_by_key(|s| s.to_ascii_lowercase());
            true
        }
    }
    pub fn remember_search(&mut self, q: &str) {
        let q = q.trim();
        if q.is_empty() {
            return;
        }
        self.history.retain(|h| !h.eq_ignore_ascii_case(q));
        self.history.insert(0, q.to_string());
        self.history.truncate(30);
    }
    /// The picture width to ask for, for a tile `w` px wide on screen.
    pub fn pic_width(&self, w: f32) -> u32 {
        let k = match self.get("image_quality") {
            "low" => 0.6,
            "medium" => 0.85,
            "max" => 2.0,
            _ => 1.15,
        };
        (w * k) as u32
    }
    /// Whether a post passes the user's filters.
    pub fn keeps(&self, sub: &str, title: &str, nsfw: bool, stickied: bool, read: bool) -> bool {
        if nsfw && self.get("nsfw") == "hide" {
            return false;
        }
        if stickied && self.on("hide_stickied") {
            return false;
        }
        if read && self.on("hide_read") {
            return false;
        }
        if self.filter_subs.iter().any(|f| f.eq_ignore_ascii_case(sub)) {
            return false;
        }
        let t = title.to_lowercase();
        !self.filter_words.iter().any(|w| !w.is_empty() && t.contains(&w.to_lowercase()))
    }

    /// Applies the display options to the drawing code.
    pub fn apply(&self) {
        crate::ui::set_text_scale(self.num("text_scale").max(60) as u16);
        crate::ui::set_content_scale(self.num("content_scale").max(60) as u16);
        crate::ui::set_font(self.get("font"));
        crate::ui::set_theme(self.get("theme"));
        crate::ui::set_accent(self.get("accent"));
    }
}

/// Posts opened before (for dimming and hiding), newest last; kept to a few thousand.
#[derive(Default)]
pub struct ReadPosts {
    set: HashSet<String>,
    order: VecDeque<String>,
}

const READ_MAX: usize = 5000;

impl ReadPosts {
    pub fn contains(&self, id: &str) -> bool {
        self.set.contains(id)
    }
    pub fn add(&mut self, id: &str) -> bool {
        if !self.set.insert(id.to_string()) {
            return false;
        }
        self.order.push_back(id.to_string());
        while self.order.len() > READ_MAX {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        true
    }
    pub fn from_text(s: &str) -> ReadPosts {
        let mut r = ReadPosts::default();
        for id in s.split_whitespace() {
            r.add(id);
        }
        r
    }
    pub fn to_text(&self) -> String {
        self.order.iter().cloned().collect::<Vec<_>>().join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_round_trip() {
        let mut p = Prefs::default();
        assert_eq!(p.get("layout"), "split");
        assert_eq!(p.label("nsfw"), "Blur");
        p.set("layout", "list");
        p.set("nsfw", "blur");
        assert_eq!(p.get("layout"), "list");
        assert!(!p.values.contains_key("nsfw"), "defaults are not stored");
        let back: Prefs = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back.get("layout"), "list");
        assert_eq!(p.imgur_id(), DEFAULT_IMGUR_ID);
        for o in OPTS {
            assert!(o.values.iter().any(|(v, _)| *v == o.default), "{} default is one of its values", o.key);
        }
        for page in pages() {
            for k in page.keys {
                assert!(opt(k).is_some(), "{k} is an option");
            }
        }
    }

    #[test]
    fn favorites_history_filters_and_read() {
        let mut p = Prefs::default();
        assert!(p.toggle_favorite("Pics"));
        assert!(p.is_favorite("pics"));
        assert!(!p.toggle_favorite("PICS"));
        p.remember_search("cats");
        p.remember_search("dogs");
        p.remember_search("Cats");
        assert_eq!(p.history, ["Cats", "dogs"]);
        p.filter_words.push("spoiler".into());
        assert!(!p.keeps("a", "Big SPOILER here", false, false, false));
        assert!(p.keeps("a", "fine", false, false, false));
        p.set("nsfw", "hide");
        assert!(!p.keeps("a", "x", true, false, false));
        let mut r = ReadPosts::default();
        assert!(r.add("a") && !r.add("a"));
        assert!(ReadPosts::from_text(&r.to_text()).contains("a"));
    }
}
