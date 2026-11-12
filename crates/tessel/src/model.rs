//! what the app knows, with no ui in it.
//!
//! everything here is plain data and plain functions so it can be tested
//! without opening a window. the view only reads this and draws it.

use tessel_proto::{
    AttentionReason, Item, ItemBody, ItemId, Millis, PermissionRequest, SessionId, SessionSnapshot, SessionSummary,
};
use std::collections::HashMap;

#[derive(Default)]
pub struct Model {
    pub sessions: Vec<SessionSummary>,
    pub open: Option<Open>,
    pub connection: Connection,
    /// projects folded shut in the sidebar: their header shows, their sessions don't.
    pub collapsed: std::collections::HashSet<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Connection {
    #[default]
    Connecting,
    Connected,
    /// not connected, and why. the app keeps retrying.
    Down(String),
}

/// the session whose transcript is on screen.
pub struct Open {
    pub id: SessionId,
    /// in the order they arrived.
    items: Vec<Item>,
    index: HashMap<ItemId, usize>,
    /// display order: indices into `items`, with each item's children placed
    /// directly under it, and how deeply each row is nested.
    rows: Vec<(usize, u8)>,
    /// where each item sits in `rows`.
    row_of: Vec<usize>,
    pub pending: Vec<PermissionRequest>,
    pub has_more: bool,
}

/// what changed in the transcript, so the list can be told precisely.
#[derive(Debug, PartialEq, Eq)]
pub enum Change {
    None,
    /// rows were added at the end.
    Appended(usize),
    /// one existing row changed and may be a different height now.
    Row(usize),
    /// everything may have changed.
    Reset,
}

impl Open {
    pub fn from_snapshot(snap: SessionSnapshot) -> Self {
        let mut items = snap.items;
        items.sort_by_key(|i| i.order);
        let index = items.iter().enumerate().map(|(i, item)| (item.id.clone(), i)).collect();
        let mut open = Self { id: snap.summary.id, items, index, rows: Vec::new(), row_of: Vec::new(), pending: snap.pending, has_more: snap.has_more };
        open.arrange();
        open
    }

    /// number of rows on screen.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// the item on a row, and how deeply it is nested.
    /// every item, in the order they arrived.
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn row(&self, row: usize) -> Option<(&Item, u8)> {
        self.rows.get(row).map(|&(i, depth)| (&self.items[i], depth))
    }

    #[cfg(test)]
    fn shown(&self) -> Vec<(&str, u8)> {
        (0..self.len()).map(|r| self.row(r).unwrap()).map(|(i, d)| (i.id.as_str(), d)).collect()
    }

    /// work out the display order: arrival order, except that anything done
    /// inside a tool call (a subagent's steps) sits directly under that call.
    /// two subagents running at once interleave in time; shown that way you
    /// can't tell whose step is whose.
    fn arrange(&mut self) {
        let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut top = Vec::new();
        for (i, item) in self.items.iter().enumerate() {
            // a parent we don't have (older than what's loaded) can't hold its
            // children, so they are shown at the top level rather than lost.
            match item.parent.as_ref().and_then(|p| self.index.get(p)).filter(|&&p| p != i) {
                Some(&p) => children.entry(p).or_default().push(i),
                None => top.push(i),
            }
        }
        self.rows.clear();
        let mut stack: Vec<(usize, u8)> = top.into_iter().rev().map(|i| (i, 0)).collect();
        while let Some((i, depth)) = stack.pop() {
            self.rows.push((i, depth));
            if let Some(kids) = children.get(&i) {
                stack.extend(kids.iter().rev().map(|&k| (k, depth.saturating_add(1))));
            }
        }
        self.row_of = vec![0; self.items.len()];
        for (row, &(i, _)) in self.rows.iter().enumerate() {
            self.row_of[i] = row;
        }
    }

    pub fn upsert(&mut self, item: Item) -> Change {
        if let Some(&i) = self.index.get(&item.id) {
            // an older version of something we already have newer: ignore it.
            if item.seq < self.items[i].seq {
                return Change::None;
            }
            let moved = self.items[i].parent != item.parent;
            self.items[i] = item;
            if moved {
                self.arrange();
                return Change::Reset;
            }
            return Change::Row(self.row_of[i]);
        }
        let in_order = self.items.last().is_none_or(|last| item.order >= last.order);
        if in_order {
            self.index.insert(item.id.clone(), self.items.len());
            self.items.push(item);
        } else {
            // arrived out of order (should not happen, but never corrupt the list).
            let at = self.items.partition_point(|i| i.order < item.order);
            self.items.insert(at, item);
            self.index = self.items.iter().enumerate().map(|(i, it)| (it.id.clone(), i)).collect();
        }
        let before = self.rows.len();
        self.arrange();
        // the common case: the new row landed at the very end.
        if in_order && self.rows.last().map(|r| r.0) == Some(self.items.len() - 1) && self.rows.len() == before + 1 {
            Change::Appended(1)
        } else {
            Change::Reset
        }
    }

    /// append streaming text. a delta that doesn't start where our text ends
    /// is dropped rather than applied in the wrong place: the completed item
    /// that follows carries the full text anyway.
    pub fn delta(&mut self, id: &ItemId, offset: u64, text: &str) -> Change {
        let Some(&i) = self.index.get(id) else { return Change::None };
        match &mut self.items[i].body {
            ItemBody::Text { text: t, streaming: true } | ItemBody::Thinking { text: t, streaming: true }
                if t.len() as u64 == offset =>
            {
                t.push_str(text);
                Change::Row(self.row_of[i])
            }
            _ => Change::None,
        }
    }
}

impl Model {
    pub fn summary(&self, id: SessionId) -> Option<&SessionSummary> {
        self.sessions.iter().find(|s| s.id == id)
    }

    pub fn open_summary(&self) -> Option<&SessionSummary> {
        self.open.as_ref().and_then(|o| self.summary(o.id))
    }

    pub fn set_sessions(&mut self, mut sessions: Vec<SessionSummary>) {
        sessions.sort_by_key(|s| (s.created_ms, s.id));
        self.sessions = sessions;
    }

    /// returns true if this session just started wanting attention.
    pub fn update_session(&mut self, summary: SessionSummary) -> bool {
        match self.sessions.iter_mut().find(|s| s.id == summary.id) {
            Some(existing) => {
                let newly = summary.attention.is_some() && existing.attention.as_ref().map(|a| a.reason) != summary.attention.as_ref().map(|a| a.reason);
                *existing = summary;
                newly
            }
            None => {
                let newly = summary.attention.is_some();
                self.sessions.push(summary);
                self.sessions.sort_by_key(|s| (s.created_ms, s.id));
                newly
            }
        }
    }

    pub fn remove_session(&mut self, id: SessionId) {
        self.sessions.retain(|s| s.id != id);
        if self.open.as_ref().is_some_and(|o| o.id == id) {
            self.open = None;
        }
    }

    /// sessions that want you, the one that has waited longest first.
    /// "blocked on you" always comes before "finished": one is holding work up.
    pub fn attention_queue(&self) -> Vec<&SessionSummary> {
        let mut q: Vec<_> = self.sessions.iter().filter(|s| s.attention.is_some()).collect();
        q.sort_by_key(|s| {
            let a = s.attention.as_ref().unwrap();
            let blocked = matches!(a.reason, AttentionReason::NeedsPermission | AttentionReason::NeedsInput);
            (!blocked, a.since_ms, s.id)
        });
        q
    }

    /// the next session that wants you after the current one, wrapping around.
    pub fn next_attention(&self, current: Option<SessionId>) -> Option<SessionId> {
        let q = self.attention_queue();
        if q.is_empty() {
            return None;
        }
        let at = current.and_then(|c| q.iter().position(|s| s.id == c));
        let next = match at {
            Some(i) => q[(i + 1) % q.len()],
            None => q[0],
        };
        // the only one waiting is the one you're already looking at.
        (Some(next.id) != current).then_some(next.id)
    }
}

// ---- the sidebar's layout, as arithmetic ------------------------------------
//
// the sidebar is a "needs you" queue followed by sessions grouped by project.
// where each row sits is plain arithmetic on fixed heights, done here so the
// animation ("this row was 132 points lower a moment ago") can be tested
// without drawing anything.

pub const HEADER_H: f32 = 32.0;
pub const ROW_H: f32 = 42.0;
/// how far below its place a brand new row starts.
pub const ENTER_FROM: f32 = 12.0;
pub const QUEUE_TITLE: &str = "Needs You";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    Header(String),
    Row(SessionId),
    /// something running inside the session above it: a subagent or a
    /// background command, by its task id.
    Sub(SessionId, String),
}

/// a sub-row is a single quiet line.
pub const SUB_H: f32 = 26.0;
/// more than this many at once and the rest are counted, not listed.
pub const SUBS_SHOWN: usize = 6;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SidebarLayout {
    pub entries: Vec<Entry>,
}

/// the project a session belongs to: its repository if it has a worktree,
/// otherwise the folder it runs in.
pub fn project_name(s: &SessionSummary) -> String {
    let path = s.worktree.as_ref().map_or(s.cwd.as_str(), |w| w.repo.as_str());
    path.trim_end_matches('/').rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or("/").to_owned()
}

fn subs(s: &SessionSummary) -> impl Iterator<Item = Entry> + '_ {
    s.tasks.iter().take(SUBS_SHOWN).map(|t| Entry::Sub(s.id, t.id.clone()))
}

impl SidebarLayout {
    /// the queue on top (blocked first, longest wait first), then every other
    /// session under its project, newest first. a session is in one place only.
    pub fn build(model: &Model) -> Self {
        let mut entries = Vec::new();
        // a workspace is a folder, and its sessions are its tabs: one row each,
        // standing for the tab that wants you, else the one busy most recently.
        let mut stand_in: HashMap<&str, &SessionSummary> = HashMap::new();
        for s in &model.sessions {
            let keep = match stand_in.get(s.cwd.as_str()) {
                None => true,
                Some(cur) => {
                    let rank = |x: &SessionSummary| (x.attention.is_some(), x.updated_ms.max(x.created_ms));
                    rank(s) > rank(cur)
                }
            };
            if keep {
                stand_in.insert(s.cwd.as_str(), s);
            }
        }
        let shown = |s: &SessionSummary| stand_in.get(s.cwd.as_str()).is_some_and(|r| r.id == s.id);
        let queue: Vec<&SessionSummary> = model.attention_queue().into_iter().filter(|s| shown(s)).collect();
        if !queue.is_empty() {
            entries.push(Entry::Header(QUEUE_TITLE.to_owned()));
            for s in &queue {
                entries.push(Entry::Row(s.id));
                entries.extend(subs(s));
            }
        }
        let mut rest: Vec<&SessionSummary> = model.sessions.iter().filter(|s| s.attention.is_none() && shown(s)).collect();
        rest.sort_by(|a, b| project_name(a).cmp(&project_name(b)).then(b.created_ms.cmp(&a.created_ms)).then(a.id.cmp(&b.id)));
        let mut current: Option<String> = None;
        for s in rest {
            let project = project_name(s);
            if current.as_ref() != Some(&project) {
                entries.push(Entry::Header(project.clone()));
                current = Some(project);
            }
            if model.collapsed.contains(current.as_deref().unwrap_or_default()) {
                continue;
            }
            entries.push(Entry::Row(s.id));
            entries.extend(subs(s));
        }
        Self { entries }
    }

    pub fn height(&self) -> f32 {
        self.entries
            .iter()
            .map(|e| match e {
                Entry::Header(_) => HEADER_H,
                Entry::Row(_) => ROW_H,
                Entry::Sub(..) => SUB_H,
            })
            .sum()
    }

    /// the top edge of every row.
    pub fn row_tops(&self) -> HashMap<SessionId, f32> {
        let mut y = 0.0;
        let mut tops = HashMap::new();
        for e in &self.entries {
            match e {
                Entry::Header(_) => y += HEADER_H,
                Entry::Sub(..) => y += SUB_H,
                Entry::Row(id) => {
                    tops.insert(*id, y);
                    y += ROW_H;
                }
            }
        }
        tops
    }

    pub fn rows(&self) -> impl Iterator<Item = SessionId> + '_ {
        self.entries.iter().filter_map(|e| match e {
            Entry::Row(id) => Some(*id),
            Entry::Header(_) | Entry::Sub(..) => None,
        })
    }

    /// going from this layout to `next`: for each row, how far from its new
    /// place it should appear to start (old top − new top). rows that did not
    /// move are left out. a row that was not here before starts a little below.
    pub fn moves_to(&self, next: &SidebarLayout) -> Vec<(SessionId, f32)> {
        let (old, new) = (self.row_tops(), next.row_tops());
        let mut moves: Vec<(SessionId, f32)> = new
            .iter()
            .filter_map(|(id, new_y)| match old.get(id) {
                Some(old_y) if old_y != new_y => Some((*id, old_y - new_y)),
                Some(_) => None,
                None => Some((*id, ENTER_FROM)),
            })
            .collect();
        moves.sort_by_key(|(id, _)| *id);
        moves
    }

    /// what to show while the pointer is resting on the list: this layout's
    /// order, minus sessions that are gone, with newcomers added at the end.
    /// rows must not change places under a cursor that is about to click.
    pub fn held(&self, next: &SidebarLayout) -> SidebarLayout {
        let alive: std::collections::HashSet<SessionId> = next.rows().collect();
        let here: std::collections::HashSet<SessionId> = self.rows().collect();
        let mut entries: Vec<Entry> = Vec::new();
        for e in &self.entries {
            match e {
                Entry::Row(id) | Entry::Sub(id, _) if alive.contains(id) => entries.push(e.clone()),
                Entry::Row(_) | Entry::Sub(..) => {}
                Entry::Header(_) => {
                    // a header whose rows have all gone goes with them.
                    if matches!(entries.last(), Some(Entry::Header(_))) {
                        entries.pop();
                    }
                    entries.push(e.clone());
                }
            }
        }
        if matches!(entries.last(), Some(Entry::Header(_))) {
            entries.pop();
        }
        // newcomers keep the header they have in `next`, at the end.
        let mut header: Option<&String> = None;
        let mut opened: Option<&String> = None;
        for e in &next.entries {
            match e {
                Entry::Header(h) => header = Some(h),
                Entry::Row(id) if !here.contains(id) => {
                    if opened != header
                        && let Some(h) = header
                    {
                        entries.push(Entry::Header(h.clone()));
                        opened = header;
                    }
                    entries.push(Entry::Row(*id));
                }
                Entry::Sub(id, _) if !here.contains(id) => entries.push(e.clone()),
                Entry::Row(_) | Entry::Sub(..) => {}
            }
        }
        SidebarLayout { entries }
    }
}

// ---- the overview grid, as arithmetic ------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

pub const CARD_MIN_W: f32 = 264.0;
pub const CARD_H: f32 = 112.0;
pub const CARD_GAP: f32 = 16.0;
pub const GRID_MARGIN: f32 = 24.0;

/// where `count` cards go in an area `width` wide starting at `top`: as many
/// columns as fit at the minimum card width, stretched to fill the row.
pub fn grid(count: usize, width: f32, top: f32) -> Vec<Rect> {
    let usable = (width - 2.0 * GRID_MARGIN).max(CARD_MIN_W);
    let cols = (((usable + CARD_GAP) / (CARD_MIN_W + CARD_GAP)).floor() as usize).max(1);
    let card_w = (usable - CARD_GAP * (cols as f32 - 1.0)) / cols as f32;
    (0..count)
        .map(|i| Rect {
            x: GRID_MARGIN + (i % cols) as f32 * (card_w + CARD_GAP),
            y: top + (i / cols) as f32 * (CARD_H + CARD_GAP),
            w: card_w,
            h: CARD_H,
        })
        .collect()
}

/// the card to move to from `from` with an arrow key, in a grid of `count`
/// cards and `cols` columns. stays put at the edges; never leaves the grid.
pub fn grid_step(from: usize, count: usize, cols: usize, dx: i32, dy: i32) -> usize {
    if count == 0 {
        return 0;
    }
    let cols = cols.max(1) as i32;
    let (col, row) = (from as i32 % cols, from as i32 / cols);
    let (ncol, nrow) = (col + dx, row + dy);
    if ncol < 0 || ncol >= cols || nrow < 0 {
        return from.min(count - 1);
    }
    let to = nrow * cols + ncol;
    if to as usize >= count { from.min(count - 1) } else { to as usize }
}

/// how many columns `grid` used for this width.
pub fn grid_cols(width: f32) -> usize {
    let usable = (width - 2.0 * GRID_MARGIN).max(CARD_MIN_W);
    (((usable + CARD_GAP) / (CARD_MIN_W + CARD_GAP)).floor() as usize).max(1)
}

/// "3s", "4m", "2h", "5d".
pub fn ago(since: Millis, now: Millis) -> String {
    let s = now.saturating_sub(since) / 1000;
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86_399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessel_proto::{Attention, RunState};

    fn session(n: u128, attention: Option<(AttentionReason, Millis)>) -> SessionSummary {
        SessionSummary {
            id: SessionId(uuid::Uuid::from_u128(n)),
            goal: format!("s{n}"),
            cwd: "/".into(),
            worktree: None,
            parent: None,
            run_state: RunState::Idle,
            activity: None,
            activity_since_ms: None,
            attention: attention.map(|(reason, since_ms)| Attention { reason, since_ms, seq: 1 }),
            warm: false,
            background_tasks: 0,
            model: None,
            permission_mode: None,
            effort: None,
            fast: false,
            context_used: None,
            context_window: None,
            tasks: vec![], mcp: vec![], linked_dirs: vec![],
            cost_usd: 0.0,
            trace: vec![],
            asking: None,
            last_seq: 1,
            created_ms: n as u64,
            updated_ms: 0,
        }
    }

    fn id(n: u128) -> SessionId {
        SessionId(uuid::Uuid::from_u128(n))
    }

    fn text(id: &str, order: u64, seq: u64, t: &str, streaming: bool) -> Item {
        Item { id: ItemId::from(id), parent: None, seq, order, created_ms: 0, body: ItemBody::Text { text: t.into(), streaming } }
    }

    trait TapSeq {
        fn tap_seq(self, seq: u64) -> Self;
    }
    impl TapSeq for Item {
        fn tap_seq(mut self, seq: u64) -> Self {
            self.seq = seq;
            self
        }
    }

    fn open(items: Vec<Item>) -> Open {
        let summary = session(1, None);
        Open::from_snapshot(SessionSnapshot { summary, items, has_more: false, pending: vec![] })
    }

    #[test]
    fn the_queue_puts_blocked_first_then_longest_waiting() {
        let mut m = Model::default();
        m.set_sessions(vec![
            session(1, Some((AttentionReason::Finished, 100))),
            session(2, None),
            session(3, Some((AttentionReason::NeedsPermission, 900))),
            session(4, Some((AttentionReason::Errored, 50))),
            session(5, Some((AttentionReason::NeedsInput, 800))),
        ]);
        let order: Vec<_> = m.attention_queue().iter().map(|s| s.goal.as_str()).collect();
        assert_eq!(order, ["s5", "s3", "s4", "s1"], "blocked (oldest first), then the rest (oldest first)");
    }

    #[test]
    fn next_attention_walks_the_queue_and_wraps() {
        let mut m = Model::default();
        assert_eq!(m.next_attention(None), None);
        m.set_sessions(vec![
            session(1, Some((AttentionReason::Finished, 100))),
            session(2, None),
            session(3, Some((AttentionReason::NeedsPermission, 900))),
        ]);
        assert_eq!(m.next_attention(None), Some(id(3)));
        assert_eq!(m.next_attention(Some(id(2))), Some(id(3)), "from a session that isn't waiting: the front of the queue");
        assert_eq!(m.next_attention(Some(id(3))), Some(id(1)));
        assert_eq!(m.next_attention(Some(id(1))), Some(id(3)), "wraps");
        m.set_sessions(vec![session(1, Some((AttentionReason::Finished, 100)))]);
        assert_eq!(m.next_attention(Some(id(1))), None, "nothing else is waiting");
    }

    #[test]
    fn update_reports_only_new_reasons_to_look() {
        let mut m = Model::default();
        assert!(!m.update_session(session(1, None)));
        assert!(m.update_session(session(1, Some((AttentionReason::Finished, 5)))), "newly wants you");
        assert!(!m.update_session(session(1, Some((AttentionReason::Finished, 5)))), "same reason again is not news");
        assert!(m.update_session(session(1, Some((AttentionReason::NeedsPermission, 9)))), "a different reason is");
        assert!(!m.update_session(session(1, None)));
        assert!(m.update_session(session(2, Some((AttentionReason::Errored, 1)))), "a session first seen already wanting you");
        assert_eq!(m.sessions.len(), 2);
    }

    #[test]
    fn removing_the_open_session_closes_it() {
        let mut m = Model::default();
        m.set_sessions(vec![session(1, None), session(2, None)]);
        m.open = Some(open(vec![]));
        m.remove_session(id(2));
        assert!(m.open.is_some());
        m.remove_session(id(1));
        assert!(m.open.is_none());
        assert!(m.sessions.is_empty());
    }

    #[test]
    fn upserts_append_update_and_never_go_backwards() {
        let mut o = open(vec![text("a", 1, 1, "one", false)]);
        assert_eq!(o.upsert(text("b", 2, 2, "", true)), Change::Appended(1));
        assert_eq!(o.upsert(text("b", 2, 5, "done", false)), Change::Row(1));
        assert_eq!(o.upsert(text("b", 2, 3, "stale", true)), Change::None, "an older version must not overwrite a newer one");
        assert!(matches!(&o.row(1).unwrap().0.body, ItemBody::Text { text, streaming: false } if text == "done"));
        // out of order arrival is placed correctly rather than corrupting the list.
        assert_eq!(o.upsert(text("z", 0, 9, "first", false)), Change::Reset);
        assert_eq!(o.shown(), [("z", 0), ("a", 0), ("b", 0)]);
        assert_eq!(o.upsert(text("a", 1, 10, "one!", false)), Change::Row(1), "the index survives a reorder");
    }

    #[test]
    fn deltas_apply_only_where_they_belong() {
        let mut o = open(vec![text("a", 1, 1, "", true), text("b", 2, 2, "done", false)]);
        assert_eq!(o.delta(&ItemId::from("a"), 0, "hel"), Change::Row(0));
        assert_eq!(o.delta(&ItemId::from("a"), 3, "lo"), Change::Row(0));
        assert_eq!(o.delta(&ItemId::from("a"), 3, "lo"), Change::None, "a repeat is not applied twice");
        assert_eq!(o.delta(&ItemId::from("a"), 99, "?"), Change::None, "a gap is not papered over");
        assert_eq!(o.delta(&ItemId::from("b"), 4, "!"), Change::None, "a finished item does not grow");
        assert_eq!(o.delta(&ItemId::from("nope"), 0, "x"), Change::None);
        assert!(matches!(&o.row(0).unwrap().0.body, ItemBody::Text { text, .. } if text == "hello"));
    }

    #[test]
    fn a_snapshot_is_put_in_transcript_order() {
        let o = open(vec![text("b", 5, 5, "", false), text("a", 2, 9, "", false)]);
        assert_eq!(o.shown(), [("a", 0), ("b", 0)]);
    }

    fn child(id: &str, parent: &str, order: u64) -> Item {
        Item { parent: Some(ItemId::from(parent)), ..text(id, order, order, id, false) }
    }

    #[test]
    fn work_inside_a_call_sits_under_that_call_even_when_two_run_at_once() {
        // two subagents started together; their steps arrive interleaved.
        let mut o = open(vec![text("user", 1, 1, "go", false), text("agent-a", 2, 2, "", false), text("agent-b", 3, 3, "", false)]);
        assert_eq!(o.upsert(child("a1", "agent-a", 4)), Change::Reset, "it lands in the middle, not at the end");
        assert_eq!(o.upsert(child("b1", "agent-b", 5)), Change::Appended(1), "under the last call is still the end");
        assert_eq!(o.upsert(child("a2", "agent-a", 6)), Change::Reset);
        assert_eq!(o.upsert(child("b2", "agent-b", 7)), Change::Appended(1));
        assert_eq!(o.upsert(text("done", 8, 8, "done", false)), Change::Appended(1));
        assert_eq!(
            o.shown(),
            [("user", 0), ("agent-a", 0), ("a1", 1), ("a2", 1), ("agent-b", 0), ("b1", 1), ("b2", 1), ("done", 0)],
            "each subagent's steps are together, in order, under it"
        );
        // changing an item reports the row it is shown on, not where it arrived.
        assert_eq!(o.upsert(child("b1", "agent-b", 5).tap_seq(99)), Change::Row(5));
        assert_eq!(o.len(), 8);
    }

    #[test]
    fn nesting_goes_as_deep_as_the_work_does_and_orphans_are_not_lost() {
        let o = open(vec![
            text("top", 1, 1, "", false),
            child("mid", "top", 2),
            child("leaf", "mid", 3),
            child("orphan", "a-call-older-than-what-is-loaded", 4),
            child("selfish", "selfish", 5),
        ]);
        assert_eq!(o.shown(), [("top", 0), ("mid", 1), ("leaf", 2), ("orphan", 0), ("selfish", 0)]);
        assert!(o.row(99).is_none());
    }

    #[test]
    fn a_streaming_reply_inside_a_subagent_updates_its_own_row() {
        let mut o = open(vec![text("agent-a", 1, 1, "", false), text("agent-b", 2, 2, "", false)]);
        let mut stream = child("a-says", "agent-a", 3);
        stream.body = ItemBody::Text { text: String::new(), streaming: true };
        o.upsert(stream);
        assert_eq!(o.shown(), [("agent-a", 0), ("a-says", 1), ("agent-b", 0)]);
        assert_eq!(o.delta(&ItemId::from("a-says"), 0, "hi"), Change::Row(1));
    }

    /// a session in a workspace of its own, a worktree of the project's repository.
    fn in_project(n: u128, cwd: &str, attention: Option<(AttentionReason, Millis)>) -> SessionSummary {
        let worktree = Some(tessel_proto::WorktreeInfo { repo: cwd.into(), branch: format!("tessel/s{n}") });
        SessionSummary { cwd: format!("{cwd}-wt{n}"), worktree, ..session(n, attention) }
    }

    #[test]
    fn a_workspace_is_one_row_standing_for_the_tab_that_wants_you_or_the_latest() {
        let tab = |n: u128, updated: Millis, attention: Option<(AttentionReason, Millis)>| SessionSummary { cwd: "/w/api".into(), updated_ms: updated, ..session(n, attention) };
        let names = |l: &SidebarLayout| l.entries.iter().map(|e| match e { Entry::Header(h) => format!("[{h}]"), Entry::Row(id) => id.0.as_u128().to_string(), Entry::Sub(..) => "sub".into() }).collect::<Vec<_>>();
        assert_eq!(names(&layout(vec![tab(1, 10, None), tab(2, 30, None), tab(3, 20, None)])), ["[api]", "2"], "the one busy most recently");
        assert_eq!(names(&layout(vec![tab(1, 10, Some((AttentionReason::NeedsPermission, 5))), tab(2, 30, None)])), [format!("[{QUEUE_TITLE}]"), "1".into()], "the one that wants you");
    }

    fn layout(sessions: Vec<SessionSummary>) -> SidebarLayout {
        let mut m = Model::default();
        m.set_sessions(sessions);
        SidebarLayout::build(&m)
    }

    fn shape(l: &SidebarLayout) -> Vec<String> {
        l.entries
            .iter()
            .map(|e| match e {
                Entry::Header(h) => format!("[{h}]"),
                Entry::Row(id) => format!("{}", id.0.as_u128()),
                Entry::Sub(id, task) => format!("{}/{task}", id.0.as_u128()),
            })
            .collect()
    }

    #[test]
    fn the_sidebar_is_the_queue_then_projects_and_each_session_appears_once() {
        let l = layout(vec![
            in_project(1, "/work/api", None),
            in_project(2, "/work/api", Some((AttentionReason::Finished, 50))),
            in_project(3, "/work/web/", None),
            in_project(4, "/work/api", None),
            in_project(5, "/work/web", Some((AttentionReason::NeedsPermission, 900))),
        ]);
        assert_eq!(shape(&l), ["[Needs You]", "5", "2", "[api]", "4", "1", "[web]", "3"], "blocked first; projects by name; newest first within");
        assert_eq!(l.rows().count(), 5);
        assert_eq!(l.height(), 3.0 * HEADER_H + 5.0 * ROW_H);
        let tops = l.row_tops();
        assert_eq!(tops[&id(5)], HEADER_H);
        assert_eq!(tops[&id(2)], HEADER_H + ROW_H);
        assert_eq!(tops[&id(4)], 2.0 * HEADER_H + 2.0 * ROW_H);
    }

    #[test]
    fn no_queue_header_when_nothing_needs_you_and_a_worktree_groups_under_its_repo() {
        let mut wt = in_project(2, "/Users/me/tessel/workspaces/api/fix-login-a1b2c", None);
        wt.worktree = Some(tessel_proto::WorktreeInfo { repo: "/work/api".into(), branch: "tessel/fix-login".into() });
        let l = layout(vec![in_project(1, "/work/api", None), wt]);
        assert_eq!(shape(&l), ["[api]", "2", "1"], "the worktree session sits with its repository, not under its own folder");
        assert_eq!(shape(&layout(vec![])), Vec::<String>::new());
        assert_eq!(project_name(&in_project(1, "/", None)), "/");
    }

    #[test]
    fn a_session_that_starts_needing_you_travels_up_and_the_rest_make_room() {
        let before = layout(vec![in_project(1, "/w/api", None), in_project(2, "/w/api", None), in_project(3, "/w/api", None)]);
        assert_eq!(shape(&before), ["[api]", "3", "2", "1"]);
        let after = layout(vec![in_project(1, "/w/api", Some((AttentionReason::NeedsPermission, 10))), in_project(2, "/w/api", None), in_project(3, "/w/api", None)]);
        assert_eq!(shape(&after), ["[Needs You]", "1", "[api]", "3", "2"]);

        let moves: HashMap<_, _> = before.moves_to(&after).into_iter().collect();
        // session 1 was the third row under one header; now it is the first row under one header.
        assert_eq!(moves[&id(1)], 2.0 * ROW_H, "it appears to start two rows lower, where it was");
        // the others were pushed down by the new header and row.
        assert_eq!(moves[&id(3)], -(HEADER_H + ROW_H));
        assert_eq!(moves[&id(2)], -(HEADER_H + ROW_H));

        // and back again is exactly the reverse.
        let back: HashMap<_, _> = after.moves_to(&before).into_iter().collect();
        assert_eq!(back[&id(1)], -2.0 * ROW_H);
        assert_eq!(back[&id(3)], HEADER_H + ROW_H);
    }

    #[test]
    fn rows_that_stay_put_do_not_move_and_new_rows_enter_from_just_below() {
        let before = layout(vec![in_project(1, "/w/api", None), in_project(2, "/w/web", None)]);
        let after = layout(vec![in_project(1, "/w/api", None), in_project(2, "/w/web", None), in_project(3, "/w/web", None)]);
        assert_eq!(shape(&after), ["[api]", "1", "[web]", "3", "2"]);
        let moves: HashMap<_, _> = before.moves_to(&after).into_iter().collect();
        assert!(!moves.contains_key(&id(1)), "nothing above it changed");
        assert_eq!(moves[&id(3)], ENTER_FROM);
        assert_eq!(moves[&id(2)], -ROW_H);
        assert!(before.moves_to(&before).is_empty());
    }

    #[test]
    fn under_a_resting_pointer_rows_keep_their_places() {
        let shown = layout(vec![in_project(1, "/w/api", None), in_project(2, "/w/api", None), in_project(3, "/w/web", None)]);
        assert_eq!(shape(&shown), ["[api]", "2", "1", "[web]", "3"]);
        // meanwhile: 1 now needs you, 3 was deleted, 4 is new and needs you, 5 is new.
        let real = layout(vec![
            in_project(1, "/w/api", Some((AttentionReason::NeedsPermission, 10))),
            in_project(2, "/w/api", None),
            in_project(4, "/w/api", Some((AttentionReason::Finished, 20))),
            in_project(5, "/w/ops", None),
        ]);
        assert_eq!(shape(&real), ["[Needs You]", "1", "4", "[api]", "2", "[ops]", "5"]);

        let held = shown.held(&real);
        assert_eq!(shape(&held), ["[api]", "2", "1", "[Needs You]", "4", "[ops]", "5"], "existing rows did not move; the deleted one and its empty header are gone; newcomers are at the end");
        assert!(shown.moves_to(&held).iter().all(|(sid, _)| *sid == id(4) || *sid == id(5)), "only newcomers animate while held");
        assert_eq!(held.rows().count(), real.rows().count(), "nothing is hidden, only unsorted");

        // once the pointer leaves, everything settles into the real order.
        let moves: HashMap<_, _> = held.moves_to(&real).into_iter().collect();
        assert_eq!(moves[&id(1)], (HEADER_H + ROW_H) - HEADER_H, "1 travels up into the queue");
        assert!(moves.contains_key(&id(4)) && moves.contains_key(&id(2)));
    }

    #[test]
    fn holding_with_nothing_changed_changes_nothing() {
        let shown = layout(vec![in_project(1, "/w/api", Some((AttentionReason::Finished, 1))), in_project(2, "/w/api", None)]);
        assert_eq!(shown.held(&shown), shown);
        assert_eq!(layout(vec![]).held(&shown), shown, "from empty, everything is a newcomer in its real order");
        assert_eq!(shown.held(&layout(vec![])), layout(vec![]));
    }

    #[test]
    fn the_grid_fills_the_width_and_wraps() {
        let cells = grid(7, 1180.0, 68.0);
        let cols = grid_cols(1180.0);
        assert_eq!(cols, 4, "four 264-wide cards fit in 1180 with margins and gaps");
        assert_eq!(cells.len(), 7);
        // the row is filled edge to edge, margins aside.
        assert!((cells[0].x - GRID_MARGIN).abs() < 0.01);
        assert!((cells[3].x + cells[3].w - (1180.0 - GRID_MARGIN)).abs() < 0.01);
        assert!(cells[0].w >= CARD_MIN_W);
        // the fifth card starts the second row, directly under the first.
        assert_eq!((cells[4].x, cells[4].y), (cells[0].x, cells[0].y + CARD_H + CARD_GAP));
        // cards never overlap.
        for (i, a) in cells.iter().enumerate() {
            for b in &cells[i + 1..] {
                let apart = a.x + a.w <= b.x + 0.01 || b.x + b.w <= a.x + 0.01 || a.y + a.h <= b.y + 0.01 || b.y + b.h <= a.y + 0.01;
                assert!(apart, "{a:?} overlaps {b:?}");
            }
        }
        // a window too narrow for one card still gets one column, not zero.
        assert_eq!(grid_cols(120.0), 1);
        assert_eq!(grid(3, 120.0, 0.0).len(), 3);
        assert!(grid(0, 800.0, 0.0).is_empty());
    }

    #[test]
    fn arrow_keys_move_within_the_grid_and_stop_at_its_edges() {
        // 7 cards, 4 columns:  0 1 2 3 / 4 5 6
        let go = |from, dx, dy| grid_step(from, 7, 4, dx, dy);
        assert_eq!(go(0, 1, 0), 1);
        assert_eq!(go(3, 1, 0), 3, "right edge");
        assert_eq!(go(0, -1, 0), 0, "left edge");
        assert_eq!(go(1, 0, 1), 5);
        assert_eq!(go(3, 0, 1), 3, "nothing below the last column of a short row");
        assert_eq!(go(5, 0, -1), 1);
        assert_eq!(go(1, 0, -1), 1, "top edge");
        assert_eq!(go(6, 1, 0), 6, "the last card is the end");
        assert_eq!(grid_step(0, 0, 4, 1, 0), 0, "an empty grid has nowhere to go");
        assert_eq!(grid_step(99, 3, 4, 0, 0), 2, "a stale position is brought back inside");
    }

    #[test]
    fn ago_reads_naturally() {
        assert_eq!(ago(0, 4_000), "4s");
        assert_eq!(ago(0, 59_999), "59s");
        assert_eq!(ago(0, 60_000), "1m");
        assert_eq!(ago(0, 20 * 60_000), "20m");
        assert_eq!(ago(0, 3 * 3_600_000), "3h");
        assert_eq!(ago(0, 2 * 86_400_000), "2d");
        assert_eq!(ago(5_000, 1_000), "0s", "a clock that went backwards is not a panic");
    }

    fn with_tasks(mut s: SessionSummary, tasks: &[&str]) -> SessionSummary {
        s.tasks = tasks.iter().map(|t| tessel_proto::TaskSummary { id: (*t).into(), description: format!("do {t}"), background: false, item: None }).collect();
        s
    }

    #[test]
    fn what_runs_inside_a_session_is_listed_under_it_and_makes_room() {
        let before = layout(vec![in_project(1, "/w/api", None), in_project(2, "/w/api", None)]);
        let after = layout(vec![in_project(1, "/w/api", None), with_tasks(in_project(2, "/w/api", None), &["a", "b"])]);
        assert_eq!(shape(&after), ["[api]", "2", "2/a", "2/b", "1"]);
        assert_eq!(after.height(), HEADER_H + 2.0 * ROW_H + 2.0 * SUB_H);
        // the session below slides down by exactly the two new lines.
        let moves: HashMap<_, _> = before.moves_to(&after).into_iter().collect();
        assert_eq!(moves[&id(1)], -2.0 * SUB_H);
        assert!(!moves.contains_key(&id(2)));
    }

    #[test]
    fn a_crowd_of_subagents_is_capped_and_nothing_shifts_under_the_pointer() {
        let many: Vec<String> = (0..20).map(|i| format!("t{i}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        let crowded = layout(vec![with_tasks(in_project(1, "/w/api", None), &many)]);
        assert_eq!(crowded.entries.iter().filter(|e| matches!(e, Entry::Sub(..))).count(), SUBS_SHOWN);

        let shown = layout(vec![in_project(1, "/w/api", None), in_project(2, "/w/api", None)]);
        let real = layout(vec![in_project(1, "/w/api", None), with_tasks(in_project(2, "/w/api", None), &["a"])]);
        assert_eq!(shown.held(&real), shown, "a subagent starting must not push rows out from under the cursor");
    }
}
