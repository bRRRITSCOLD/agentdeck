//! The main window's content: what the daemon connection allows. Until it connects, a
//! centred status; once it does, the workspace shell (the sidebar and panes arrive with the
//! following #12 PRs).

use brrritscold_core_gpui::{
    ActiveTheme as _, Alert, AlertAction, AlertDescription, AlertTitle, AlertVariant, Button,
    ButtonSize, ButtonVariant, IconName,
};
use gpui::{
    Context, Entity, IntoElement, ParentElement as _, Render, Styled as _, Window, div, px,
};

use crate::connection::{Connection, Status};

/// The main window's root view, inside core-gpui's `Root`.
pub struct Shell {
    connection: Entity<Connection>,
}

impl Shell {
    /// A shell that follows `connection`.
    pub fn new(connection: Entity<Connection>, cx: &mut Context<Self>) -> Self {
        cx.observe(&connection, |_, _, cx| cx.notify()).detach();
        Shell { connection }
    }
}

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = cx.theme().semantic();
        let centred = div().size_full().flex().items_center().justify_center();
        match self.connection.read(cx).status() {
            Status::Connecting => centred
                .text_sm()
                .text_color(s.muted_foreground)
                .child("Connecting to agentdeckd…"),
            Status::Failed(reason) => {
                let connection = self.connection.clone();
                let retry = Button::new("retry")
                    .variant(ButtonVariant::Outline)
                    .size(ButtonSize::Sm)
                    .label("Retry")
                    .on_click(move |_, _, cx| {
                        connection.update(cx, |connection, cx| connection.connect(cx));
                    });
                centred.child(
                    Alert::new()
                        .variant(AlertVariant::Destructive)
                        .icon(IconName::Unplug)
                        .title(AlertTitle::new("agentdeck can't reach its daemon"))
                        .description(AlertDescription::new(reason.clone()))
                        .action(AlertAction::new().child(retry))
                        .w(px(480.)),
                )
            }
            Status::Connected(_) => centred,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use agentdeck_client::daemon::Daemon;
    use agentdeck_client::ws::Socket;
    use brrritscold_core_gpui::{Root, Theme, ThemePreference};
    use gpui::{App, AppContext as _, Task, TestAppContext, VisualTestContext};

    use super::*;
    use crate::connection::Connector;
    use crate::connection::fake::FakeConnector;

    /// A connect that never resolves, so the shell stays connecting.
    struct Pending;

    impl Connector for Pending {
        fn connect(&self, cx: &App) -> Task<anyhow::Result<(Daemon, Socket)>> {
            cx.background_spawn(std::future::pending())
        }
    }

    /// Opens a `Root` window on a [`Shell`] over a connection through `connector`, and draws
    /// it in light, then dark.
    fn draw_in_both_themes(connector: Rc<dyn Connector>, cx: &mut TestAppContext) {
        cx.update(brrritscold_core_gpui::init);
        let window = cx.add_window(|window, cx| {
            let connection = cx.new(|cx| Connection::new(connector, cx));
            let shell = cx.new(|cx| Shell::new(connection, cx));
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
}
