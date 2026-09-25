//! Typeahead filtering: which parts of a tree match a name.
//!
//! A node whose name contains the needle (ignoring ASCII case) matches, and
//! is kept whole: everything under a matching directory goes with it. Its
//! ancestors are kept only partly, and sized by what matched beneath them,
//! so a filtered treemap shows exactly the matches, at their true relative
//! sizes, in the places they live.

use rustc_hash::FxHashMap;

use crate::classify::Category;
use crate::tree::{Metric, Node};

/// How a node takes part in a filtered view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keep {
    /// It matches: drawn as usual, with everything beneath it.
    Whole,
    /// It holds matches: drawn with only those, at their size.
    Partial { bytes: u64, files: u64 },
}

/// The outcome of filtering a subtree by name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Matches {
    /// The needle, lowercased.
    pub needle: String,
    /// Absolute crumbs of the subtree that was searched.
    pub base: Vec<usize>,
    /// Keyed by absolute crumbs. Only matches and their ancestors appear;
    /// a match's own descendants are implied.
    pub keep: FxHashMap<Vec<usize>, Keep>,
    /// Topmost matches: a match inside a match is not counted again.
    pub count: usize,
    pub bytes: u64,
    pub files: u64,
}

impl Matches {
    /// How the node at `crumbs` takes part: `None` when it is filtered out.
    /// Anything outside the searched subtree, and anything beneath a match,
    /// is kept whole.
    pub fn keep(&self, crumbs: &[usize]) -> Option<Keep> {
        if !crumbs.starts_with(&self.base) {
            return Some(Keep::Whole);
        }
        for length in self.base.len()..=crumbs.len() {
            match self.keep.get(&crumbs[..length]) {
                Some(Keep::Whole) => return Some(Keep::Whole),
                Some(partial) if length == crumbs.len() => {
                    return Some(*partial);
                }
                // Only the base may be absent: it holds the matches but is
                // not recorded, being where the search started.
                None if length > self.base.len() => return None,
                _ => {}
            }
        }
        // The base itself holds the matches.
        Some(Keep::Partial {
            bytes: self.bytes,
            files: self.files,
        })
    }

    /// The value a kept node is laid out by.
    pub const fn value(keep: Keep, node: &Node, metric: Metric) -> u64 {
        match (keep, metric) {
            (Keep::Whole, _) => node.value(metric),
            (Keep::Partial { bytes, .. }, Metric::Bytes) => bytes,
            (Keep::Partial { files, .. }, Metric::Files) => files,
        }
    }
}

/// Search `node`, found at absolute `base`, for names containing `needle`.
/// `None` for an empty needle: nothing is filtered.
pub fn filter(node: &Node, base: &[usize], needle: &str) -> Option<Matches> {
    let needle = needle.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return None;
    }
    let mut matches = Matches {
        needle,
        base: base.to_vec(),
        ..Matches::default()
    };
    let mut crumbs = base.to_vec();
    let needle = matches.needle.clone();
    let (bytes, files) =
        visit(node, &mut crumbs, &mut matches, &|child: &Node| {
            contains_ignoring_case(&child.name, &needle)
        });
    matches.bytes = bytes;
    matches.files = files;
    Some(matches)
}

/// What a legend pick keeps, rather than a typed name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Query {
    /// Files of this kind. Categories are inherited but a name can
    /// override its parent's — `node_modules` in a checkout is cache, not
    /// code — so files are judged one by one, and a folder is kept
    /// whole only when every file in it matched.
    Category(Category),
    /// Space that can be had back. Nothing under a reclaimable folder can
    /// be anything else, so it is kept whole where it starts.
    Reclaimable,
    /// Files last written more than `after` and at most `through` days
    /// before `now` (Unix seconds); judged file by file, like a category.
    Age { now: i64, after: i64, through: i64 },
}

impl Query {
    fn takes_file(self, node: &Node) -> bool {
        match self {
            Self::Category(category) => node.category == category,
            Self::Reclaimable => node.reclaim.is_some(),
            Self::Age {
                now,
                after,
                through,
            } => {
                let days = (now - node.modified).max(0) / 86_400;
                crate::tree::known_time(node.modified)
                    && days > after
                    && days <= through
            }
        }
    }
}

/// Keep what `query` picks out of `node`, found at absolute `base`.
/// `label` names the pick where the interface talks about it.
pub fn filter_query(
    node: &Node,
    base: &[usize],
    query: Query,
    label: &str,
) -> Matches {
    let mut matches = Matches {
        needle: label.to_string(),
        base: base.to_vec(),
        ..Matches::default()
    };
    let mut crumbs = base.to_vec();
    let (bytes, files) = if query == Query::Reclaimable {
        visit(node, &mut crumbs, &mut matches, &|child: &Node| {
            query.takes_file(child)
        })
    } else {
        let mut kept = Vec::new();
        let (bytes, files, _) = visit_files(
            node,
            &mut crumbs,
            &|child: &Node| query.takes_file(child),
            &mut kept,
        );
        matches.keep.extend(kept);
        (bytes, files)
    };
    matches.bytes = bytes;
    matches.files = files;
    // Places, not files: a folder kept whole counts once.
    matches.count = matches
        .keep
        .values()
        .filter(|keep| **keep == Keep::Whole)
        .count();
    matches
}

/// File-by-file matching. Returns the bytes and files that matched beneath
/// `node`, and whether everything beneath it did. Entries go to `kept` in
/// visiting order, so a folder that matched completely can drop its
/// children's entries and stand for them as one whole match.
fn visit_files(
    node: &Node,
    crumbs: &mut Vec<usize>,
    takes: &dyn Fn(&Node) -> bool,
    kept: &mut Vec<(Vec<usize>, Keep)>,
) -> (u64, u64, bool) {
    let (mut bytes, mut files, mut whole) = (0, 0, true);
    for (index, child) in node.children.iter().enumerate() {
        crumbs.push(index);
        if child.is_dir() {
            let start = kept.len();
            let (child_bytes, child_files, child_whole) =
                visit_files(child, crumbs, takes, kept);
            if child_bytes > 0 || child_files > 0 {
                let keep = if child_whole {
                    kept.truncate(start);
                    Keep::Whole
                } else {
                    Keep::Partial {
                        bytes: child_bytes,
                        files: child_files,
                    }
                };
                kept.push((crumbs.clone(), keep));
                bytes += child_bytes;
                files += child_files;
            }
            whole &= child_whole;
        } else if takes(child) {
            kept.push((crumbs.clone(), Keep::Whole));
            bytes += child.bytes;
            files += child.files;
        } else {
            whole = false;
        }
        crumbs.pop();
    }
    (bytes, files, whole)
}

/// Returns the bytes and files that matched at or beneath `node`'s
/// children, recording what to keep. A child that `takes` is kept whole,
/// with everything beneath it.
fn visit(
    node: &Node,
    crumbs: &mut Vec<usize>,
    matches: &mut Matches,
    takes: &dyn Fn(&Node) -> bool,
) -> (u64, u64) {
    let mut total = (0, 0);
    for (index, child) in node.children.iter().enumerate() {
        crumbs.push(index);
        if takes(child) {
            matches.keep.insert(crumbs.clone(), Keep::Whole);
            matches.count += 1;
            total.0 += child.bytes;
            total.1 += child.files;
        } else if !child.children.is_empty() {
            let (bytes, files) = visit(child, crumbs, matches, takes);
            if bytes > 0 || files > 0 {
                matches
                    .keep
                    .insert(crumbs.clone(), Keep::Partial { bytes, files });
                total.0 += bytes;
                total.1 += files;
            }
        }
        crumbs.pop();
    }
    total
}

/// Substring search ignoring ASCII case, without allocating: a filter runs
/// over every name in view on every keystroke.
fn contains_ignoring_case(haystack: &str, lower_needle: &str) -> bool {
    let (hay, needle) = (haystack.as_bytes(), lower_needle.as_bytes());
    if needle.is_empty() {
        return true;
    }
    if needle.len() > hay.len() {
        return false;
    }
    hay.windows(needle.len()).any(|window| {
        window
            .iter()
            .zip(needle)
            .all(|(left, right)| left.to_ascii_lowercase() == *right)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::{NodeKind, aggregate};

    fn file(name: &str, bytes: u64) -> Node {
        Node::entry(name, NodeKind::File, bytes)
    }

    fn dir(name: &str, children: Vec<Node>) -> Node {
        let mut node = Node::directory(name);
        node.children = children;
        node
    }

    fn tree() -> Node {
        let mut root = dir(
            "root",
            vec![
                dir(
                    "src",
                    vec![
                        dir("App", vec![file("main.rs", 10)]),
                        file("notes", 5),
                    ],
                ),
                dir(
                    "apps",
                    vec![file("x", 100), dir("apple", vec![file("y", 7)])],
                ),
                file("readme", 1),
            ],
        );
        aggregate(&mut root, Metric::Bytes);
        root
    }

    fn crumbs_of(root: &Node, names: &[&str]) -> Vec<usize> {
        let mut node = root;
        names
            .iter()
            .map(|name| {
                let index = node
                    .children
                    .iter()
                    .position(|child| &*child.name == *name)
                    .expect("present");
                node = &node.children[index];
                index
            })
            .collect()
    }

    #[test]
    fn matches_are_kept_whole_and_their_ancestors_by_what_matched() {
        let root = tree();
        let found = filter(&root, &[], "APP").expect("a needle");
        assert_eq!(found.count, 2, "src/App and apps; apple is inside apps");
        assert_eq!(found.bytes, 10 + 107);
        assert_eq!(found.keep(&crumbs_of(&root, &["apps"])), Some(Keep::Whole));
        assert_eq!(
            found.keep(&crumbs_of(&root, &["apps", "x"])),
            Some(Keep::Whole),
            "inside a match"
        );
        assert_eq!(
            found.keep(&crumbs_of(&root, &["src"])),
            Some(Keep::Partial {
                bytes: 10,
                files: 1
            })
        );
        assert_eq!(found.keep(&crumbs_of(&root, &["src", "notes"])), None);
        assert_eq!(found.keep(&crumbs_of(&root, &["readme"])), None);
    }

    #[test]
    fn a_search_below_the_root_leaves_the_rest_alone() {
        let root = tree();
        let src = crumbs_of(&root, &["src"]);
        let found = filter(root.resolve(&src).expect("src"), &src, "main")
            .expect("needle");
        assert_eq!(found.keep(&crumbs_of(&root, &["apps"])), Some(Keep::Whole));
        assert_eq!(found.keep(&crumbs_of(&root, &["src", "notes"])), None);
        assert_eq!(
            found.keep(&src),
            Some(Keep::Partial {
                bytes: 10,
                files: 1
            })
        );
    }

    #[test]
    fn an_empty_needle_filters_nothing() {
        assert!(filter(&tree(), &[], "  ").is_none());
        let found = filter(&tree(), &[], "zzz").expect("needle");
        assert_eq!(found.count, 0);
        assert!(found.keep.is_empty());
    }

    #[test]
    fn case_is_ignored_without_allocating() {
        assert!(contains_ignoring_case("Cargo.TOML", "toml"));
        assert!(!contains_ignoring_case("ab", "abc"));
        assert!(contains_ignoring_case("x", ""));
    }

    /// Colour a node and everything under it, as classify's inheritance
    /// would.
    fn paint(mut node: Node, category: Category) -> Node {
        fn walk(node: &mut Node, category: Category) {
            node.category = category;
            for child in &mut node.children {
                walk(child, category);
            }
        }
        walk(&mut node, category);
        node
    }

    /// A checkout with `node_modules` inside, a plain source folder, and a
    /// photo.
    fn kinds() -> Node {
        let repo = dir(
            "repo",
            vec![
                file("main.rs", 10),
                paint(
                    dir("node_modules", vec![file("lib.js", 50)]),
                    Category::Toolchain,
                ),
            ],
        );
        let mut root = dir(
            "root",
            vec![
                paint(repo, Category::Code),
                paint(dir("src", vec![file("a.rs", 20)]), Category::Code),
                paint(file("photo.jpg", 5), Category::Media),
            ],
        );
        // The override the inheritance allows: node_modules stays a
        // toolchain inside a code folder.
        let modules = crumbs_of(&root, &["repo", "node_modules"]);
        root.children[modules[0]].children[modules[1]] = paint(
            dir("node_modules", vec![file("lib.js", 50)]),
            Category::Toolchain,
        );
        aggregate(&mut root, Metric::Bytes);
        root
    }

    #[test]
    fn a_category_keeps_its_files_and_leaves_out_what_overrides_it() {
        let root = kinds();
        let code =
            filter_query(&root, &[], Query::Category(Category::Code), "Code");
        assert_eq!(code.bytes, 30, "main.rs and a.rs, not lib.js");
        assert_eq!(code.count, 2, "main.rs, and src as one place");
        let repo = crumbs_of(&root, &["repo"]);
        assert_eq!(
            code.keep(&repo),
            Some(Keep::Partial {
                bytes: 10,
                files: 1
            }),
            "the checkout, without its node_modules"
        );
        assert_eq!(
            code.keep(&crumbs_of(&root, &["repo", "node_modules"])),
            None
        );
        assert_eq!(code.keep(&crumbs_of(&root, &["src"])), Some(Keep::Whole));
        assert_eq!(code.keep(&crumbs_of(&root, &["photo.jpg"])), None);
        assert_eq!(code.needle, "Code");
    }

    #[test]
    fn reclaimable_space_is_kept_whole_where_it_starts() {
        let mut cache = dir("cache", vec![file("blob", 40), file("more", 2)]);
        cache.reclaim = Some(crate::classify::Reclaim::Regenerable);
        for child in &mut cache.children {
            child.reclaim = Some(crate::classify::Reclaim::Regenerable);
        }
        let mut root = dir("root", vec![cache, file("keep", 9)]);
        aggregate(&mut root, Metric::Bytes);
        let found = filter_query(&root, &[], Query::Reclaimable, "Reclaimable");
        assert_eq!((found.bytes, found.count), (42, 1));
        assert_eq!(
            found.keep(&crumbs_of(&root, &["cache"])),
            Some(Keep::Whole)
        );
    }

    #[test]
    fn an_age_band_keeps_files_by_their_last_write() {
        const DAY: i64 = 86_400;
        // Mid-2024, well clear of the placeholder stamps.
        let now = 19_900 * DAY;
        let written = |name, bytes, days_ago: i64| {
            let mut node = file(name, bytes);
            node.modified = now - days_ago * DAY;
            node
        };
        let mut root = dir(
            "root",
            vec![
                dir("old", vec![written("a", 70, 800), written("b", 30, 400)]),
                dir("mixed", vec![written("c", 5, 500), written("d", 1, 2)]),
                file("undated", 3),
            ],
        );
        aggregate(&mut root, Metric::Bytes);
        let older = Query::Age {
            now,
            after: 365,
            through: i64::MAX,
        };
        let found = filter_query(&root, &[], older, "Older");
        assert_eq!(found.bytes, 105);
        assert_eq!(found.keep(&crumbs_of(&root, &["old"])), Some(Keep::Whole));
        assert_eq!(
            found.keep(&crumbs_of(&root, &["mixed"])),
            Some(Keep::Partial { bytes: 5, files: 1 })
        );
        assert_eq!(found.keep(&crumbs_of(&root, &["undated"])), None);
    }
}
