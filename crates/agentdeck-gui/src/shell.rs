//! The main window's content: what the daemon connection allows. Until it connects, a
//! centred status; once it does, the sidebar beside the selected view (the panes arrive with
//! the following #12 PRs). When the compositor leaves decorations to the app, a title bar and
//! resize edges frame it.

use brrritscold_core_gpui::{
    ActiveTheme as _, Alert, AlertAction, AlertDescription, AlertTitle, AlertVariant, Button,
    ButtonSize, ButtonVariant, IconName, Separator, SeparatorOrientation,
};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Decorations, Entity, FocusHandle,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _, Window, div, px,
};

use crate::connection::{Connection, Status};
use crate::frame;
use crate::sidebar::SidebarView;
use crate::title_bar::{Close, Minimize, TitleBar, ToggleMaximize};

/// The main window's root view, inside core-gpui's `Root`.
pub struct Shell {
    connection: Entity<Connection>,
    /// The sidebar, while connected; a new connection gets a new one.
    sidebar: Option<Entity<SidebarView>>,
    /// Held by the shell so the title bar's actions reach its handlers while nothing inside
    /// has focus.
    focus: FocusHandle,
}

impl Shell {
    /// A shell that follows `connection`, focused in `window`.
    pub fn new(
        connection: Entity<Connection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&connection, |shell, _, cx| {
            shell.follow_connection(cx);
            cx.notify();
        })
        .detach();
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut shell = Shell {
            connection,
            sidebar: None,
            focus,
        };
        shell.follow_connection(cx);
        shell
    }

    /// Opens a sidebar on a new session, and drops it once the connection is lost.
    fn follow_connection(&mut self, cx: &mut Context<Self>) {
        match self.connection.read(cx).status() {
            Status::Connected(session) => {
                if self.sidebar.is_none() {
                    let api = session.api.clone();
                    let connection = self.connection.clone();
                    self.sidebar = Some(cx.new(|cx| SidebarView::new(api, &connection, cx)));
                }
            }
            Status::Connecting | Status::Failed(_) => self.sidebar = None,
        }
    }

    /// What the connection allows, filling the space under the title bar.
    fn content(&self, cx: &mut Context<Self>) -> AnyElement {
        let s = cx.theme().semantic();
        let centred = div().flex_1().flex().items_center().justify_center();
        match self.connection.read(cx).status() {
            Status::Connecting => centred
                .text_sm()
                .text_color(s.muted_foreground)
                .child("Connecting to agentdeckd…")
                .into_any_element(),
            Status::Failed(reason) => {
                let connection = self.connection.clone();
                let retry = Button::new("retry")
                    .variant(ButtonVariant::Outline)
                    .size(ButtonSize::Sm)
                    .label("Retry")
                    .on_click(move |_, _, cx| {
                        connection.update(cx, |connection, cx| connection.connect(cx));
                    });
                centred
                    .child(
                        Alert::new()
                            .variant(AlertVariant::Destructive)
                            .icon(IconName::Unplug)
                            .title(AlertTitle::new("agentdeck can't reach its daemon"))
                            .description(AlertDescription::new(reason.clone()))
                            .action(AlertAction::new().child(retry))
                            .w(px(480.)),
                    )
                    .into_any_element()
            }
            // Penpot "Main window" `Body`: Sidebar | Separator | the selected view.
            Status::Connected(_) => div()
                .flex_1()
                .min_h_0()
                .flex()
                .children(self.sidebar.clone())
                .child(Separator::new().orientation(SeparatorOrientation::Vertical))
                .child(div().flex_1())
                .into_any_element(),
        }
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let client_tiling = match window.window_decorations() {
            Decorations::Client { tiling } => Some(tiling),
            Decorations::Server => None,
        };
        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .track_focus(&self.focus)
            .on_action(|_: &Minimize, window, _| window.minimize_window())
            .on_action(|_: &ToggleMaximize, window, _| window.zoom_window())
            .on_action(|_: &Close, window, _| window.remove_window())
            // Penpot "Main window": TitleBar / Separator / Body.
            .when(client_tiling.is_some(), |shell| {
                shell
                    .child(TitleBar::new("agentdeck"))
                    .child(Separator::new())
            })
            .child(self.content(cx))
            .when_some(client_tiling, |shell, tiling| {
                shell.children(frame::resize_edges(tiling))
            })
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;
    use std::sync::Arc;

    use agentdeck_client::ws::Socket;
    use brrritscold_core_gpui::{Root, Theme, ThemePreference};
    use gpui::{App, Task, TestAppContext, VisualTestContext};

    use super::*;
    use crate::api::Api;
    use crate::connection::Connector;
    use crate::connection::fake::FakeConnector;

    /// A connect that never resolves, so the shell stays connecting.
    struct Pending;

    impl Connector for Pending {
        fn connect(&self, cx: &App) -> Task<anyhow::Result<(Arc<dyn Api>, Socket)>> {
            cx.background_spawn(std::future::pending())
        }
    }

    /// Opens a `Root` window on a [`Shell`] over a connection through `connector`, and draws
    /// it in light, then dark.
    fn draw_in_both_themes(connector: Rc<dyn Connector>, cx: &mut TestAppContext) {
        cx.update(brrritscold_core_gpui::init);
        let window = cx.add_window(|window, cx| {
            let connection = cx.new(|cx| Connection::new(connector, cx));
            let shell = cx.new(|cx| Shell::new(connection, window, cx));
            Root::new(shell, window, cx)
        });
        let cx = &mut VisualTestContext::from_window(*window, cx);
        for preference in [ThemePreference::Light, ThemePreference::Dark] {
            cx.update(|_, cx| Theme::set_preference(preference, cx));
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }
    }

    mod render {
        use super::*;

        #[gpui::test]
        fn should_draw_while_connecting_in_both_themes(cx: &mut TestAppContext) {
            draw_in_both_themes(Rc::new(Pending), cx);
        }

        #[gpui::test]
        fn should_draw_a_failure_in_both_themes(cx: &mut TestAppContext) {
            draw_in_both_themes(Rc::new(FakeConnector::default().then_fail("refused")), cx);
        }

        #[gpui::test]
        fn should_draw_once_connected_in_both_themes(cx: &mut TestAppContext) {
            let connector = Rc::new(FakeConnector::default());
            let _remote = connector.then_connect();

            draw_in_both_themes(connector, cx);
        }
    }

    mod window_actions {
        use super::*;
        use crate::title_bar::Close;

        #[gpui::test]
        fn should_remove_the_window_on_close(cx: &mut TestAppContext) {
            cx.update(brrritscold_core_gpui::init);
            let window = cx.add_window(|window, cx| {
                let connection = cx.new(|cx| Connection::new(Rc::new(Pending), cx));
                let shell = cx.new(|cx| Shell::new(connection, window, cx));
                Root::new(shell, window, cx)
            });
            let handle = window.into();
            let cx = &mut VisualTestContext::from_window(handle, cx);
            cx.update(|window, cx| window.draw(cx).clear(cx));

            cx.dispatch_action(Close);
            cx.run_until_parked();

            assert!(!cx.windows().contains(&handle));
        }
    }
}
