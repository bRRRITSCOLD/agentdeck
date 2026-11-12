//! Resize edges for a window whose decorations the client draws. The compositor gives such a
//! window no frame to grab, so thin strips along its inside edges hand a press to the
//! compositor as a resize. A side tiled against the screen or another window has none.

use gpui::{
    CursorStyle, InteractiveElement as _, IntoElement, MouseButton, Pixels, ResizeEdge,
    Styled as _, Tiling, div, px,
};

/// How far in from a side its resize strip reaches.
const EDGE: Pixels = px(4.);
/// The side of a corner's square, so a corner is easier to grab than a side.
const CORNER: Pixels = px(8.);

/// The strips to draw over the window's content, last, so they sit on top of it.
pub fn resize_edges(tiling: Tiling) -> impl Iterator<Item = impl IntoElement> {
    edges(tiling).into_iter().map(strip)
}

/// The edges a window tiled as `tiling` can be resized from: a side unless it is tiled, a
/// corner unless either of its sides is.
fn edges(tiling: Tiling) -> Vec<ResizeEdge> {
    let Tiling {
        top,
        left,
        right,
        bottom,
    } = tiling;
    [
        (ResizeEdge::Top, !top),
        (ResizeEdge::Right, !right),
        (ResizeEdge::Bottom, !bottom),
        (ResizeEdge::Left, !left),
        (ResizeEdge::TopLeft, !top && !left),
        (ResizeEdge::TopRight, !top && !right),
        (ResizeEdge::BottomRight, !bottom && !right),
        (ResizeEdge::BottomLeft, !bottom && !left),
    ]
    .into_iter()
    .filter_map(|(edge, free)| free.then_some(edge))
    .collect()
}

/// The strip for `edge`: placed and given its cursor, starting a resize on a press.
fn strip(edge: ResizeEdge) -> impl IntoElement {
    let strip = div().absolute().occlude();
    let strip = match edge {
        ResizeEdge::Top => strip
            .top_0()
            .left(CORNER)
            .right(CORNER)
            .h(EDGE)
            .cursor(CursorStyle::ResizeUpDown),
        ResizeEdge::Bottom => strip
            .bottom_0()
            .left(CORNER)
            .right(CORNER)
            .h(EDGE)
            .cursor(CursorStyle::ResizeUpDown),
        ResizeEdge::Left => strip
            .left_0()
            .top(CORNER)
            .bottom(CORNER)
            .w(EDGE)
            .cursor(CursorStyle::ResizeLeftRight),
        ResizeEdge::Right => strip
            .right_0()
            .top(CORNER)
            .bottom(CORNER)
            .w(EDGE)
            .cursor(CursorStyle::ResizeLeftRight),
        ResizeEdge::TopLeft => strip
            .top_0()
            .left_0()
            .size(CORNER)
            .cursor(CursorStyle::ResizeUpLeftDownRight),
        ResizeEdge::TopRight => strip
            .top_0()
            .right_0()
            .size(CORNER)
            .cursor(CursorStyle::ResizeUpRightDownLeft),
        ResizeEdge::BottomRight => strip
            .bottom_0()
            .right_0()
            .size(CORNER)
            .cursor(CursorStyle::ResizeUpLeftDownRight),
        ResizeEdge::BottomLeft => strip
            .bottom_0()
            .left_0()
            .size(CORNER)
            .cursor(CursorStyle::ResizeUpRightDownLeft),
    };
    strip.on_mouse_down(MouseButton::Left, move |_, window, cx| {
        cx.stop_propagation();
        window.start_window_resize(edge);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    mod edges {
        use super::*;

        fn tiled(top: bool, left: bool, right: bool, bottom: bool) -> Tiling {
            Tiling {
                top,
                left,
                right,
                bottom,
            }
        }

        #[test]
        fn should_offer_every_side_and_corner_when_nothing_is_tiled() {
            assert_eq!(edges(Tiling::default()).len(), 8);
        }

        #[test]
        fn should_offer_nothing_when_every_side_is_tiled() {
            assert!(edges(Tiling::tiled()).is_empty());
        }

        #[test]
        fn should_drop_a_tiled_side_and_both_its_corners() {
            let left_half = tiled(true, true, false, true);

            assert_eq!(edges(left_half), [ResizeEdge::Right]);
        }

        #[test]
        fn should_keep_the_corner_between_two_free_sides() {
            let top_left_quarter = tiled(true, true, false, false);

            assert_eq!(
                edges(top_left_quarter),
                [
                    ResizeEdge::Right,
                    ResizeEdge::Bottom,
                    ResizeEdge::BottomRight
                ]
            );
        }
    }
}
