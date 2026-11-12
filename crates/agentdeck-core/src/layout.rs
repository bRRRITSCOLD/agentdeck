//! A tab's split tree (spec §3 `tabs.layout`): a pane, or a split of two subtrees.
//!
//! Stored as JSON: `{"pane":id}` or `{"split":"h"|"v","ratio":f,"a":…,"b":…}`.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::id::PaneId;
use crate::text_enum::text_enum;

text_enum! {
    /// How a split arranges its two sides.
    pub enum SplitDirection ("split direction") {
        /// Side by side: `a` on the left, `b` on the right (as tmux's `split-window -h`).
        Horizontal => "h",
        /// Stacked: `a` above, `b` below.
        Vertical => "v",
    }
}

/// What a layout change can fail on.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum LayoutError {
    /// The pane isn't in this layout.
    #[error("pane {0} is not in the layout")]
    PaneNotFound(PaneId),
    /// The pane is already in this layout; each pane appears once.
    #[error("pane {0} is already in the layout")]
    DuplicatePane(PaneId),
    /// A split ratio outside `(0, 1)`.
    #[error("split ratio {0} is not between 0 and 1")]
    Ratio(f32),
}

/// The share of a split that its `a` side takes, strictly between 0 and 1.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "f32", into = "f32")]
pub struct Ratio(f32);

impl Ratio {
    /// An even split.
    pub const HALF: Ratio = Ratio(0.5);

    /// A ratio, if `value` is strictly between 0 and 1.
    pub fn new(value: f32) -> Result<Self, LayoutError> {
        if value > 0.0 && value < 1.0 {
            Ok(Self(value))
        } else {
            Err(LayoutError::Ratio(value))
        }
    }

    /// The share as a number in `(0, 1)`.
    pub const fn get(self) -> f32 {
        self.0
    }
}

impl TryFrom<f32> for Ratio {
    type Error = LayoutError;

    fn try_from(value: f32) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Ratio> for f32 {
    fn from(ratio: Ratio) -> Self {
        ratio.0
    }
}

/// A tab's split tree. Every pane appears once; [`Layout::split`] keeps it that way.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum Layout {
    /// One pane filling the space.
    Pane {
        /// The pane shown here.
        pane: PaneId,
    },
    /// Two subtrees sharing the space.
    Split {
        /// How `a` and `b` are arranged.
        split: SplitDirection,
        /// The share `a` takes.
        ratio: Ratio,
        /// The left or top side.
        a: Box<Layout>,
        /// The right or bottom side.
        b: Box<Layout>,
    },
}

impl Layout {
    /// A tab holding one pane.
    pub const fn single(pane: PaneId) -> Self {
        Layout::Pane { pane }
    }

    /// Every pane, left to right and top to bottom.
    pub fn panes(&self) -> Vec<PaneId> {
        let mut panes = Vec::new();
        self.collect(&mut panes);
        panes
    }

    fn collect(&self, panes: &mut Vec<PaneId>) {
        match self {
            Layout::Pane { pane } => panes.push(*pane),
            Layout::Split { a, b, .. } => {
                a.collect(panes);
                b.collect(panes);
            }
        }
    }

    /// Whether `pane` is in this layout.
    pub fn contains(&self, pane: PaneId) -> bool {
        match self {
            Layout::Pane { pane: here } => *here == pane,
            Layout::Split { a, b, .. } => a.contains(pane) || b.contains(pane),
        }
    }

    /// Splits `target` in two, evenly: `target` keeps the `a` side and `new` takes `b`.
    pub fn split(
        &mut self,
        target: PaneId,
        direction: SplitDirection,
        new: PaneId,
    ) -> Result<(), LayoutError> {
        if self.contains(new) {
            return Err(LayoutError::DuplicatePane(new));
        }
        let leaf = self
            .leaf_mut(target)
            .ok_or(LayoutError::PaneNotFound(target))?;
        *leaf = Layout::Split {
            split: direction,
            ratio: Ratio::HALF,
            a: Box::new(Layout::single(target)),
            b: Box::new(Layout::single(new)),
        };
        Ok(())
    }

    fn leaf_mut(&mut self, target: PaneId) -> Option<&mut Layout> {
        match self {
            Layout::Pane { pane } if *pane == target => Some(self),
            Layout::Pane { .. } => None,
            Layout::Split { a, b, .. } => match a.leaf_mut(target) {
                Some(leaf) => Some(leaf),
                None => b.leaf_mut(target),
            },
        }
    }

    /// Removes `pane`; its sibling takes the space of their split. `None` when it was the
    /// last pane, which leaves the tab empty.
    pub fn remove(self, pane: PaneId) -> Result<Option<Layout>, LayoutError> {
        if self.contains(pane) {
            Ok(self.without(pane))
        } else {
            Err(LayoutError::PaneNotFound(pane))
        }
    }

    fn without(self, gone: PaneId) -> Option<Layout> {
        match self {
            Layout::Pane { pane } if pane == gone => None,
            Layout::Pane { .. } => Some(self),
            Layout::Split { split, ratio, a, b } => match (a.without(gone), b.without(gone)) {
                (Some(a), Some(b)) => Some(Layout::Split {
                    split,
                    ratio,
                    a: Box::new(a),
                    b: Box::new(b),
                }),
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn pane(n: u128) -> PaneId {
        PaneId::from(Uuid::from_u128(n))
    }

    mod layout {
        use super::*;

        mod json {
            use super::*;

            #[test]
            fn should_read_the_spec_shape() {
                let json = format!(
                    r#"{{"split":"h","ratio":0.25,"a":{{"pane":"{}"}},"b":{{"split":"v","ratio":0.5,"a":{{"pane":"{}"}},"b":{{"pane":"{}"}}}}}}"#,
                    pane(1),
                    pane(2),
                    pane(3)
                );
                let layout: Layout = serde_json::from_str(&json).expect("deserialize");
                assert_eq!(layout.panes(), [pane(1), pane(2), pane(3)]);
                assert_eq!(serde_json::to_string(&layout).expect("serialize"), json);
            }

            #[test]
            fn should_reject_a_ratio_outside_zero_to_one() {
                let json = format!(
                    r#"{{"split":"h","ratio":1.5,"a":{{"pane":"{}"}},"b":{{"pane":"{}"}}}}"#,
                    pane(1),
                    pane(2)
                );
                assert!(serde_json::from_str::<Layout>(&json).is_err());
            }

            #[test]
            fn should_reject_an_unknown_field() {
                let json = format!(r#"{{"pane":"{}","zoom":true}}"#, pane(1));
                assert!(serde_json::from_str::<Layout>(&json).is_err());
            }
        }

        mod split {
            use super::*;

            #[test]
            fn should_put_the_new_pane_after_the_target_evenly() {
                let mut layout = Layout::single(pane(1));
                layout
                    .split(pane(1), SplitDirection::Vertical, pane(2))
                    .expect("split");
                assert_eq!(
                    layout,
                    Layout::Split {
                        split: SplitDirection::Vertical,
                        ratio: Ratio::HALF,
                        a: Box::new(Layout::single(pane(1))),
                        b: Box::new(Layout::single(pane(2))),
                    }
                );
            }

            #[test]
            fn should_split_a_nested_pane_in_place() {
                let mut layout = Layout::single(pane(1));
                layout
                    .split(pane(1), SplitDirection::Horizontal, pane(2))
                    .expect("first split");
                layout
                    .split(pane(1), SplitDirection::Vertical, pane(3))
                    .expect("second split");
                assert_eq!(layout.panes(), [pane(1), pane(3), pane(2)]);
            }

            #[test]
            fn should_reject_a_target_that_is_not_in_the_layout() {
                let mut layout = Layout::single(pane(1));
                let err = layout
                    .split(pane(9), SplitDirection::Horizontal, pane(2))
                    .unwrap_err();
                assert_eq!(err, LayoutError::PaneNotFound(pane(9)));
            }

            #[test]
            fn should_reject_a_pane_that_is_already_in_the_layout() {
                let mut layout = Layout::single(pane(1));
                let err = layout
                    .split(pane(1), SplitDirection::Horizontal, pane(1))
                    .unwrap_err();
                assert_eq!(err, LayoutError::DuplicatePane(pane(1)));
            }
        }

        mod remove {
            use super::*;

            fn three_panes() -> Layout {
                let mut layout = Layout::single(pane(1));
                layout
                    .split(pane(1), SplitDirection::Horizontal, pane(2))
                    .expect("split 1");
                layout
                    .split(pane(2), SplitDirection::Vertical, pane(3))
                    .expect("split 2");
                layout
            }

            #[test]
            fn should_give_the_siblings_space_to_the_sibling() {
                let layout = three_panes().remove(pane(2)).expect("remove");
                assert_eq!(
                    layout,
                    Some(Layout::Split {
                        split: SplitDirection::Horizontal,
                        ratio: Ratio::HALF,
                        a: Box::new(Layout::single(pane(1))),
                        b: Box::new(Layout::single(pane(3))),
                    })
                );
            }

            #[test]
            fn should_leave_nothing_when_the_last_pane_goes() {
                assert_eq!(Layout::single(pane(1)).remove(pane(1)), Ok(None));
            }

            #[test]
            fn should_reject_a_pane_that_is_not_in_the_layout() {
                assert_eq!(
                    three_panes().remove(pane(9)),
                    Err(LayoutError::PaneNotFound(pane(9)))
                );
            }
        }
    }

    mod ratio {
        use super::*;

        #[test]
        fn should_reject_the_bounds_themselves() {
            assert_eq!(Ratio::new(0.0), Err(LayoutError::Ratio(0.0)));
            assert_eq!(Ratio::new(1.0), Err(LayoutError::Ratio(1.0)));
            assert!(Ratio::new(f32::NAN).is_err());
        }
    }
}
