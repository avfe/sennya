//! Workspace model shared by the daemon (owner, persists it) and the GUI (edits it).
//!
//! A workspace is a list of pages. Every terminal session lives on exactly one page,
//! inside that page's tiling tree. The "tabs" view mode shows one session of a page
//! full-size, the "grid" mode shows the whole tiling tree.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

pub type SessionId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dir {
    /// Children side by side.
    Horizontal,
    /// Children stacked top to bottom.
    Vertical,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Node {
    Leaf(SessionId),
    Split { id: u64, dir: Dir, children: Vec<Node>, ratios: Vec<f32> },
}

/// How a page places new terminals.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Arrange {
    /// Re-tile everything as an even grid whenever a terminal is added or removed.
    Grid,
    Columns,
    Rows,
    /// First terminal big on the left, the rest stacked on the right.
    MainStack,
    /// User-arranged: new terminals split the focused one or go into the page's zone.
    Manual,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
    Center,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Page {
    pub id: u64,
    pub name: String,
    pub root: Option<Node>,
    pub arrange: Arrange,
    /// Split container that collects new terminals (an "area" the user designated).
    pub zone: Option<u64>,
    pub focused: Option<SessionId>,
    pub zoomed: Option<SessionId>,
    /// Terminals of this page that keep running but are not shown in the grid.
    #[serde(default)]
    pub hidden: Vec<SessionId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViewMode {
    Tabs,
    Grid,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub pages: Vec<Page>,
    pub active_page: usize,
    pub mode: ViewMode,
    pub sidebar: bool,
    /// User-given terminal names.
    pub names: BTreeMap<SessionId, String>,
    pub next_id: u64,
}

fn normalize(ratios: &mut [f32]) {
    let sum: f32 = ratios.iter().sum();
    if sum <= f32::EPSILON || !sum.is_finite() {
        let n = ratios.len() as f32;
        ratios.iter_mut().for_each(|r| *r = 1.0 / n);
    } else {
        ratios.iter_mut().for_each(|r| *r /= sum);
    }
}

impl Node {
    pub fn leaves(&self, out: &mut Vec<SessionId>) {
        match self {
            Node::Leaf(id) => out.push(*id),
            Node::Split { children, .. } => children.iter().for_each(|c| c.leaves(out)),
        }
    }

    pub fn leaf_ids(&self) -> Vec<SessionId> {
        let mut out = Vec::new();
        self.leaves(&mut out);
        out
    }

    pub fn contains(&self, sid: SessionId) -> bool {
        match self {
            Node::Leaf(id) => *id == sid,
            Node::Split { children, .. } => children.iter().any(|c| c.contains(sid)),
        }
    }

    /// Remove a leaf, collapsing splits that end up with a single child.
    pub fn remove(self, sid: SessionId) -> Option<Node> {
        match self {
            Node::Leaf(id) if id == sid => None,
            leaf @ Node::Leaf(_) => Some(leaf),
            Node::Split { id, dir, children, ratios } => {
                let mut kept = Vec::with_capacity(children.len());
                let mut kept_ratios = Vec::with_capacity(children.len());
                for (child, ratio) in children.into_iter().zip(ratios) {
                    if let Some(child) = child.remove(sid) {
                        kept.push(child);
                        kept_ratios.push(ratio);
                    }
                }
                match kept.len() {
                    0 => None,
                    1 => kept.pop(),
                    _ => {
                        normalize(&mut kept_ratios);
                        Some(Node::Split { id, dir, children: kept, ratios: kept_ratios })
                    }
                }
            }
        }
    }

    /// Put `new` next to `target`. Reuses the parent split when it already runs in `dir`.
    pub fn insert_at(
        &mut self,
        target: SessionId,
        new: SessionId,
        dir: Dir,
        after: bool,
        next_id: &mut u64,
    ) -> bool {
        match self {
            Node::Leaf(id) if *id == target => {
                let (a, b) = if after {
                    (Node::Leaf(target), Node::Leaf(new))
                } else {
                    (Node::Leaf(new), Node::Leaf(target))
                };
                *next_id += 1;
                *self = Node::Split { id: *next_id, dir, children: vec![a, b], ratios: vec![0.5, 0.5] };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { dir: d, children, ratios, .. } => {
                if *d == dir {
                    if let Some(i) =
                        children.iter().position(|c| matches!(c, Node::Leaf(id) if *id == target))
                    {
                        let half = ratios[i] / 2.0;
                        ratios[i] = half;
                        let at = if after { i + 1 } else { i };
                        children.insert(at, Node::Leaf(new));
                        ratios.insert(at, half);
                        return true;
                    }
                }
                children.iter_mut().any(|c| c.insert_at(target, new, dir, after, next_id))
            }
        }
    }

    /// Append to a split container and give all of its children equal space.
    pub fn append_to_split(&mut self, split_id: u64, new: SessionId) -> bool {
        match self {
            Node::Leaf(_) => false,
            Node::Split { id, children, ratios, .. } => {
                if *id == split_id {
                    children.push(Node::Leaf(new));
                    ratios.push(0.0);
                    let n = ratios.len() as f32;
                    ratios.iter_mut().for_each(|r| *r = 1.0 / n);
                    true
                } else {
                    children.iter_mut().any(|c| c.append_to_split(split_id, new))
                }
            }
        }
    }

    pub fn has_split(&self, split_id: u64) -> bool {
        match self {
            Node::Leaf(_) => false,
            Node::Split { id, children, .. } => {
                *id == split_id || children.iter().any(|c| c.has_split(split_id))
            }
        }
    }

    /// Id of the split that directly contains the leaf `sid`.
    pub fn parent_split(&self, sid: SessionId) -> Option<u64> {
        match self {
            Node::Leaf(_) => None,
            Node::Split { id, children, .. } => {
                if children.iter().any(|c| matches!(c, Node::Leaf(l) if *l == sid)) {
                    Some(*id)
                } else {
                    children.iter().find_map(|c| c.parent_split(sid))
                }
            }
        }
    }

    pub fn map_leaves(&mut self, f: &mut impl FnMut(SessionId) -> SessionId) {
        match self {
            Node::Leaf(id) => *id = f(*id),
            Node::Split { children, .. } => children.iter_mut().for_each(|c| c.map_leaves(f)),
        }
    }

    /// Adjust the boundary between child `index` and `index + 1` of split `split_id`.
    pub fn resize_split(&mut self, split_id: u64, index: usize, delta: f32, min: f32) -> bool {
        match self {
            Node::Leaf(_) => false,
            Node::Split { id, children, ratios, .. } => {
                if *id == split_id {
                    if index + 1 >= ratios.len() {
                        return false;
                    }
                    let pair = ratios[index] + ratios[index + 1];
                    let min = min.min(pair / 2.0);
                    let a = (ratios[index] + delta).clamp(min, pair - min);
                    ratios[index] = a;
                    ratios[index + 1] = pair - a;
                    true
                } else {
                    children.iter_mut().any(|c| c.resize_split(split_id, index, delta, min))
                }
            }
        }
    }

    /// Merge nested splits that run in the same direction.
    pub fn flatten(self) -> Node {
        match self {
            leaf @ Node::Leaf(_) => leaf,
            Node::Split { id, dir, children, ratios } => {
                let mut out_children = Vec::new();
                let mut out_ratios = Vec::new();
                for (child, ratio) in children.into_iter().zip(ratios) {
                    match child.flatten() {
                        Node::Split { dir: d, children: cc, ratios: cr, .. } if d == dir => {
                            for (c, r) in cc.into_iter().zip(cr) {
                                out_children.push(c);
                                out_ratios.push(r * ratio);
                            }
                        }
                        other => {
                            out_children.push(other);
                            out_ratios.push(ratio);
                        }
                    }
                }
                normalize(&mut out_ratios);
                Node::Split { id, dir, children: out_children, ratios: out_ratios }
            }
        }
    }
}

fn even_split(dir: Dir, nodes: Vec<Node>, next_id: &mut u64) -> Node {
    if nodes.len() == 1 {
        return nodes.into_iter().next().unwrap();
    }
    *next_id += 1;
    let n = nodes.len();
    Node::Split { id: *next_id, dir, children: nodes, ratios: vec![1.0 / n as f32; n] }
}

/// Build a fresh tiling tree for `ids`.
pub fn build_layout(arrange: Arrange, ids: &[SessionId], next_id: &mut u64) -> Option<Node> {
    if ids.is_empty() {
        return None;
    }
    let leaves = || ids.iter().map(|id| Node::Leaf(*id)).collect::<Vec<_>>();
    Some(match arrange {
        Arrange::Columns => even_split(Dir::Horizontal, leaves(), next_id),
        Arrange::Rows => even_split(Dir::Vertical, leaves(), next_id),
        Arrange::MainStack if ids.len() > 1 => {
            let stack = even_split(Dir::Vertical, leaves()[1..].to_vec(), next_id);
            *next_id += 1;
            Node::Split {
                id: *next_id,
                dir: Dir::Horizontal,
                children: vec![Node::Leaf(ids[0]), stack],
                ratios: vec![0.6, 0.4],
            }
        }
        _ => {
            let n = ids.len();
            let cols = (n as f32).sqrt().ceil() as usize;
            let rows: Vec<Node> = ids
                .chunks(cols)
                .map(|row| even_split(Dir::Horizontal, row.iter().map(|id| Node::Leaf(*id)).collect(), next_id))
                .collect();
            even_split(Dir::Vertical, rows, next_id)
        }
    })
}

impl Page {
    pub fn new(id: u64, name: impl Into<String>, arrange: Arrange) -> Page {
        Page { id, name: name.into(), root: None, arrange, zone: None, focused: None, zoomed: None, hidden: Vec::new() }
    }

    /// Terminals shown in the grid, in layout order.
    pub fn tiled(&self) -> Vec<SessionId> {
        self.root.as_ref().map(Node::leaf_ids).unwrap_or_default()
    }

    /// Every terminal of the page: shown ones first, then hidden ones.
    pub fn sessions(&self) -> Vec<SessionId> {
        let mut v = self.tiled();
        v.extend(self.hidden.iter().copied());
        v
    }

    pub fn tiles(&self, sid: SessionId) -> bool {
        self.root.as_ref().is_some_and(|r| r.contains(sid))
    }

    pub fn contains(&self, sid: SessionId) -> bool {
        self.tiles(sid) || self.hidden.contains(&sid)
    }

    pub fn is_empty(&self) -> bool {
        self.root.is_none() && self.hidden.is_empty()
    }

    fn rebuild(&mut self, ids: &[SessionId], next_id: &mut u64) {
        self.root = build_layout(self.arrange, ids, next_id);
        self.zone = None;
    }

    fn remove(&mut self, sid: SessionId, next_id: &mut u64) -> bool {
        if self.hidden.contains(&sid) {
            self.hidden.retain(|s| *s != sid);
            if self.focused == Some(sid) {
                self.focused = self.tiled().first().copied();
            }
            return true;
        }
        if !self.tiles(sid) {
            return false;
        }
        let before = self.tiled();
        let pos = before.iter().position(|s| *s == sid).unwrap_or(0);
        self.root = self.root.take().and_then(|r| r.remove(sid)).map(Node::flatten);
        let after = self.tiled();
        if self.arrange != Arrange::Manual {
            self.rebuild(&after, next_id);
        } else if let Some(zone) = self.zone {
            if !self.root.as_ref().is_some_and(|r| r.has_split(zone)) {
                self.zone = None;
            }
        }
        if self.focused == Some(sid) || self.focused.is_none() {
            self.focused = after.get(pos.saturating_sub(1).min(after.len().saturating_sub(1))).copied();
        }
        if self.zoomed == Some(sid) {
            self.zoomed = None;
        }
        true
    }
}

impl Workspace {
    pub fn new(mode: ViewMode) -> Workspace {
        Workspace {
            pages: vec![Page::new(1, "Main", Arrange::Grid)],
            active_page: 0,
            mode,
            sidebar: true,
            names: BTreeMap::new(),
            next_id: 1,
        }
    }

    pub fn alloc_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn page(&self) -> &Page {
        &self.pages[self.active_page.min(self.pages.len() - 1)]
    }

    pub fn page_mut(&mut self) -> &mut Page {
        let i = self.active_page.min(self.pages.len() - 1);
        &mut self.pages[i]
    }

    pub fn all_sessions(&self) -> Vec<SessionId> {
        self.pages.iter().flat_map(|p| p.sessions()).collect()
    }

    pub fn page_of(&self, sid: SessionId) -> Option<usize> {
        self.pages.iter().position(|p| p.contains(sid))
    }

    /// The terminal that has keyboard focus.
    pub fn focused(&self) -> Option<SessionId> {
        let page = self.page();
        page.focused.filter(|f| page.contains(*f)).or_else(|| page.sessions().first().copied())
    }

    /// Main + stack with `main` as the big pane and `new` added to the stack.
    pub fn stack_beside(&mut self, main: SessionId, new: SessionId) {
        let Some(pi) = self.page_of(main) else { return };
        let mut next_id = self.next_id;
        let page = &mut self.pages[pi];
        page.hidden.retain(|s| *s != main);
        let mut ids = vec![main];
        ids.extend(page.tiled().into_iter().filter(|s| *s != main && *s != new));
        ids.push(new);
        page.arrange = Arrange::MainStack;
        page.rebuild(&ids, &mut next_id);
        page.focused = Some(new);
        page.zoomed = None;
        self.next_id = next_id;
        self.active_page = pi;
    }

    /// Take a terminal out of the grid; it keeps running and stays on its page.
    pub fn hide(&mut self, sid: SessionId) {
        let Some(pi) = self.page_of(sid) else { return };
        let mut next_id = self.next_id;
        let page = &mut self.pages[pi];
        if page.hidden.contains(&sid) {
            return;
        }
        page.remove(sid, &mut next_id);
        page.hidden.push(sid);
        if page.focused == Some(sid) || page.focused.is_none() {
            page.focused = page.tiled().first().copied();
        }
        self.next_id = next_id;
    }

    /// Put a hidden terminal back into the grid and focus it.
    pub fn unhide(&mut self, sid: SessionId) {
        let Some(pi) = self.page_of(sid) else { return };
        if !self.pages[pi].hidden.contains(&sid) {
            return;
        }
        self.pages[pi].hidden.retain(|s| *s != sid);
        self.add_session(sid, pi, None, Dir::Horizontal);
    }

    pub fn focus(&mut self, sid: SessionId) {
        if let Some(i) = self.page_of(sid) {
            self.active_page = i;
            self.pages[i].focused = Some(sid);
            if self.pages[i].zoomed.is_some_and(|z| z != sid) {
                self.pages[i].zoomed = None;
            }
        }
    }

    /// Place a new session on a page.
    ///
    /// `split` forces a manual split of `near` (or the focused terminal) in that direction;
    /// otherwise the page's arrangement decides, with `hint` as the split direction for
    /// manual pages.
    pub fn add_session(&mut self, sid: SessionId, page_idx: usize, split: Option<Dir>, hint: Dir) {
        let page_idx = page_idx.min(self.pages.len() - 1);
        let mut next_id = self.next_id;
        let page = &mut self.pages[page_idx];
        let target = page.focused.filter(|f| page.tiles(*f));
        if page.root.is_none() {
            page.root = Some(Node::Leaf(sid));
        } else if let Some(dir) = split {
            let target = target.unwrap_or_else(|| page.tiled()[0]);
            page.root.as_mut().unwrap().insert_at(target, sid, dir, true, &mut next_id);
            page.arrange = Arrange::Manual;
        } else if page.arrange != Arrange::Manual {
            let mut ids = page.tiled();
            ids.push(sid);
            page.rebuild(&ids, &mut next_id);
        } else {
            let zone = page.zone;
            let root = page.root.as_mut().unwrap();
            let in_zone = zone.is_some_and(|z| root.append_to_split(z, sid));
            if !in_zone {
                let target = target.unwrap_or_else(|| root.leaf_ids()[0]);
                root.insert_at(target, sid, hint, true, &mut next_id);
            }
        }
        page.zoomed = None;
        page.focused = Some(sid);
        self.next_id = next_id;
        self.active_page = page_idx;
    }

    pub fn remove_session(&mut self, sid: SessionId) {
        let mut next_id = self.next_id;
        for page in &mut self.pages {
            page.remove(sid, &mut next_id);
        }
        self.next_id = next_id;
        self.names.remove(&sid);
        self.drop_empty_pages();
    }

    fn drop_empty_pages(&mut self) {
        if self.pages.len() > 1 {
            let active_id = self.page().id;
            self.pages.retain(|p| !p.is_empty());
            if self.pages.is_empty() {
                self.pages.push(Page::new(active_id, "Main", Arrange::Grid));
            }
            self.active_page = self.pages.iter().position(|p| p.id == active_id).unwrap_or_else(|| {
                self.active_page.min(self.pages.len() - 1)
            });
        }
        self.active_page = self.active_page.min(self.pages.len() - 1);
    }

    /// Make the workspace consistent with the set of sessions that actually exist.
    /// Unknown leaves are dropped, live sessions missing from the layout are adopted.
    pub fn sanitize(&mut self, live: &HashSet<SessionId>) {
        if self.pages.is_empty() {
            let id = self.alloc_id();
            self.pages.push(Page::new(id, "Main", Arrange::Grid));
        }
        let mut seen = HashSet::new();
        let mut next_id = self.next_id;
        for page in &mut self.pages {
            let ids = page.sessions();
            for (i, &sid) in ids.iter().enumerate() {
                let duplicate = ids[..i].contains(&sid) || ids[i + 1..].contains(&sid);
                if !live.contains(&sid) || seen.contains(&sid) || duplicate {
                    // Duplicates lose every copy here and get re-adopted below as orphans.
                    page.root = page.root.take().and_then(|r| r.remove(sid));
                    page.hidden.retain(|s| *s != sid);
                } else {
                    seen.insert(sid);
                }
            }
            if page.arrange != Arrange::Manual {
                let ids = page.tiled();
                page.root = build_layout(page.arrange, &ids, &mut next_id);
            }
        }
        self.next_id = next_id;
        self.names.retain(|sid, _| live.contains(sid));
        self.active_page = self.active_page.min(self.pages.len() - 1);
        let mut orphans: Vec<SessionId> = live.iter().copied().filter(|s| !seen.contains(s)).collect();
        orphans.sort_unstable();
        let page = self.active_page;
        for sid in orphans {
            self.add_session(sid, page, None, Dir::Horizontal);
        }
        self.drop_empty_pages();
    }

    /// Drag & drop in the grid: put `sid` on the `edge` side of `target` (or swap on center).
    pub fn move_session(&mut self, sid: SessionId, target: SessionId, edge: Edge) {
        if sid == target {
            return;
        }
        let Some(target_page) = self.page_of(target) else { return };
        if edge == Edge::Center {
            let swap = |id: SessionId| match id {
                id if id == sid => target,
                id if id == target => sid,
                id => id,
            };
            for page in &mut self.pages {
                if let Some(root) = &mut page.root {
                    root.map_leaves(&mut |id| swap(id));
                }
                // Swapping with a hidden terminal swaps which of the two is hidden.
                page.hidden.iter_mut().for_each(|id| *id = swap(*id));
            }
            self.focus(sid);
            return;
        }
        let mut next_id = self.next_id;
        for page in &mut self.pages {
            if page.tiles(sid) {
                page.root = page.root.take().and_then(|r| r.remove(sid)).map(Node::flatten);
            }
            page.hidden.retain(|s| *s != sid);
        }
        let (dir, after) = match edge {
            Edge::Left => (Dir::Horizontal, false),
            Edge::Right => (Dir::Horizontal, true),
            Edge::Top => (Dir::Vertical, false),
            _ => (Dir::Vertical, true),
        };
        let page = &mut self.pages[target_page];
        if let Some(root) = &mut page.root {
            root.insert_at(target, sid, dir, after, &mut next_id);
        }
        page.arrange = Arrange::Manual;
        page.focused = Some(sid);
        self.next_id = next_id;
        self.drop_empty_pages();
        self.focus(sid);
    }

    pub fn move_to_page(&mut self, sid: SessionId, page_idx: usize) {
        if page_idx >= self.pages.len() || self.pages[page_idx].contains(sid) {
            return;
        }
        let page_id = self.pages[page_idx].id;
        let mut next_id = self.next_id;
        for page in &mut self.pages {
            page.remove(sid, &mut next_id);
        }
        self.next_id = next_id;
        let idx = self.pages.iter().position(|p| p.id == page_id).unwrap_or(0);
        self.add_session(sid, idx, None, Dir::Horizontal);
        self.drop_empty_pages();
        self.focus(sid);
    }

    pub fn set_arrange(&mut self, page_idx: usize, arrange: Arrange) {
        let mut next_id = self.next_id;
        if let Some(page) = self.pages.get_mut(page_idx) {
            page.arrange = arrange;
            if arrange != Arrange::Manual {
                let ids = page.tiled();
                page.rebuild(&ids, &mut next_id);
            }
            page.zoomed = None;
        }
        self.next_id = next_id;
    }

    pub fn new_page(&mut self) -> usize {
        let id = self.alloc_id();
        let name = format!("Page {}", self.pages.len() + 1);
        self.pages.push(Page::new(id, name, Arrange::Grid));
        self.active_page = self.pages.len() - 1;
        self.active_page
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws_with(n: u64) -> Workspace {
        let mut ws = Workspace::new(ViewMode::Grid);
        for sid in 1..=n {
            ws.add_session(sid, 0, None, Dir::Horizontal);
        }
        ws
    }

    #[test]
    fn grid_layout_is_balanced() {
        let ws = ws_with(4);
        match ws.page().root.as_ref().unwrap() {
            Node::Split { dir: Dir::Vertical, children, .. } => {
                assert_eq!(children.len(), 2);
                assert!(children.iter().all(|c| matches!(c, Node::Split { children, .. } if children.len() == 2)));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(ws.all_sessions(), vec![1, 2, 3, 4]);
    }

    #[test]
    fn remove_rebalances_and_moves_focus() {
        let mut ws = ws_with(3);
        ws.focus(3);
        ws.remove_session(3);
        assert_eq!(ws.all_sessions(), vec![1, 2]);
        assert_eq!(ws.focused(), Some(2));
        ws.remove_session(1);
        ws.remove_session(2);
        assert!(ws.page().root.is_none());
        assert_eq!(ws.pages.len(), 1);
    }

    #[test]
    fn manual_split_and_move() {
        let mut ws = ws_with(2);
        ws.focus(1);
        ws.add_session(3, 0, Some(Dir::Vertical), Dir::Horizontal);
        assert_eq!(ws.page().arrange, Arrange::Manual);
        assert_eq!(ws.all_sessions(), vec![1, 3, 2]);
        ws.move_session(2, 1, Edge::Left);
        assert_eq!(ws.all_sessions(), vec![2, 1, 3]);
        ws.move_session(2, 3, Edge::Center);
        assert_eq!(ws.all_sessions(), vec![3, 1, 2]);
    }

    #[test]
    fn zone_collects_new_terminals() {
        let mut ws = ws_with(2);
        ws.set_arrange(0, Arrange::MainStack);
        ws.page_mut().arrange = Arrange::Manual;
        let zone = ws.page().root.as_ref().unwrap().parent_split(2);
        ws.page_mut().zone = zone;
        ws.focus(1);
        ws.add_session(3, 0, None, Dir::Horizontal);
        assert_eq!(ws.page().root.as_ref().unwrap().parent_split(3), zone);
    }

    #[test]
    fn sanitize_drops_dead_and_adopts_orphans() {
        let mut ws = ws_with(3);
        let live: HashSet<_> = [2, 3, 7].into_iter().collect();
        ws.sanitize(&live);
        let mut all = ws.all_sessions();
        all.sort();
        assert_eq!(all, vec![2, 3, 7]);
    }

    #[test]
    fn hidden_terminals_stay_on_their_page() {
        let mut ws = ws_with(3);
        ws.focus(2);
        ws.hide(2);
        assert_eq!(ws.page().tiled(), vec![1, 3]);
        assert_eq!(ws.page().sessions(), vec![1, 3, 2]);
        assert_ne!(ws.focused(), Some(2));
        assert_eq!(ws.page_of(2), Some(0));
        // A page that only has hidden terminals is not empty.
        ws.hide(1);
        ws.hide(3);
        assert!(ws.page().root.is_none());
        let p = ws.new_page();
        ws.add_session(9, p, None, Dir::Horizontal);
        ws.remove_session(9);
        assert_eq!(ws.pages.len(), 1);
        assert_eq!(ws.pages[0].hidden, vec![2, 1, 3]);
        // Sanitizing keeps live hidden terminals and drops dead ones.
        ws.sanitize(&[1, 2].into_iter().collect());
        assert_eq!(ws.pages[0].hidden, vec![2, 1]);
        ws.unhide(2);
        assert_eq!(ws.page().tiled(), vec![2]);
        assert_eq!(ws.focused(), Some(2));
        // Swapping a shown and a hidden terminal swaps their visibility.
        ws.move_session(1, 2, Edge::Center);
        assert_eq!(ws.page().tiled(), vec![1]);
        assert_eq!(ws.page().hidden, vec![2]);
    }

    #[test]
    fn pages_and_move_between_them() {
        let mut ws = ws_with(2);
        let p = ws.new_page();
        ws.move_to_page(2, p);
        assert_eq!(ws.pages[0].sessions(), vec![1]);
        assert_eq!(ws.pages[1].sessions(), vec![2]);
        ws.remove_session(2);
        assert_eq!(ws.pages.len(), 1);
    }
}
