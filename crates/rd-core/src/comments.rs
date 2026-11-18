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
            body: s(d, "body"),
            score: i(d, "score"),
            score_hidden: b(d, "score_hidden"),
            created_utc: i(d, "created_utc"),
            depth: i(d, "depth").max(0) as u32,
            is_submitter: b(d, "is_submitter"),
            distinguished: opt_s(d, "distinguished"),
            stickied: b(d, "stickied"),
            collapsed: b(d, "collapsed"),
            likes: d["likes"].as_bool(),
            author_flair_text: opt_s(d, "author_flair_text"),
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
}
