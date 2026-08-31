//! The split-pane layout tree.
//!
//! Pure data structure, generic over a leaf id so it can be unit-tested
//! without GPUI. A leaf is one terminal pane; a split holds two or more
//! children laid out along an axis. Splitting a leaf whose parent already
//! runs along the requested axis inserts a sibling rather than nesting, so
//! repeated splits in the same direction stay evenly sized (like tmux)
//! instead of halving each time.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq)]
pub enum Node<T> {
    Leaf(T),
    Split { axis: Axis, children: Vec<Node<T>> },
}

impl<T: PartialEq + Clone> Node<T> {
    pub fn leaf(id: T) -> Self {
        Node::Leaf(id)
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

    /// Split `target` along `direction`, inserting `new_leaf`.
    pub fn split(&mut self, target: &T, direction: Direction, new_leaf: T) -> bool {
        let axis = direction.axis();
        let before = direction.is_before();

        // Sibling insertion when this split already runs along `axis`.
        if let Node::Split { axis: my_axis, children } = self
            && *my_axis == axis
            && let Some(ix) = children
                .iter()
                .position(|c| matches!(c, Node::Leaf(id) if id == target))
        {
            let at = if before { ix } else { ix + 1 };
            children.insert(at, Node::Leaf(new_leaf));
            return true;
        }

        match self {
            Node::Leaf(id) if id == target => {
                let existing = Node::Leaf(id.clone());
                let fresh = Node::Leaf(new_leaf);
                let children =
                    if before { vec![fresh, existing] } else { vec![existing, fresh] };
                *self = Node::Split { axis, children };
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

    /// Remove `target`. A split left with a single child collapses into it,
    /// so the tree never keeps pointless one-way nodes.
    pub fn remove(&mut self, target: &T) -> bool {
        let Node::Split { children, .. } = self else {
            // A lone root leaf cannot be removed; the caller decides what
            // closing the last pane means.
            return false;
        };

        if let Some(ix) = children
            .iter()
            .position(|c| matches!(c, Node::Leaf(id) if id == target))
        {
            children.remove(ix);
            if children.len() == 1 {
                *self = children.remove(0);
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(node: &Node<u32>) -> Vec<u32> {
        node.leaves()
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
            Node::Split { axis, children } => {
                assert_eq!(*axis, Axis::Horizontal);
                assert_eq!(children.len(), 2, "left pane must stay a single child");
                assert_eq!(children[0], Node::Leaf(1));
                match &children[1] {
                    Node::Split { axis, children } => {
                        assert_eq!(*axis, Axis::Vertical);
                        assert_eq!(children, &vec![Node::Leaf(2), Node::Leaf(3)]);
                    }
                    other => panic!("right side should be a vertical split, got {other:?}"),
                }
            }
            other => panic!("root should be a horizontal split, got {other:?}"),
        }
    }

    #[test]
    fn repeated_same_axis_splits_stay_siblings() {
        let mut tree = Node::leaf(1u32);
        tree.split(&1, Direction::Right, 2);
        tree.split(&2, Direction::Right, 3);
        // Three even columns, not nested halves.
        match &tree {
            Node::Split { axis, children } => {
                assert_eq!(*axis, Axis::Horizontal);
                assert_eq!(children.len(), 3);
            }
            other => panic!("expected one flat split, got {other:?}"),
        }
        assert_eq!(ids(&tree), vec![1, 2, 3]);
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
        assert_eq!(tree, Node::Split { axis: Axis::Horizontal, children: vec![Node::Leaf(1), Node::Leaf(2)] });

        assert!(tree.remove(&2));
        assert_eq!(tree, Node::Leaf(1), "root collapses to the survivor");
        assert_eq!(tree.len(), 1);
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
}
