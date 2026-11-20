//! Comment trees: parse `/comments/<id>`, merge `/api/morechildren` results in place, and flatten
//! the tree into display rows that honour collapsed threads (the list the TV's comment pane draws).

use crate::model::{b, i, opt_s, s, Post};
use crate::RdError;
use serde_json::Value;
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq)]
pub struct Comment {
    pub id: String,
    /// `t1_<id>`.
    pub name: String,
    /// `t3_…` for top-level comments, `t1_…` for replies.
    pub parent_id: String,
    pub author: String,
    /// Markdown.
    pub body: String,
    pub score: i64,
    pub score_hidden: bool,
    pub created_utc: i64,
    pub depth: u32,
    pub is_submitter: bool,
    pub distinguished: Option<String>,
    pub stickied: bool,
    /// Reddit's own default (e.g. below the score threshold).
    pub collapsed: bool,
    pub likes: Option<bool>,
    /// In the signed-in user's saved items.
    pub saved: bool,
    pub author_flair_text: Option<String>,
    pub replies: Vec<Node>,
}

/// "load more comments (N)" — or "continue this thread" when `children` is empty and `count` is 0.
#[derive(Debug, Clone, PartialEq)]
pub struct More {
    pub id: String,
    pub parent_id: String,
    pub count: i64,
    pub depth: u32,
    pub children: Vec<String>,
}

impl More {
    /// Reddit's "continue this thread" link: no IDs to fetch here, open the parent as a new page.
    pub fn is_continue_thread(&self) -> bool {
        self.children.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Comment(Comment),
    More(More),
}

impl Node {
    pub fn name(&self) -> String {
        match self {
            Node::Comment(c) => c.name.clone(),
            Node::More(m) => format!("more_{}", m.id),
        }
    }
    fn parent_id(&self) -> &str {
        match self {
            Node::Comment(c) => &c.parent_id,
            Node::More(m) => &m.parent_id,
        }
    }
}

fn parse_thing(t: &Value) -> Option<Node> {
    let d = &t["data"];
    match t["kind"].as_str()? {
        "t1" => Some(Node::Comment(Comment {
            id: s(d, "id"),
            name: s(d, "name"),
            parent_id: s(d, "parent_id"),
            author: s(d, "author"),
            body: crate::media::resolve_embeds(&s(d, "body"), &d["media_metadata"]),
            score: i(d, "score"),
            score_hidden: b(d, "score_hidden"),
            created_utc: i(d, "created_utc"),
            depth: i(d, "depth").max(0) as u32,
            is_submitter: b(d, "is_submitter"),
            distinguished: opt_s(d, "distinguished"),
            stickied: b(d, "stickied"),
            collapsed: b(d, "collapsed"),
            likes: d["likes"].as_bool(),
            saved: b(d, "saved"),
            author_flair_text: crate::model::flair(d, "author_flair_richtext", "author_flair_text"),
            // `replies` is "" when empty, otherwise a Listing.
            replies: d["replies"]["data"]["children"]
                .as_array()
                .map(|c| c.iter().filter_map(parse_thing).collect())
                .unwrap_or_default(),
        })),
        "more" => Some(Node::More(More {
            id: s(d, "id"),
            parent_id: s(d, "parent_id"),
            count: i(d, "count"),
            depth: i(d, "depth").max(0) as u32,
            children: d["children"]
                .as_array()
                .map(|c| c.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                .unwrap_or_default(),
        })),
        _ => None,
    }
}

/// A post with its comment tree.
#[derive(Debug, Clone, PartialEq)]
pub struct Thread {
    pub post: Post,
    pub comments: Vec<Node>,
}

/// Parses the two-element array `/comments/<id>` returns: `[post listing, comment listing]`.
pub fn parse_thread(v: &Value) -> Result<Thread, RdError> {
    let arr = v.as_array().filter(|a| a.len() == 2).ok_or_else(|| RdError::Parse("thread is not [post, comments]".into()))?;
    let post = crate::model::parse_posts(&arr[0])?
        .items
        .into_iter()
        .next()
        .ok_or_else(|| RdError::Parse("thread has no post".into()))?;
    let comments = arr[1]["data"]["children"]
        .as_array()
        .map(|c| c.iter().filter_map(parse_thing).collect())
        .unwrap_or_default();
    Ok(Thread { post, comments })
}

/// "Load more" without `/api/morechildren`, which Reddit has blocked ("blocked by network
/// security") since late October 2026: focused comment listings,
/// `/comments/{post}/_/{comment}?context=0`, one per parent (a reply row) or per comment (a
/// top-level row). The answer is a thread whose comments start at the focused comment.
pub fn focused_request(post_id: &str, comment_id: &str, sort: &str) -> (String, Vec<(String, String)>) {
    (
        format!("/comments/{post_id}/_/{comment_id}"),
        vec![("context".into(), "0".into()), ("limit".into(), "500".into()), ("sort".into(), sort.into())],
    )
}

/// How many comments a top-level "load more" fetches one by one per press.
pub const FOCUSED_BATCH: usize = 20;

/// A subtree pulled apart into a flat list in display order (each node's replies emptied): the
/// shape [`splice_more`] rebuilds by `parent_id`.
pub fn flatten_nodes(nodes: Vec<Node>, out: &mut Vec<Node>) {
    for n in nodes {
        match n {
            Node::Comment(mut c) => {
                let replies = std::mem::take(&mut c.replies);
                out.push(Node::Comment(c));
                flatten_nodes(replies, out);
            }
            more => out.push(more),
        }
    }
}

/// From a focused listing of the parent comment: its replies whose IDs are `wanted`, flattened,
/// and the IDs that came back (descendants included).
pub fn take_wanted_replies(focused: Vec<Node>, wanted: &[String]) -> (Vec<Node>, HashSet<String>) {
    let mut got = HashSet::new();
    let mut out = Vec::new();
    for root in focused {
        if let Node::Comment(c) = root {
            for r in c.replies {
                if let Node::Comment(rc) = &r {
                    if wanted.contains(&rc.id) {
                        let mut flat = Vec::new();
                        flatten_nodes(vec![r], &mut flat);
                        for f in &flat {
                            if let Node::Comment(fc) = f {
                                got.insert(fc.id.clone());
                            }
                        }
                        out.extend(flat);
                    }
                }
            }
        }
    }
    (out, got)
}

/// Query for `GET /api/morechildren`. Reddit accepts at most 100 IDs per call; `next_batch`
/// splits a long `More` so a "load more (500)" row fills in steps.
pub fn morechildren_query<'a>(link_fullname: &'a str, ids: &'a str, sort: &'a str) -> Vec<(&'a str, &'a str)> {
    vec![
        ("api_type", "json"),
        ("link_id", link_fullname),
        ("children", ids),
        ("limit_children", "false"),
        ("sort", sort),
    ]
}

pub const MORECHILDREN_MAX: usize = 100;

/// The first ≤100 child IDs of `more`, comma-joined, and the IDs left for a later call.
pub fn next_batch(more: &More) -> (String, Vec<String>) {
    let n = more.children.len().min(MORECHILDREN_MAX);
    (more.children[..n].join(","), more.children[n..].to_vec())
}

/// Parses `{"json":{"errors":[],"data":{"things":[...]}}}` into a flat list of nodes.
pub fn parse_morechildren(v: &Value) -> Result<Vec<Node>, RdError> {
    let j = &v["json"];
    if let Some(errs) = j["errors"].as_array().filter(|e| !e.is_empty()) {
        return Err(RdError::Parse(format!("morechildren: {errs:?}")));
    }
    Ok(j["data"]["things"]
        .as_array()
        .ok_or_else(|| RdError::Parse("morechildren has no things".into()))?
        .iter()
        .filter_map(parse_thing)
        .collect())
}

/// Replaces the `More` named `more_id` with the flat `things` from morechildren, rebuilt into a
/// tree by `parent_id`. `remaining` (IDs not yet fetched) becomes a new `More` at the same spot.
/// Returns false if `more_id` was not found.
pub fn splice_more(tree: &mut Vec<Node>, more_id: &str, things: Vec<Node>, remaining: Vec<String>) -> bool {
    fn find<'a>(nodes: &'a mut Vec<Node>, more_id: &str) -> Option<(&'a mut Vec<Node>, usize)> {
        if let Some(i) = nodes.iter().position(|n| matches!(n, Node::More(m) if m.id == more_id)) {
            return Some((nodes, i));
        }
        for n in nodes.iter_mut() {
            if let Node::Comment(c) = n {
                if let Some(hit) = find(&mut c.replies, more_id) {
                    return Some(hit);
                }
            }
        }
        None
    }
    let Some((siblings, at)) = find(tree, more_id) else { return false };
    let Node::More(old) = siblings.remove(at) else { unreachable!() };

    // Things arrive in display order; children come after their parent. Attach each to its parent
    // among the new things, or keep it as a top-level item at the More's position.
    let mut roots: Vec<Node> = Vec::new();
    for t in things {
        if !attach(&mut roots, t.parent_id().to_string(), &t) {
            roots.push(t);
        }
    }
    if !remaining.is_empty() {
        roots.push(Node::More(More {
            id: old.id.clone(),
            parent_id: old.parent_id.clone(),
            count: (old.count - (old.children.len() - remaining.len()) as i64).max(remaining.len() as i64),
            depth: old.depth,
            children: remaining,
        }));
    }
    for (k, n) in roots.into_iter().enumerate() {
        siblings.insert(at + k, n);
    }
    true
}

fn attach(nodes: &mut [Node], parent: String, child: &Node) -> bool {
    for n in nodes.iter_mut().rev() {
        if let Node::Comment(c) = n {
            if c.name == parent {
                c.replies.push(child.clone());
                return true;
            }
            if attach(&mut c.replies, parent.clone(), child) {
                return true;
            }
        }
    }
    false
}

/// One line in the comment pane.
#[derive(Debug, Clone, PartialEq)]
pub enum Row<'a> {
    Comment { c: &'a Comment, depth: u32, collapsed: bool, hidden_replies: usize },
    More { m: &'a More, depth: u32 },
}

/// Flattens the tree depth-first. A collapsed comment shows as one row and hides its subtree;
/// `collapsed` holds user toggles by fullname and flips Reddit's own default.
pub fn flatten<'a>(tree: &'a [Node], toggled: &HashSet<String>) -> Vec<Row<'a>> {
    fn walk<'a>(nodes: &'a [Node], depth: u32, toggled: &HashSet<String>, out: &mut Vec<Row<'a>>) {
        for n in nodes {
            match n {
                Node::Comment(c) => {
                    let collapsed = c.collapsed != toggled.contains(&c.name);
                    let hidden = if collapsed { count(&c.replies) } else { 0 };
                    out.push(Row::Comment { c, depth, collapsed, hidden_replies: hidden });
                    if !collapsed {
                        walk(&c.replies, depth + 1, toggled, out);
                    }
                }
                Node::More(m) => out.push(Row::More { m, depth }),
            }
        }
    }
    let mut out = Vec::new();
    walk(tree, 0, toggled, &mut out);
    out
}

/// Number of comments in a subtree (unfetched `More` counts included).
pub fn count(nodes: &[Node]) -> usize {
    nodes
        .iter()
        .map(|n| match n {
            Node::Comment(c) => 1 + count(&c.replies),
            Node::More(m) => m.count.max(0) as usize,
        })
        .sum()
}

/// Names (`t1_…`) of every comment in a tree.
pub fn comment_names(nodes: &[Node]) -> HashSet<String> {
    fn walk(nodes: &[Node], out: &mut HashSet<String>) {
        for n in nodes {
            if let Node::Comment(c) = n {
                out.insert(c.name.clone());
                walk(&c.replies, out);
            }
        }
    }
    let mut out = HashSet::new();
    walk(nodes, &mut out);
    out
}

/// A thread loaded again, laid over the copy on screen so nothing the user opened is lost: at
/// each level the fresh comments in Reddit's order (new scores, new replies), then the comments
/// only the old copy has (pages fetched with "load more", older ones past the fresh listing's
/// limit), then the fresh "load more" rows, without the comments the merged tree already holds.
pub fn merge_fresh(old: Vec<Node>, fresh: Vec<Node>) -> Vec<Node> {
    fn level(old: Vec<Node>, fresh: Vec<Node>) -> Vec<Node> {
        let mut kept: Vec<Comment> = old
            .into_iter()
            .filter_map(|n| match n {
                Node::Comment(c) => Some(c),
                Node::More(_) => None,
            })
            .collect();
        let mut out = Vec::new();
        let mut more = Vec::new();
        for n in fresh {
            match n {
                Node::Comment(mut c) => {
                    if let Some(i) = kept.iter().position(|o| o.name == c.name) {
                        let o = kept.remove(i);
                        c.replies = level(o.replies, std::mem::take(&mut c.replies));
                    }
                    out.push(Node::Comment(c));
                }
                m @ Node::More(_) => more.push(m),
            }
        }
        out.extend(kept.into_iter().map(Node::Comment));
        out.extend(more);
        out
    }
    fn ids(nodes: &[Node], out: &mut HashSet<String>) {
        for n in nodes {
            if let Node::Comment(c) = n {
                out.insert(c.id.clone());
                ids(&c.replies, out);
            }
        }
    }
    fn trim(nodes: &mut Vec<Node>, have: &HashSet<String>) {
        nodes.retain_mut(|n| match n {
            Node::Comment(c) => {
                trim(&mut c.replies, have);
                true
            }
            Node::More(m) if m.is_continue_thread() => true,
            Node::More(m) => {
                let before = m.children.len();
                m.children.retain(|id| !have.contains(id));
                m.count = (m.count - (before - m.children.len()) as i64).max(m.children.len() as i64);
                !m.children.is_empty()
            }
        });
    }
    let mut merged = level(old, fresh);
    let mut have = HashSet::new();
    ids(&merged, &mut have);
    trim(&mut merged, &have);
    merged
}

/// Whether a post reads like a thread people follow as it happens (a match, game, live or
/// discussion thread) and is young enough, under 12 hours, to still be one.
pub fn is_live_thread(title: &str, flair: Option<&str>, created_utc: i64, now: i64) -> bool {
    const PHRASES: [&str; 4] = ["match thread", "game thread", "live thread", "discussion thread"];
    if now - created_utc >= 12 * 3600 {
        return false;
    }
    let live = |s: &str| {
        let s = s.to_lowercase();
        PHRASES.iter().any(|p| s.contains(p)) || s.split(|c: char| !c.is_alphanumeric()).any(|w| w == "live")
    };
    live(title) || flair.is_some_and(live)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let p = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    fn names(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                Row::Comment { c, depth, .. } => format!("{}{}", "  ".repeat(*depth as usize), c.id),
                Row::More { m, depth } => format!("{}more({})", "  ".repeat(*depth as usize), m.count),
            })
            .collect()
    }

    #[test]
    fn parses_a_thread() {
        let t = parse_thread(&fixture("thread.json")).unwrap();
        assert_eq!(t.post.id, "1abc001");
        let rows = flatten(&t.comments, &HashSet::new());
        assert_eq!(names(&rows), ["c1", "  c2", "    c3", "  more(5)", "c4", "more(40)"]);
        let Row::Comment { c, .. } = &rows[0] else { panic!() };
        assert_eq!(c.body, "Top comment with a [link](https://example.com) & stuff");
        assert!(c.is_submitter);
        assert_eq!(count(&t.comments), 4 + 5 + 40);
    }

    #[test]
    fn collapsing_hides_the_subtree_and_respects_reddits_default() {
        let t = parse_thread(&fixture("thread.json")).unwrap();
        let mut toggled = HashSet::new();
        toggled.insert("t1_c1".to_string());
        let rows = flatten(&t.comments, &toggled);
        assert_eq!(names(&rows), ["c1", "c4", "more(40)"]);
        assert!(matches!(rows[0], Row::Comment { collapsed: true, hidden_replies: 7, .. }));
        // c4 is collapsed by Reddit; toggling it opens it.
        assert!(matches!(rows[1], Row::Comment { collapsed: true, .. }));
        toggled.insert("t1_c4".to_string());
        assert!(matches!(flatten(&t.comments, &toggled)[1], Row::Comment { collapsed: false, .. }));
    }

    #[test]
    fn splices_morechildren_into_place() {
        let mut t = parse_thread(&fixture("thread.json")).unwrap();
        let things = parse_morechildren(&fixture("morechildren.json")).unwrap();
        assert_eq!(things.len(), 3);
        // The nested More under c1 ("m1") had 5 children; 3 came back, 2 remain.
        assert!(splice_more(&mut t.comments, "m1", things, vec!["c8".into(), "c9".into()]));
        let rows = flatten(&t.comments, &HashSet::new());
        assert_eq!(names(&rows), ["c1", "  c2", "    c3", "  c5", "    c6", "  c7", "  more(2)", "c4", "more(40)"]);
        assert!(!splice_more(&mut t.comments, "nope", vec![], vec![]));
    }

    #[test]
    fn focused_listings_fill_a_more_row() {
        let mut t = parse_thread(&fixture("thread.json")).unwrap();
        // A focused listing of c1 returns c1 with all its replies; m1 wanted c5, c7 (and more).
        let things = parse_morechildren(&fixture("morechildren.json")).unwrap();
        let mut c1 = match &t.comments[0] { Node::Comment(c) => c.clone(), _ => panic!() };
        // Rebuild c1's replies as the focused listing would: c2 (already shown) + the new ones.
        let mut rebuilt: Vec<Node> = Vec::new();
        for n in things {
            if !attach(&mut rebuilt, n.parent_id().to_string(), &n) {
                rebuilt.push(n);
            }
        }
        c1.replies.extend(rebuilt);
        let wanted: Vec<String> = ["c5", "c7", "c8"].iter().map(|s| s.to_string()).collect();
        let (flat, got) = take_wanted_replies(vec![Node::Comment(c1)], &wanted);
        assert!(got.contains("c5") && got.contains("c6") && got.contains("c7") && !got.contains("c2"));
        let remaining: Vec<String> = wanted.into_iter().filter(|w| !got.contains(w)).collect();
        assert_eq!(remaining, ["c8"]);
        assert!(splice_more(&mut t.comments, "m1", flat, remaining));
        let rows = flatten(&t.comments, &HashSet::new());
        assert_eq!(names(&rows), ["c1", "  c2", "    c3", "  c5", "    c6", "  c7", "  more(1)", "c4", "more(40)"]);
        assert_eq!(focused_request("abc", "c1", "top").0, "/comments/abc/_/c1");
    }

    #[test]
    fn morechildren_batches_of_100() {
        let more = More { id: "m".into(), parent_id: "t3_x".into(), count: 250, depth: 0, children: (0..250).map(|i| format!("k{i}")).collect() };
        let (ids, rest) = next_batch(&more);
        assert_eq!(ids.split(',').count(), 100);
        assert_eq!(rest.len(), 150);
        assert!(!more.is_continue_thread());
        let q = morechildren_query("t3_x", &ids, "confidence");
        assert_eq!(q[0], ("api_type", "json"));
        assert!(parse_morechildren(&serde_json::json!({"json":{"errors":[["RATELIMIT"]]}})).is_err());
    }

    fn comment(id: &str, parent: &str, score: i64, replies: Vec<Node>) -> Node {
        let mut c = match parse_thing(&serde_json::json!({"kind": "t1", "data": {"id": id, "name": format!("t1_{id}"), "parent_id": parent}})) {
            Some(Node::Comment(c)) => c,
            _ => panic!(),
        };
        c.score = score;
        c.replies = replies;
        Node::Comment(c)
    }

    #[test]
    fn a_fresh_copy_keeps_what_was_loaded_and_adds_whats_new() {
        // On screen: c1 with c2 under it (c5 came from "load more"), then c4.
        let old = vec![
            comment("c1", "t3_p", 10, vec![comment("c2", "t1_c1", 1, vec![]), comment("c5", "t1_c1", 1, vec![])]),
            comment("c4", "t3_p", 3, vec![]),
        ];
        // Fresh: a new top comment first, c1 with a new score and a new reply, c5 back in a
        // "load more" row (with one comment never fetched), c4 past the listing's limit.
        let more = Node::More(More { id: "m".into(), parent_id: "t1_c1".into(), count: 2, depth: 1, children: vec!["c5".into(), "c9".into()] });
        let fresh = vec![comment("c7", "t3_p", 1, vec![]), comment("c1", "t3_p", 12, vec![comment("c2", "t1_c1", 1, vec![]), comment("c6", "t1_c1", 1, vec![]), more])];
        let before = comment_names(&old);
        let merged = merge_fresh(old, fresh);
        let rows = flatten(&merged, &HashSet::new());
        assert_eq!(names(&rows), ["c7", "c1", "  c2", "  c6", "  c5", "  more(1)", "c4"]);
        let Row::Comment { c, .. } = &rows[1] else { panic!() };
        assert_eq!(c.score, 12);
        let added: Vec<String> = comment_names(&merged).difference(&before).cloned().collect();
        assert_eq!(added.len(), 2);
        assert!(added.contains(&"t1_c7".to_string()) && added.contains(&"t1_c6".to_string()));
        // A "load more" row left with nothing to fetch goes; "continue this thread" stays.
        let cont = Node::More(More { id: "k".into(), parent_id: "t1_c2".into(), count: 0, depth: 2, children: vec![] });
        let gone = Node::More(More { id: "g".into(), parent_id: "t3_p".into(), count: 1, depth: 0, children: vec!["c4".into()] });
        let merged = merge_fresh(vec![comment("c4", "t3_p", 1, vec![cont.clone()])], vec![gone, comment("c4", "t3_p", 1, vec![cont])]);
        assert_eq!(names(&flatten(&merged, &HashSet::new())), ["c4", "  more(0)"]);
    }

    #[test]
    fn live_threads_by_title_or_flair_while_young() {
        let now = 1_000_000;
        assert!(is_live_thread("Match Thread: Arsenal vs Spurs", None, now - 600, now));
        assert!(is_live_thread("Post Game Thread: Lakers win", None, now - 600, now));
        assert!(is_live_thread("Election results", Some("LIVE"), now - 600, now));
        assert!(is_live_thread("[Live] Launch coverage", None, now - 600, now));
        assert!(!is_live_thread("I delivered pizza alive", None, now - 600, now));
        assert!(!is_live_thread("Match Thread: yesterday", None, now - 13 * 3600, now));
    }

    #[test]
    fn comments_carry_their_saved_state() {
        let Some(Node::Comment(c)) = parse_thing(&serde_json::json!({"kind": "t1", "data": {"id": "x", "saved": true}})) else { panic!() };
        assert!(c.saved);
        let t = parse_thread(&fixture("thread.json")).unwrap();
        assert!(t.comments.iter().all(|n| !matches!(n, Node::Comment(c) if c.saved)));
    }
}
