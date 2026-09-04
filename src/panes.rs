//! The split-pane layout tree.
//!
//! Pure data structure, generic over a leaf id so it can be unit-tested
//! without GPUI. A leaf is one terminal pane; a split holds two or more
//! children laid out along an axis, each with a share of the space. Splitting
//! a leaf whose parent already runs along the requested axis inserts a
//! sibling rather than nesting, so repeated splits in the same direction stay
//! evenly sized (like tmux) instead of halving each time.
//!
//! `ratios` runs parallel to `children` and always sums to 1. Every mutation
//! ends in `normalise()`, which is the single place that invariant is
//! enforced — keep it that way.

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    /// Children sit side by side.
    Horizontal,
    /// Children are stacked.
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    pub fn axis(self) -> Axis {
        match self {
            Direction::Left | Direction::Right => Axis::Horizontal,
            Direction::Up | Direction::Down => Axis::Vertical,
        }
    }

    /// Whether a new pane is placed before the existing one.
    pub fn is_before(self) -> bool {
        matches!(self, Direction::Left | Direction::Up)
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Node<T> {
    Leaf(T),
    Split {
        axis: Axis,
        children: Vec<Node<T>>,
        /// Parallel to `children`; sums to 1.0. Absent in files written
        /// before v0.4 → even split.
        #[serde(default)]
        ratios: Vec<f32>,
    },
}

/// A path from the root: each entry is a child index in a split.
pub type NodePath = Vec<usize>;

fn even(n: usize) -> Vec<f32> {
    vec![1.0 / n as f32; n]
}

impl<T: PartialEq + Clone> Node<T> {
    pub fn leaf(id: T) -> Self {
        Node::Leaf(id)
    }

    /// An evenly divided split.
    pub fn split_even(axis: Axis, children: Vec<Node<T>>) -> Self {
        let ratios = even(children.len());
        Node::Split { axis, children, ratios }
    }

    pub fn leaves(&self) -> Vec<T> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<T>) {
        match self {
            Node::Leaf(id) => out.push(id.clone()),
            Node::Split { children, .. } => {
                for c in children {
                    c.collect(out);
                }
            }
        }
    }

    pub fn len(&self) -> usize {
        self.leaves().len()
    }

    /// Rebuild the tree with every leaf transformed — used to swap pane ids
    /// for saved directories (and back) during workspace persistence.
    pub fn map<U: PartialEq + Clone>(&self, f: &mut impl FnMut(&T) -> U) -> Node<U> {
        match self {
            Node::Leaf(id) => Node::Leaf(f(id)),
            Node::Split { axis, children, ratios } => Node::Split {
                axis: *axis,
                children: children.iter().map(|c| c.map(f)).collect(),
                ratios: ratios.clone(),
            },
        }
    }

    /// Restore the invariants on this split and every split below it: one
    /// ratio per child, all positive and finite, summing to 1. Anything
    /// inconsistent (a hand-edited file, an older format) becomes an even
    /// split rather than an error.
    pub fn normalise(&mut self) {
        let Node::Split { children, ratios, .. } = self else { return };
        let n = children.len();
        let usable = ratios.len() == n && ratios.iter().all(|r| r.is_finite() && *r > 0.0);
        if !usable {
            *ratios = even(n);
        } else {
            let sum: f32 = ratios.iter().sum();
            if (sum - 1.0).abs() > 1e-4 {
                for r in ratios.iter_mut() {
                    *r /= sum;
                }
            }
        }
        debug_assert_eq!(children.len(), ratios.len());
        for c in children.iter_mut() {
            c.normalise();
        }
    }

    pub fn at_path(&self, path: &[usize]) -> Option<&Node<T>> {
        let mut node = self;
        for &ix in path {
            let Node::Split { children, .. } = node else { return None };
            node = children.get(ix)?;
        }
        Some(node)
    }

    pub fn at_path_mut(&mut self, path: &[usize]) -> Option<&mut Node<T>> {
        let mut node = self;
        for &ix in path {
            let Node::Split { children, .. } = node else { return None };
            node = children.get_mut(ix)?;
        }
        Some(node)
    }

    /// Child indices from the root down to `target`'s leaf.
    pub fn path_to(&self, target: &T) -> Option<NodePath> {
        match self {
            Node::Leaf(id) => (id == target).then(Vec::new),
            Node::Split { children, .. } => {
                for (ix, c) in children.iter().enumerate() {
                    if let Some(mut rest) = c.path_to(target) {
                        rest.insert(0, ix);
                        return Some(rest);
                    }
                }
                None
            }
        }
    }

    /// Split `target` along `direction`, inserting `new_leaf`. The new pane
    /// takes half of the target's share; nothing else moves.
    pub fn split(&mut self, target: &T, direction: Direction, new_leaf: T) -> bool {
        let axis = direction.axis();
        let before = direction.is_before();

        // Sibling insertion when this split already runs along `axis`.
        if let Node::Split { axis: my_axis, children, ratios } = self
            && *my_axis == axis
            && let Some(ix) = children
                .iter()
                .position(|c| matches!(c, Node::Leaf(id) if id == target))
        {
            let at = if before { ix } else { ix + 1 };
            let half = ratios[ix] / 2.0;
            ratios[ix] = half;
            ratios.insert(at, half);
            children.insert(at, Node::Leaf(new_leaf));
            self.normalise();
            return true;
        }

        match self {
            Node::Leaf(id) if id == target => {
                let existing = Node::Leaf(id.clone());
                let fresh = Node::Leaf(new_leaf);
                let children =
                    if before { vec![fresh, existing] } else { vec![existing, fresh] };
                *self = Node::split_even(axis, children);
                true
            }
            Node::Leaf(_) => false,
            Node::Split { children, .. } => {
                for child in children.iter_mut() {
                    if child.split(target, direction, new_leaf.clone()) {
                        return true;
                    }
                }
                false
            }
        }
    }

    /// Remove `target`, sharing its space out proportionally among the
    /// surviving siblings. A split left with a single child collapses into
    /// it, so the tree never keeps pointless one-way nodes.
    pub fn remove(&mut self, target: &T) -> bool {
        let Node::Split { children, ratios, .. } = self else {
            // A lone root leaf cannot be removed; the caller decides what
            // closing the last pane means.
            return false;
        };

        if let Some(ix) = children
            .iter()
            .position(|c| matches!(c, Node::Leaf(id) if id == target))
        {
            children.remove(ix);
            if ix < ratios.len() {
                ratios.remove(ix);
            }
            if children.len() == 1 {
                *self = children.remove(0);
            } else {
                self.normalise();
            }
            return true;
        }

        for child in children.iter_mut() {
            if child.remove(target) {
                if let Node::Split { children: inner, .. } = child
                    && inner.len() == 1
                {
                    *child = inner.remove(0);
                }
                return true;
            }
        }
        false
    }

    /// Move the divider after child `divider` of the split at `path` by
    /// `delta` (a fraction of the split; positive grows the child before the
    /// divider). Only the two neighbouring children change. Clamped so
    /// neither drops below `min`; returns whether anything moved.
    pub fn resize_divider(&mut self, path: &[usize], divider: usize, delta: f32, min: f32) -> bool {
        let Some(Node::Split { ratios, .. }) = self.at_path_mut(path) else { return false };
        if divider + 1 >= ratios.len() || !delta.is_finite() {
            return false;
        }
        let (a, b) = (ratios[divider], ratios[divider + 1]);
        let min = min.clamp(0.0, 0.5);
        // The most either side can give up.
        let delta = delta.clamp(-(a - min).max(0.0), (b - min).max(0.0));
        if delta.abs() < 1e-6 {
            return false;
        }
        ratios[divider] = a + delta;
        ratios[divider + 1] = b - delta;
        self.at_path_mut(path).expect("path still valid").normalise();
        true
    }

    /// Grow (or shrink, for negative `delta`) the pane `target` along `axis`
    /// by a fraction of the nearest enclosing split that runs that way. The
    /// space comes from the next sibling, or the previous one for the last
    /// child — so the pane's own edge moves, never a far-away one.
    pub fn resize_leaf(&mut self, target: &T, axis: Axis, delta: f32, min: f32) -> bool {
        let Some(path) = self.path_to(target) else { return false };
        for depth in (0..path.len()).rev() {
            let parent = &path[..depth];
            let ix = path[depth];
            let Some(Node::Split { axis: a, children, .. }) = self.at_path(parent) else { continue };
            if *a != axis {
                continue;
            }
            let n = children.len();
            return if ix + 1 < n {
                self.resize_divider(parent, ix, delta, min)
            } else if ix > 0 {
                self.resize_divider(parent, ix - 1, -delta, min)
            } else {
                false
            };
        }
        false
    }

    /// Reset every split, at every depth, to even shares.
    pub fn equalise(&mut self) {
        if let Node::Split { children, ratios, .. } = self {
            *ratios = even(children.len());
            for c in children.iter_mut() {
                c.equalise();
            }
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(node: &Node<u32>) -> Vec<u32> {
        node.leaves()
    }

    fn ratios_of(node: &Node<u32>, path: &[usize]) -> Vec<f32> {
        match node.at_path(path).unwrap() {
            Node::Split { ratios, .. } => ratios.clone(),
            Node::Leaf(_) => panic!("not a split"),
        }
    }

    fn assert_close(a: &[f32], b: &[f32]) {
        assert_eq!(a.len(), b.len(), "{a:?} vs {b:?}");
        for (x, y) in a.iter().zip(b) {
            assert!((x - y).abs() < 1e-4, "{a:?} vs {b:?}");
        }
    }

    /// Walk every split and check the parallel-vec invariant.
    fn check_invariants(node: &Node<u32>) {
        if let Node::Split { children, ratios, .. } = node {
            assert_eq!(children.len(), ratios.len());
            assert!(ratios.iter().all(|r| *r > 0.0));
            assert!((ratios.iter().sum::<f32>() - 1.0).abs() < 1e-4, "{ratios:?}");
            for c in children {
                check_invariants(c);
            }
        }
    }

    #[test]
    fn split_right_then_down_nests_only_the_right_pane() {
        // The scenario from the spec: split right, focus the new pane, split
        // down. The original pane keeps the full left side.
        let mut tree = Node::leaf(1u32);
        assert!(tree.split(&1, Direction::Right, 2));
        assert_eq!(ids(&tree), vec![1, 2]);

        assert!(tree.split(&2, Direction::Down, 3));
        match &tree {
            Node::Split { axis, children, ratios } => {
                assert_eq!(*axis, Axis::Horizontal);
                assert_eq!(children.len(), 2, "left pane must stay a single child");
                assert_close(ratios, &[0.5, 0.5]);
                assert_eq!(children[0], Node::Leaf(1));
                match &children[1] {
                    Node::Split { axis, children, ratios } => {
                        assert_eq!(*axis, Axis::Vertical);
                        assert_eq!(children, &vec![Node::Leaf(2), Node::Leaf(3)]);
                        assert_close(ratios, &[0.5, 0.5]);
                    }
                    other => panic!("right side should be a vertical split, got {other:?}"),
                }
            }
            other => panic!("root should be a horizontal split, got {other:?}"),
        }
        check_invariants(&tree);
    }

    #[test]
    fn repeated_same_axis_splits_stay_siblings() {
        let mut tree = Node::leaf(1u32);
        tree.split(&1, Direction::Right, 2);
        tree.split(&2, Direction::Right, 3);
        // Three columns, not nested halves. The last split halves pane 2's
        // share, so the shape is 1/2, 1/4, 1/4 — like vim.
        match &tree {
            Node::Split { axis, children, ratios } => {
                assert_eq!(*axis, Axis::Horizontal);
                assert_eq!(children.len(), 3);
                assert_close(ratios, &[0.5, 0.25, 0.25]);
            }
            other => panic!("expected one flat split, got {other:?}"),
        }
        assert_eq!(ids(&tree), vec![1, 2, 3]);
        check_invariants(&tree);
    }

    #[test]
    fn splitting_in_a_three_way_split_halves_only_the_target() {
        let mut tree = Node::split_even(Axis::Horizontal, vec![Node::Leaf(1u32), Node::Leaf(2), Node::Leaf(3)]);
        assert!(tree.split(&2, Direction::Right, 4));
        assert_eq!(ids(&tree), vec![1, 2, 4, 3]);
        assert_close(&ratios_of(&tree, &[]), &[1.0 / 3.0, 1.0 / 6.0, 1.0 / 6.0, 1.0 / 3.0]);
        check_invariants(&tree);
    }

    #[test]
    fn split_left_and_up_insert_before() {
        let mut tree = Node::leaf(1u32);
        tree.split(&1, Direction::Left, 2);
        assert_eq!(ids(&tree), vec![2, 1]);

        let mut tree = Node::leaf(1u32);
        tree.split(&1, Direction::Up, 2);
        assert_eq!(ids(&tree), vec![2, 1]);
    }

    #[test]
    fn removing_collapses_single_child_splits() {
        let mut tree = Node::leaf(1u32);
        tree.split(&1, Direction::Right, 2);
        tree.split(&2, Direction::Down, 3);

        assert!(tree.remove(&3));
        // The right side collapses back to a plain leaf.
        assert_eq!(tree, Node::split_even(Axis::Horizontal, vec![Node::Leaf(1), Node::Leaf(2)]));

        assert!(tree.remove(&2));
        assert_eq!(tree, Node::Leaf(1), "root collapses to the survivor");
        assert_eq!(tree.len(), 1);
    }

    #[test]
    fn removing_redistributes_proportionally() {
        let mut tree = Node::Split {
            axis: Axis::Horizontal,
            children: vec![Node::Leaf(1u32), Node::Leaf(2), Node::Leaf(3)],
            ratios: vec![0.5, 0.3, 0.2],
        };
        assert!(tree.remove(&2));
        assert_eq!(ids(&tree), vec![1, 3]);
        // 0.5 : 0.2 keeps its proportion → 5/7 : 2/7.
        assert_close(&ratios_of(&tree, &[]), &[5.0 / 7.0, 2.0 / 7.0]);
        check_invariants(&tree);
    }

    #[test]
    fn last_leaf_cannot_be_removed() {
        let mut tree = Node::leaf(1u32);
        assert!(!tree.remove(&1));
        assert_eq!(tree.len(), 1);
    }

    #[test]
    fn removing_an_unknown_leaf_is_a_no_op() {
        let mut tree = Node::leaf(1u32);
        tree.split(&1, Direction::Right, 2);
        assert!(!tree.remove(&99));
        assert_eq!(ids(&tree), vec![1, 2]);
    }

    #[test]
    fn paths_round_trip() {
        let mut tree = Node::leaf(1u32);
        tree.split(&1, Direction::Right, 2);
        tree.split(&2, Direction::Down, 3);
        assert_eq!(tree.path_to(&1), Some(vec![0]));
        assert_eq!(tree.path_to(&2), Some(vec![1, 0]));
        assert_eq!(tree.path_to(&3), Some(vec![1, 1]));
        assert_eq!(tree.path_to(&9), None);
        assert_eq!(tree.at_path(&[1, 1]), Some(&Node::Leaf(3)));
        assert_eq!(tree.at_path(&[1, 2]), None);
        assert_eq!(tree.at_path(&[0, 0]), None, "leaves have no children");
        assert!(matches!(tree.at_path(&[]), Some(Node::Split { .. })));
        if let Some(Node::Leaf(id)) = tree.at_path_mut(&[1, 1]) {
            *id = 30;
        }
        assert_eq!(ids(&tree), vec![1, 2, 30]);
    }

    #[test]
    fn dragging_a_divider_moves_only_its_neighbours() {
        let mut tree = Node::split_even(Axis::Horizontal, vec![Node::Leaf(1u32), Node::Leaf(2), Node::Leaf(3)]);
        assert!(tree.resize_divider(&[], 0, 0.1, 0.05));
        assert_close(&ratios_of(&tree, &[]), &[1.0 / 3.0 + 0.1, 1.0 / 3.0 - 0.1, 1.0 / 3.0]);
        check_invariants(&tree);
        assert!(!tree.resize_divider(&[], 2, 0.1, 0.05), "no divider after the last child");
        assert!(!tree.resize_divider(&[0], 0, 0.1, 0.05), "leaves have no dividers");
    }

    #[test]
    fn dragging_past_the_minimum_stops_at_it() {
        let mut tree = Node::split_even(Axis::Horizontal, vec![Node::Leaf(1u32), Node::Leaf(2)]);
        assert!(tree.resize_divider(&[], 0, 5.0, 0.1));
        assert_close(&ratios_of(&tree, &[]), &[0.9, 0.1]);
        // Already at the limit: nothing to give.
        assert!(!tree.resize_divider(&[], 0, 0.01, 0.1));
        // And back the other way, clamped again.
        assert!(tree.resize_divider(&[], 0, -5.0, 0.1));
        assert_close(&ratios_of(&tree, &[]), &[0.1, 0.9]);
        check_invariants(&tree);
    }

    #[test]
    fn resize_leaf_finds_the_enclosing_split_on_the_right_axis() {
        // [1 | [2 / 3]]: pane 3 is inside a vertical split inside a
        // horizontal one.
        let mut tree = Node::leaf(1u32);
        tree.split(&1, Direction::Right, 2);
        tree.split(&2, Direction::Down, 3);

        // Vertically, 3's parent runs that way: 3 is the last child, so it
        // grows by taking from 2.
        assert!(tree.resize_leaf(&3, Axis::Vertical, 0.2, 0.05));
        assert_close(&ratios_of(&tree, &[1]), &[0.3, 0.7]);
        assert_close(&ratios_of(&tree, &[]), &[0.5, 0.5]);

        // Horizontally, walk up to the root split: the right column (which
        // holds 3) grows by taking from 1.
        assert!(tree.resize_leaf(&3, Axis::Horizontal, 0.1, 0.05));
        assert_close(&ratios_of(&tree, &[]), &[0.4, 0.6]);
        assert_close(&ratios_of(&tree, &[1]), &[0.3, 0.7]);

        // Pane 1 grows to the right by pushing its own divider.
        assert!(tree.resize_leaf(&1, Axis::Horizontal, 0.1, 0.05));
        assert_close(&ratios_of(&tree, &[]), &[0.5, 0.5]);
        // A lone root leaf has nothing to resize against.
        assert!(!Node::leaf(7u32).resize_leaf(&7, Axis::Horizontal, 0.1, 0.05));
        check_invariants(&tree);
    }

    #[test]
    fn equalise_restores_evenness_at_any_depth() {
        let mut tree = Node::Split {
            axis: Axis::Horizontal,
            children: vec![
                Node::Leaf(1u32),
                Node::Split {
                    axis: Axis::Vertical,
                    children: vec![Node::Leaf(2), Node::Leaf(3), Node::Leaf(4)],
                    ratios: vec![0.7, 0.2, 0.1],
                },
            ],
            ratios: vec![0.8, 0.2],
        };
        tree.equalise();
        assert_close(&ratios_of(&tree, &[]), &[0.5, 0.5]);
        assert_close(&ratios_of(&tree, &[1]), &[1.0 / 3.0; 3]);
        check_invariants(&tree);
    }

    #[test]
    fn normalise_repairs_bad_ratios() {
        let mut tree: Node<u32> = Node::Split {
            axis: Axis::Horizontal,
            children: vec![Node::Leaf(1), Node::Leaf(2)],
            ratios: vec![],
        };
        tree.normalise();
        assert_close(&ratios_of(&tree, &[]), &[0.5, 0.5]);

        let mut tree: Node<u32> = Node::Split {
            axis: Axis::Horizontal,
            children: vec![Node::Leaf(1), Node::Leaf(2)],
            ratios: vec![3.0, 1.0],
        };
        tree.normalise();
        assert_close(&ratios_of(&tree, &[]), &[0.75, 0.25]);

        for bad in [vec![0.5], vec![0.0, 1.0], vec![f32::NAN, 0.5], vec![-1.0, 2.0]] {
            let mut tree: Node<u32> = Node::Split {
                axis: Axis::Horizontal,
                children: vec![Node::Leaf(1), Node::Leaf(2)],
                ratios: bad,
            };
            tree.normalise();
            assert_close(&ratios_of(&tree, &[]), &[0.5, 0.5]);
        }
    }

    #[test]
    fn map_preserves_ratios() {
        let tree = Node::Split {
            axis: Axis::Vertical,
            children: vec![Node::Leaf(1u32), Node::Leaf(2)],
            ratios: vec![0.3, 0.7],
        };
        let mapped = tree.map(&mut |id| format!("p{id}"));
        assert_eq!(mapped.leaves(), vec!["p1", "p2"]);
        match mapped.at_path(&[]).unwrap() {
            Node::Split { ratios, .. } => assert_close(ratios, &[0.3, 0.7]),
            _ => panic!(),
        }
        let back = mapped.map(&mut |s| s[1..].parse::<u32>().unwrap());
        assert_eq!(back, tree);
    }

    #[test]
    fn v1_json_without_ratios_loads_as_even() {
        let json = r#"{"split":{"axis":"horizontal","children":[{"leaf":1},{"leaf":2},{"leaf":3}]}}"#;
        let mut tree: Node<u32> = serde_json::from_str(json).unwrap();
        assert_eq!(ids(&tree), vec![1, 2, 3]);
        tree.normalise();
        assert_close(&ratios_of(&tree, &[]), &[1.0 / 3.0; 3]);
    }
}
