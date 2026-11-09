//! Tabs and split panes, without a window: which panes exist, how each
//! tab divides its area between them, which one has focus. The window
//! (`window.rs`) owns the sessions and draws; this module only does the
//! geometry and bookkeeping, so all of it is tested here.

use serde::{Deserialize, Serialize};

pub type PaneId = u64;

/// How a split divides its area.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Split {
    /// Side by side (a divider running top to bottom).
    Right,
    /// One above the other.
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// A tab's layout: a pane, or two parts divided at `ratio` (the first's
/// share, 0-1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Node {
    Pane(PaneId),
    Split { split: Split, ratio: f32, first: Box<Node>, second: Box<Node> },
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

/// A divider between two parts of a split, for drawing and dragging.
#[derive(Clone, Debug, PartialEq)]
pub struct Divider {
    /// Where it is drawn (and grabbed).
    pub rect: Rect,
    pub split: Split,
    /// The split's whole area, to turn a pointer position into a ratio.
    pub area: Rect,
    /// Path to the split node from the tab's root (false = first).
    pub path: Vec<bool>,
}

/// Smallest share either side of a split keeps.
const MIN_RATIO: f32 = 0.08;

impl Node {
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<PaneId>) {
        match self {
            Node::Pane(p) => out.push(*p),
            Node::Split { first, second, .. } => {
                first.collect(out);
                second.collect(out);
            }
        }
    }

    fn contains(&self, pane: PaneId) -> bool {
        match self {
            Node::Pane(p) => *p == pane,
            Node::Split { first, second, .. } => first.contains(pane) || second.contains(pane),
        }
    }

    /// Replace pane `at` with a split of it and `new`.
    fn split_at(&mut self, at: PaneId, new: PaneId, split: Split) -> bool {
        match self {
            Node::Pane(p) if *p == at => {
                *self = Node::Split {
                    split,
                    ratio: 0.5,
                    first: Box::new(Node::Pane(at)),
                    second: Box::new(Node::Pane(new)),
                };
                true
            }
            Node::Pane(_) => false,
            Node::Split { first, second, .. } => {
                first.split_at(at, new, split) || second.split_at(at, new, split)
            }
        }
    }

    /// Remove a pane; its sibling takes the split's place. `None` if this
    /// node was that pane.
    fn remove(self, pane: PaneId) -> Option<Node> {
        match self {
            Node::Pane(p) if p == pane => None,
            Node::Pane(_) => Some(self),
            Node::Split { split, ratio, first, second } => {
                match (first.remove(pane), second.remove(pane)) {
                    (Some(a), Some(b)) => {
                        Some(Node::Split { split, ratio, first: Box::new(a), second: Box::new(b) })
                    }
                    (Some(a), None) => Some(a),
                    (None, Some(b)) => Some(b),
                    (None, None) => None,
                }
            }
        }
    }

    fn layout(
        &self,
        area: Rect,
        gap: f32,
        path: &mut Vec<bool>,
        out: &mut Vec<(PaneId, Rect)>,
        divs: &mut Vec<Divider>,
    ) {
        match self {
            Node::Pane(p) => out.push((*p, area)),
            Node::Split { split, ratio, first, second } => {
                let (a, b, d) = divide(area, *split, *ratio, gap);
                divs.push(Divider { rect: d, split: *split, area, path: path.clone() });
                path.push(false);
                first.layout(a, gap, path, out, divs);
                path.pop();
                path.push(true);
                second.layout(b, gap, path, out, divs);
                path.pop();
            }
        }
    }

    fn at_path(&mut self, path: &[bool]) -> Option<&mut Node> {
        match (path.split_first(), self) {
            (None, n) => Some(n),
            (Some((false, rest)), Node::Split { first, .. }) => first.at_path(rest),
            (Some((true, rest)), Node::Split { second, .. }) => second.at_path(rest),
            _ => None,
        }
    }

    /// Path to the nearest split above `pane` that divides along `split`,
    /// and whether the pane is in its second part.
    fn ancestor(&self, pane: PaneId, split: Split) -> Option<(Vec<bool>, bool)> {
        fn walk(
            n: &Node,
            pane: PaneId,
            split: Split,
            path: &mut Vec<bool>,
            best: &mut Option<(Vec<bool>, bool)>,
        ) -> bool {
            match n {
                Node::Pane(p) => *p == pane,
                Node::Split { split: s, first, second, .. } => {
                    for (side, child) in [(false, first), (true, second)] {
                        path.push(side);
                        let found = walk(child, pane, split, path, best);
                        path.pop();
                        if found {
                            // Deepest matching split wins: set on the way up
                            // only if nothing deeper matched.
                            if *s == split && best.is_none() {
                                *best = Some((path.clone(), side));
                            }
                            return true;
                        }
                    }
                    false
                }
            }
        }
        let mut best = None;
        walk(self, pane, split, &mut Vec::new(), &mut best);
        best
    }
}

/// The two parts of `area` and the divider between them.
fn divide(area: Rect, split: Split, ratio: f32, gap: f32) -> (Rect, Rect, Rect) {
    let r = ratio.clamp(MIN_RATIO, 1.0 - MIN_RATIO);
    match split {
        Split::Right => {
            let w1 = ((area.w - gap) * r).round().max(0.0);
            let a = Rect::new(area.x, area.y, w1, area.h);
            let d = Rect::new(area.x + w1, area.y, gap, area.h);
            let b = Rect::new(area.x + w1 + gap, area.y, (area.w - w1 - gap).max(0.0), area.h);
            (a, b, d)
        }
        Split::Down => {
            let h1 = ((area.h - gap) * r).round().max(0.0);
            let a = Rect::new(area.x, area.y, area.w, h1);
            let d = Rect::new(area.x, area.y + h1, area.w, gap);
            let b = Rect::new(area.x, area.y + h1 + gap, area.w, (area.h - h1 - gap).max(0.0));
            (a, b, d)
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tab {
    pub id: u64,
    pub root: Node,
    pub focus: PaneId,
    /// The focused pane fills the tab.
    pub zoomed: bool,
    /// A label the user gave the tab (otherwise it follows its pane).
    pub label: Option<String>,
    /// The tab's profile (theme and background); `None` for the window's.
    pub profile: Option<String>,
}

/// What closing a pane did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Closed {
    /// The pane went; its tab remains.
    Pane,
    /// It was the tab's last pane; the tab went too.
    Tab,
    /// It was the last pane of the last tab.
    Last,
    NotFound,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Workspace {
    pub tabs: Vec<Tab>,
    pub active: usize,
    next_tab: u64,
}

impl Workspace {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a tab holding `root` after the active one, and switch to it.
    pub fn add_tab(&mut self, root: Node, profile: Option<String>) -> usize {
        self.next_tab += 1;
        let focus = root.panes()[0];
        let tab = Tab { id: self.next_tab, root, focus, zoomed: false, label: None, profile };
        let at = if self.tabs.is_empty() { 0 } else { self.active + 1 };
        self.tabs.insert(at, tab);
        self.active = at;
        at
    }

    pub fn tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    pub fn tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    /// The focused pane of the active tab.
    pub fn focus(&self) -> Option<PaneId> {
        self.tab().map(|t| t.focus)
    }

    pub fn tab_of(&self, pane: PaneId) -> Option<usize> {
        self.tabs.iter().position(|t| t.root.contains(pane))
    }

    /// Every pane, in tab order.
    pub fn panes(&self) -> Vec<PaneId> {
        self.tabs.iter().flat_map(|t| t.root.panes()).collect()
    }

    /// Split the focused pane; `new` gets the second part and the focus.
    pub fn split(&mut self, new: PaneId, split: Split) -> bool {
        let Some(t) = self.tab_mut() else { return false };
        let at = t.focus;
        if t.root.split_at(at, new, split) {
            t.focus = new;
            t.zoomed = false;
            true
        } else {
            false
        }
    }

    pub fn close(&mut self, pane: PaneId) -> Closed {
        let Some(i) = self.tab_of(pane) else { return Closed::NotFound };
        let t = &mut self.tabs[i];
        let order = t.root.panes();
        let root = std::mem::replace(&mut t.root, Node::Pane(0));
        match root.remove(pane) {
            Some(rest) => {
                t.root = rest;
                if t.focus == pane {
                    // The pane before it in order (or after, if it was first).
                    let pos = order.iter().position(|p| *p == pane).unwrap_or(0);
                    t.focus = if pos > 0 { order[pos - 1] } else { order[1] };
                }
                t.zoomed = false;
                Closed::Pane
            }
            None => {
                self.tabs.remove(i);
                if self.tabs.is_empty() {
                    self.active = 0;
                    return Closed::Last;
                }
                if self.active > i || self.active >= self.tabs.len() {
                    self.active = self.active.saturating_sub(1);
                }
                Closed::Tab
            }
        }
    }

    pub fn close_tab(&mut self, i: usize) -> Vec<PaneId> {
        if i >= self.tabs.len() {
            return Vec::new();
        }
        let panes = self.tabs[i].root.panes();
        self.tabs.remove(i);
        if self.active > i || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        panes
    }

    /// Focus a pane (and its tab).
    pub fn set_focus(&mut self, pane: PaneId) -> bool {
        let Some(i) = self.tab_of(pane) else { return false };
        self.active = i;
        self.tabs[i].focus = pane;
        true
    }

    /// Switch tab by offset (wrapping).
    pub fn cycle(&mut self, delta: i64) {
        if self.tabs.is_empty() {
            return;
        }
        let n = self.tabs.len() as i64;
        self.active = (self.active as i64 + delta).rem_euclid(n) as usize;
    }

    pub fn goto(&mut self, i: usize) -> bool {
        if i < self.tabs.len() {
            self.active = i;
            true
        } else {
            false
        }
    }

    /// Move the active tab by `delta` places.
    pub fn move_tab(&mut self, delta: i64) {
        let n = self.tabs.len() as i64;
        if n < 2 {
            return;
        }
        let to = (self.active as i64 + delta).clamp(0, n - 1) as usize;
        let t = self.tabs.remove(self.active);
        self.tabs.insert(to, t);
        self.active = to;
    }

    /// Move tab `from` to position `to` (drag and drop).
    pub fn reorder(&mut self, from: usize, to: usize) {
        if from >= self.tabs.len() || to >= self.tabs.len() || from == to {
            return;
        }
        let active_id = self.tabs[self.active].id;
        let t = self.tabs.remove(from);
        self.tabs.insert(to, t);
        self.active = self.tabs.iter().position(|t| t.id == active_id).unwrap_or(0);
    }

    /// Pane rectangles of the active tab in `area` (a zoomed tab shows its
    /// focused pane only), and its dividers.
    pub fn layout(&self, area: Rect, gap: f32) -> (Vec<(PaneId, Rect)>, Vec<Divider>) {
        let Some(t) = self.tab() else { return (Vec::new(), Vec::new()) };
        if t.zoomed {
            return (vec![(t.focus, area)], Vec::new());
        }
        let (mut panes, mut divs) = (Vec::new(), Vec::new());
        t.root.layout(area, gap, &mut Vec::new(), &mut panes, &mut divs);
        (panes, divs)
    }

    /// Move focus to the nearest pane in a direction.
    pub fn focus_toward(&mut self, dir: Direction, area: Rect, gap: f32) -> bool {
        let (rects, _) = self.layout(area, gap);
        let Some(focus) = self.focus() else { return false };
        let Some(&(_, from)) = rects.iter().find(|(p, _)| *p == focus) else { return false };
        let (fx, fy) = from.center();
        let best = rects
            .iter()
            .filter(|(p, r)| {
                *p != focus
                    && match dir {
                        Direction::Left => r.x + r.w <= from.x + 0.5,
                        Direction::Right => r.x >= from.x + from.w - 0.5,
                        Direction::Up => r.y + r.h <= from.y + 0.5,
                        Direction::Down => r.y >= from.y + from.h - 0.5,
                    }
            })
            .min_by(|(_, a), (_, b)| {
                let d = |r: &Rect| {
                    let (x, y) = r.center();
                    // Prefer panes in line with this one.
                    match dir {
                        Direction::Left | Direction::Right => (x - fx).abs() + 2.0 * (y - fy).abs(),
                        Direction::Up | Direction::Down => (y - fy).abs() + 2.0 * (x - fx).abs(),
                    }
                };
                d(a).total_cmp(&d(b))
            });
        match best {
            Some((p, _)) => {
                let p = *p;
                self.tab_mut().unwrap().focus = p;
                true
            }
            None => false,
        }
    }

    /// Move the divider nearest the focused pane in a direction by `step`
    /// (a share of the split's size).
    pub fn resize(&mut self, dir: Direction, step: f32) -> bool {
        let Some(t) = self.tab_mut() else { return false };
        let split = match dir {
            Direction::Left | Direction::Right => Split::Right,
            Direction::Up | Direction::Down => Split::Down,
        };
        let Some((path, _)) = t.root.ancestor(t.focus, split) else { return false };
        let delta = match dir {
            Direction::Right | Direction::Down => step,
            _ => -step,
        };
        if let Some(Node::Split { ratio, .. }) = t.root.at_path(&path) {
            *ratio = (*ratio + delta).clamp(MIN_RATIO, 1.0 - MIN_RATIO);
            return true;
        }
        false
    }

    /// Set a split's ratio (dragging a divider).
    pub fn set_ratio(&mut self, path: &[bool], value: f32) {
        if let Some(t) = self.tab_mut() {
            if let Some(Node::Split { ratio, .. }) = t.root.at_path(path) {
                *ratio = value.clamp(MIN_RATIO, 1.0 - MIN_RATIO);
            }
        }
    }

    /// Make every split of the active tab even.
    pub fn equalize(&mut self) {
        fn even(n: &mut Node) {
            if let Node::Split { ratio, first, second, .. } = n {
                *ratio = 0.5;
                even(first);
                even(second);
            }
        }
        if let Some(t) = self.tab_mut() {
            even(&mut t.root);
        }
    }

    pub fn toggle_zoom(&mut self) {
        if let Some(t) = self.tab_mut() {
            t.zoomed = !t.zoomed && t.root.panes().len() > 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> Workspace {
        let mut w = Workspace::new();
        w.add_tab(Node::Pane(1), None);
        w
    }

    const AREA: Rect = Rect { x: 0.0, y: 0.0, w: 802.0, h: 602.0 };

    #[test]
    fn splitting_and_rectangles() {
        let mut w = ws();
        assert!(w.split(2, Split::Right));
        assert_eq!(w.focus(), Some(2));
        let (r, d) = w.layout(AREA, 2.0);
        assert_eq!(
            r,
            [(1, Rect::new(0.0, 0.0, 400.0, 602.0)), (2, Rect::new(402.0, 0.0, 400.0, 602.0))]
        );
        assert_eq!(d[0].rect, Rect::new(400.0, 0.0, 2.0, 602.0));
        assert!(w.split(3, Split::Down));
        let (r, d) = w.layout(AREA, 2.0);
        assert_eq!(r[1], (2, Rect::new(402.0, 0.0, 400.0, 300.0)));
        assert_eq!(r[2], (3, Rect::new(402.0, 302.0, 400.0, 300.0)));
        assert_eq!(d.len(), 2);
        assert_eq!(d[1].path, [true]);
        assert_eq!(w.panes(), [1, 2, 3]);
    }

    #[test]
    fn closing_moves_focus_and_collapses() {
        let mut w = ws();
        w.split(2, Split::Right);
        w.split(3, Split::Down);
        assert_eq!(w.close(3), Closed::Pane);
        assert_eq!(w.focus(), Some(2));
        assert_eq!(
            w.tab().unwrap().root,
            Node::Split {
                split: Split::Right,
                ratio: 0.5,
                first: Box::new(Node::Pane(1)),
                second: Box::new(Node::Pane(2))
            }
        );
        assert_eq!(w.close(1), Closed::Pane);
        assert_eq!(w.tab().unwrap().root, Node::Pane(2));
        w.add_tab(Node::Pane(9), None);
        assert_eq!(w.active, 1);
        assert_eq!(w.close(9), Closed::Tab);
        assert_eq!(w.active, 0);
        assert_eq!(w.close(2), Closed::Last);
        assert_eq!(w.close(2), Closed::NotFound);
    }

    #[test]
    fn focus_by_direction() {
        let mut w = ws();
        w.split(2, Split::Right); // 1 | 2
        w.split(3, Split::Down); //      2 / 3
        assert!(w.focus_toward(Direction::Left, AREA, 2.0));
        assert_eq!(w.focus(), Some(1));
        assert!(!w.focus_toward(Direction::Left, AREA, 2.0), "nothing further left");
        assert!(w.focus_toward(Direction::Right, AREA, 2.0));
        assert_eq!(w.focus(), Some(2), "the upper one is nearer the middle... or in line");
        assert!(w.focus_toward(Direction::Down, AREA, 2.0));
        assert_eq!(w.focus(), Some(3));
        assert!(w.focus_toward(Direction::Up, AREA, 2.0));
        assert_eq!(w.focus(), Some(2));
    }

    #[test]
    fn resizing_zoom_and_dragging() {
        let mut w = ws();
        w.split(2, Split::Right);
        w.split(3, Split::Down);
        // From pane 3, Left/Right move the outer vertical divider.
        assert!(w.resize(Direction::Left, 0.1));
        let (r, _) = w.layout(AREA, 2.0);
        assert_eq!(r[0].1.w, 320.0);
        assert!(w.resize(Direction::Down, 0.1), "and Up/Down the inner one");
        let (r, _) = w.layout(AREA, 2.0);
        assert_eq!(r[1].1.h, 360.0);
        w.set_ratio(&[], 0.99);
        let (r, _) = w.layout(AREA, 2.0);
        assert!(r[0].1.w < 740.0, "clamped: {:?}", r[0]);
        w.equalize();
        assert_eq!(w.layout(AREA, 2.0).0[0].1.w, 400.0);
        w.toggle_zoom();
        assert_eq!(w.layout(AREA, 2.0).0, [(3, AREA)]);
        w.toggle_zoom();
        assert_eq!(w.layout(AREA, 2.0).0.len(), 3);
        let mut single = ws();
        assert!(!single.resize(Direction::Left, 0.1));
        single.toggle_zoom();
        assert!(!single.tab().unwrap().zoomed, "nothing to zoom");
    }

    #[test]
    fn tabs_cycle_move_and_reorder() {
        let mut w = ws();
        w.add_tab(Node::Pane(2), None);
        w.add_tab(Node::Pane(3), Some("work".into()));
        assert_eq!(w.active, 2);
        w.cycle(1);
        assert_eq!(w.active, 0);
        w.cycle(-1);
        assert_eq!(w.active, 2);
        w.move_tab(-5);
        assert_eq!(w.active, 0);
        assert_eq!(w.panes(), [3, 1, 2]);
        w.reorder(0, 2);
        assert_eq!(w.panes(), [1, 2, 3]);
        assert_eq!(w.focus(), Some(3), "the active tab stays active");
        assert!(w.set_focus(1));
        assert_eq!(w.active, 0);
        assert_eq!(w.close_tab(0), [1]);
        assert_eq!(w.panes(), [2, 3]);
    }

    #[test]
    fn layouts_serialize() {
        let mut w = ws();
        w.split(2, Split::Down);
        let json = serde_json::to_string(&w.tab().unwrap().root).unwrap();
        assert_eq!(json, r#"{"split":"down","ratio":0.5,"first":1,"second":2}"#);
        let back: Node = serde_json::from_str(&json).unwrap();
        assert_eq!(back, w.tab().unwrap().root);
    }
}
