//! The vertical scrollbar every scrolling list shows (spec §11): the Pull requests view,
//! Settings and the sidebar's workspaces. Penpot draws it as shadcn's `ScrollArea` thumb
//! (`bg-border rounded-full`): a 10px track at the area's right edge holding an 8px thumb.
//!
//! It never sits over a row. The Pull requests view puts it just right of its table, in the
//! content's padding ([`table_of`](crate::table::table_of)). The sidebar's workspace list
//! reaches its container's right edge and leaves a [`GUTTER`] there, past its rows, for the
//! track to sit in.
//!
//! It is `gpui-base`'s [`Scrollbar`], dressed to match. Neither core-react nor core-gpui has a
//! `ScrollArea` yet, so it stays here until one is promoted through core-react.

use brrritscold_core_gpui::ActiveTheme as _;
use brrritscold_core_gpui_tokens::primitive::{border_radius, space};
use gpui::{App, Pixels, px};
use gpui_base::{Scrollbar, ScrollbarHandle, ScrollbarMode};

/// The track's width. Raw in Penpot too, as shadcn's `w-2.5`: no sizing token is 10px.
const TRACK: Pixels = px(10.);

/// The thumb's width, the track less shadcn's `p-px` on either side.
const THUMB: Pixels = px(8.);

/// The room a list leaves at its right edge for the scrollbar: the track, flush with the
/// edge, and `space.0-5` clear of the rows. Penpot pads the sidebar's `Workspaces` with
/// `space.3` on the right for it.
pub const GUTTER: Pixels = space::S3;

/// A vertical scrollbar over the area `handle` scrolls, shown while the content is taller
/// than the area. Add it after the area, so it paints on top; it sits over the area's right
/// edge wherever it is laid out, so the area's content leaves a [`GUTTER`] for it.
#[track_caller]
pub fn vertical<H: ScrollbarHandle + Clone>(handle: &H, cx: &App) -> Scrollbar {
    let border = cx.theme().semantic().border;
    Scrollbar::vertical(handle)
        // Always, not only while scrolling: it is how a list says it is longer than the window.
        .mode(ScrollbarMode::Always)
        .styles(|styles| {
            styles.track(|track| track.width(TRACK)).thumb(|thumb| {
                thumb
                    .bg(border)
                    .width(THUMB)
                    .inset((TRACK - THUMB) / 2.)
                    .radius(border_radius::FULL)
            })
        })
}

/// Finds the scrollbar's thumb in what a window last drew.
#[cfg(test)]
pub mod testing {
    use brrritscold_core_gpui::ActiveTheme as _;
    use gpui::{Background, Bounds, Pixels, VisualTestContext, px};

    use super::TRACK;

    /// The thumb drawn over `area`'s right edge in the last frame, if any.
    pub fn thumb(area: Bounds<Pixels>, cx: &mut VisualTestContext) -> Option<Bounds<Pixels>> {
        cx.update(|window, cx| {
            let border = Background::from(cx.theme().semantic().border);
            let scale = window.scale_factor();
            window
                .painted_quads()
                .into_iter()
                .filter(|quad| quad.background == border)
                .map(|quad| quad.bounds.map(|v| px(v.0 / scale)))
                .find(|b| b.right() <= area.right() && b.left() >= area.right() - TRACK)
        })
    }
}

#[cfg(test)]
mod tests {
    use brrritscold_core_gpui::{Theme, ThemePreference};
    use gpui::{
        Context, InteractiveElement as _, IntoElement, ParentElement as _, Render, ScrollDelta,
        ScrollEvent, ScrollHandle, StatefulInteractiveElement as _, Styled as _, TestAppContext,
        VisualTestContext, Window, div, point, px,
    };

    use super::testing::thumb;
    use super::*;

    /// A 200px area of `rows` 40px rows.
    struct Area {
        rows: usize,
        scroll: ScrollHandle,
    }

    impl Render for Area {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .child(
                    div()
                        .id("area")
                        .debug_selector(|| "area".into())
                        .w(px(200.))
                        .h(px(200.))
                        .flex()
                        .flex_col()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .children((0..self.rows).map(|_| div().flex_none().h(px(40.)))),
                )
                .child(vertical(&self.scroll, cx))
        }
    }

    fn open(rows: usize, cx: &mut TestAppContext) -> &mut VisualTestContext {
        cx.update(brrritscold_core_gpui::init);
        let (_, cx) = cx.add_window_view(|_, _| Area {
            rows,
            scroll: ScrollHandle::new(),
        });
        draw(cx);
        cx
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    fn area(cx: &mut VisualTestContext) -> gpui::Bounds<gpui::Pixels> {
        cx.debug_bounds("area").expect("the area")
    }

    #[gpui::test]
    fn should_show_a_scrollbar_when_the_list_overflows(cx: &mut TestAppContext) {
        let cx = open(20, cx);

        let area = area(cx);
        let thumb = thumb(area, cx).expect("a thumb");
        // An 8px thumb, centred in the 10px track at the area's right edge.
        assert_eq!(thumb.size.width, px(8.));
        assert_eq!(thumb.right(), area.right() - px(1.));
    }

    #[test]
    fn should_leave_the_track_and_a_gap_in_the_gutter() {
        assert_eq!(GUTTER, TRACK + px(2.));
    }

    #[gpui::test]
    fn should_hide_the_scrollbar_when_everything_fits(cx: &mut TestAppContext) {
        let cx = open(3, cx);

        let area = area(cx);
        assert_eq!(thumb(area, cx), None);
    }

    #[gpui::test]
    fn should_move_the_thumb_when_the_list_scrolls(cx: &mut TestAppContext) {
        let cx = open(20, cx);
        let area = area(cx);
        let before = thumb(area, cx).expect("a thumb");

        cx.simulate_event(ScrollEvent {
            position: area.center(),
            delta: ScrollDelta::Pixels(point(px(0.), px(-200.))),
            ..ScrollEvent::default()
        });
        draw(cx);

        let after = thumb(area, cx).expect("a thumb");
        assert!(after.origin.y > before.origin.y);
    }

    #[gpui::test]
    fn should_fill_the_thumb_with_the_border_colour_in_both_themes(cx: &mut TestAppContext) {
        let cx = open(20, cx);

        for preference in [ThemePreference::Light, ThemePreference::Dark] {
            cx.update(|_, cx| Theme::set_preference(preference, cx));
            draw(cx);

            let area = area(cx);
            assert!(thumb(area, cx).is_some(), "{preference:?}");
        }
    }
}
