//! Posts the user opened before: when, and how many comments they had then. Comments written
//! since show as new when the post is opened again, and feeds show how many were added ("+12").
//! Kept in the data folder as `seen.txt`, one post per line (`id visit_secs comments`), oldest
//! first, the most recent [`SEEN_MAX`].

use std::collections::{HashMap, VecDeque};

/// The last visit to a post.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Visit {
    /// Unix seconds: the comment list as loaded then.
    pub at: i64,
    /// The post's comment count then.
    pub comments: i64,
}

#[derive(Default)]
pub struct SeenPosts {
    map: HashMap<String, Visit>,
    /// Oldest visit first.
    order: VecDeque<String>,
}

const SEEN_MAX: usize = 2000;

impl SeenPosts {
    pub fn get(&self, id: &str) -> Option<Visit> {
        self.map.get(id).copied()
    }

    /// Records a visit (it becomes the newest; the oldest beyond the cap are forgotten).
    pub fn visit(&mut self, id: &str, v: Visit) {
        if self.map.insert(id.to_string(), v).is_some() {
            self.order.retain(|o| o != id);
        }
        self.order.push_back(id.to_string());
        while self.order.len() > SEEN_MAX {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
    }

    /// Comments added to post `id` since the last visit (0 when it was never opened).
    pub fn grown(&self, id: &str, comments_now: i64) -> i64 {
        self.map.get(id).map_or(0, |v| (comments_now - v.comments).max(0))
    }

    pub fn from_text(s: &str) -> SeenPosts {
        let mut seen = SeenPosts::default();
        for line in s.lines() {
            let mut f = line.split_whitespace();
            if let (Some(id), Some(Ok(at)), Some(Ok(comments))) = (f.next(), f.next().map(str::parse), f.next().map(str::parse)) {
                seen.visit(id, Visit { at, comments });
            }
        }
        seen
    }

    pub fn to_text(&self) -> String {
        self.order.iter().filter_map(|id| self.map.get(id).map(|v| format!("{id} {} {}", v.at, v.comments))).collect::<Vec<_>>().join("\n")
    }
}

fn path() -> std::path::PathBuf {
    crate::store::data_file("seen.txt")
}

pub fn load() -> SeenPosts {
    SeenPosts::from_text(&std::fs::read_to_string(path()).unwrap_or_default())
}

pub fn save(s: &SeenPosts) {
    if let Err(e) = crate::store::write_atomic(&path(), s.to_text().as_bytes(), false) {
        crate::log(&format!("seen: save failed: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visits_round_trip_and_stay_capped() {
        let mut s = SeenPosts::default();
        s.visit("a", Visit { at: 100, comments: 5 });
        s.visit("b", Visit { at: 200, comments: 9 });
        s.visit("a", Visit { at: 300, comments: 7 });
        assert_eq!(s.to_text(), "b 200 9\na 300 7");
        let back = SeenPosts::from_text(&s.to_text());
        assert_eq!(back.get("a"), Some(Visit { at: 300, comments: 7 }));
        assert_eq!((back.grown("a", 12), back.grown("a", 3), back.grown("zz", 50)), (5, 0, 0));
        for i in 0..SEEN_MAX + 5 {
            s.visit(&format!("p{i}"), Visit { at: i as i64, comments: 0 });
        }
        assert_eq!(s.order.len(), SEEN_MAX);
        assert!(s.get("a").is_none() && s.get(&format!("p{}", SEEN_MAX + 4)).is_some());
        assert!(SeenPosts::from_text("broken line\nx 1 2").get("x").is_some());
    }
}
