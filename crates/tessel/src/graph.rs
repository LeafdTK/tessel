//! the shape of history: which lane each commit sits in, and the lines that
//! join it to its children above and its parents below.
//!
//! pure and incremental-friendly: give it commits newest first, children
//! before parents (as `git log --date-order` lists them), and it gives back
//! one row per commit to paint. nothing here knows about pixels.

use tessel_proto::Commit;

/// where on a row an edge starts or ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Top,
    Middle,
    Bottom,
}

/// a line inside one row, between two lanes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edge {
    pub from: (u16, Level),
    pub to: (u16, Level),
    /// which colour its line has: lanes keep theirs from branch to merge.
    pub color: u16,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    /// the commit's lane.
    pub lane: u16,
    pub color: u16,
    pub edges: Vec<Edge>,
    /// lanes this row uses: the gutter is this wide.
    pub width: u16,
}

/// lay out history. a commit takes the lane of the first child waiting for
/// it; its first parent continues that lane, other parents (a merge) take the
/// lane already waiting for them or a new one.
pub fn layout(commits: &[Commit]) -> Vec<Row> {
    // per lane: the commit it's waiting for, and its colour.
    let mut active: Vec<Option<(&str, u16)>> = Vec::new();
    let mut next_color: u16 = 0;
    let mut rows = Vec::with_capacity(commits.len());
    let free = |active: &mut Vec<Option<(&str, u16)>>| match active.iter().position(Option::is_none) {
        Some(i) => i,
        None => {
            active.push(None);
            active.len() - 1
        }
    };
    for c in commits {
        let waiting: Vec<usize> = active.iter().enumerate().filter(|(_, a)| a.is_some_and(|(s, _)| s == c.sha)).map(|(i, _)| i).collect();
        let (lane, color) = match waiting.first() {
            Some(&i) => (i, active[i].expect("waiting").1),
            None => {
                let i = free(&mut active);
                next_color += 1;
                (i, next_color - 1)
            }
        };
        let mut edges = Vec::new();
        for (i, a) in active.iter().enumerate() {
            let Some((sha, col)) = a else { continue };
            if *sha == c.sha {
                edges.push(Edge { from: (i as u16, Level::Top), to: (lane as u16, Level::Middle), color: *col });
            } else {
                edges.push(Edge { from: (i as u16, Level::Top), to: (i as u16, Level::Bottom), color: *col });
            }
        }
        for i in &waiting {
            active[*i] = None;
        }
        if let Some((first, rest)) = c.parents.split_first() {
            active[lane] = Some((first.as_str(), color));
            edges.push(Edge { from: (lane as u16, Level::Middle), to: (lane as u16, Level::Bottom), color });
            for p in rest {
                let (j, col) = match active.iter().position(|a| a.is_some_and(|(s, _)| s == p)) {
                    Some(j) => (j, active[j].expect("found").1),
                    None => {
                        let j = free(&mut active);
                        next_color += 1;
                        active[j] = Some((p.as_str(), next_color - 1));
                        (j, next_color - 1)
                    }
                };
                edges.push(Edge { from: (lane as u16, Level::Middle), to: (j as u16, Level::Bottom), color: col });
            }
        }
        while active.last().is_some_and(Option::is_none) {
            active.pop();
        }
        let width = edges.iter().map(|e| e.from.0.max(e.to.0) + 1).max().unwrap_or(1).max(lane as u16 + 1);
        rows.push(Row { lane: lane as u16, color, edges, width });
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(sha: &str, parents: &[&str]) -> Commit {
        Commit { sha: sha.into(), parents: parents.iter().map(|p| (*p).to_owned()).collect(), refs: vec![], author: String::new(), time_ms: 0, subject: String::new() }
    }

    #[test]
    fn a_straight_line_stays_in_one_lane() {
        let rows = layout(&[c("c", &["b"]), c("b", &["a"]), c("a", &[])]);
        assert!(rows.iter().all(|r| r.lane == 0 && r.width == 1));
        assert_eq!(rows[0].edges, [Edge { from: (0, Level::Middle), to: (0, Level::Bottom), color: 0 }], "the newest has nothing above");
        assert_eq!(rows[2].edges, [Edge { from: (0, Level::Top), to: (0, Level::Middle), color: 0 }], "the first has nothing below");
    }

    #[test]
    fn a_branch_and_its_merge_open_and_close_a_second_lane() {
        //  m        merge of a feature into main
        //  |\
        //  | f      the feature
        //  b |      main moved on
        //  |/
        //  a        where they split
        let rows = layout(&[c("m", &["b", "f"]), c("f", &["a"]), c("b", &["a"]), c("a", &[])]);
        assert_eq!(rows.iter().map(|r| r.lane).collect::<Vec<_>>(), [0, 1, 0, 0]);
        assert!(rows[0].edges.contains(&Edge { from: (0, Level::Middle), to: (1, Level::Bottom), color: 1 }), "the merge reaches out to the feature's lane");
        assert!(rows[2].edges.contains(&Edge { from: (1, Level::Top), to: (1, Level::Bottom), color: 1 }), "the feature passes by main's commit");
        // both lanes wait for `a`: they come together on it.
        let joins: Vec<&Edge> = rows[3].edges.iter().filter(|e| e.to == (0, Level::Middle)).collect();
        assert_eq!(joins.len(), 2, "{:?}", rows[3].edges);
        assert_eq!(rows[1].color, 1, "the feature keeps its own colour");
    }

    #[test]
    fn a_lane_freed_by_a_merge_is_used_again_and_the_gutter_stays_narrow() {
        let rows = layout(&[c("m2", &["m1", "g"]), c("g", &["m1"]), c("m1", &["x", "f"]), c("f", &["x"]), c("x", &[])]);
        assert!(rows.iter().all(|r| r.width <= 2), "{rows:?}");
    }
}
