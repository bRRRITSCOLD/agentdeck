//! The app's shortcuts (spec §11).
//!
//! GPUI matches a binding before the focused view sees the key, so a bound key never reaches
//! the terminal. Every binding is one a shell has no use for, after GNOME Terminal: ctrl-shift
//! chords (`secondary-shift-`, cmd-shift on macOS), ctrl-tab, ctrl-pageup / ctrl-pagedown and
//! `alt-<digit>`. Copy and paste are `secondary-shift-c` / `secondary-shift-v` in a terminal,
//! so `ctrl-c` and `ctrl-v` stay the shell's. Plain `ctrl-<letter>`, `alt-<letter>`, the
//! arrows and the paging keys stay the shell's.
//!
//! `secondary-` resolves per OS only through [`KeyBinding::load`], so every binding loads
//! that way.

use std::rc::Rc;

use gpui::{Action, App, KeyBinding, KeyBindingContextPredicate, actions};

/// The key context the workspace view sets; its shortcuts only apply inside it.
pub const WORKSPACE: &str = "Workspace";

/// The key context a terminal sets; copy and paste only apply inside it.
pub const TERMINAL: &str = "Terminal";

/// The key context an instruction being edited sets: Ctrl+Enter in its textarea saves
/// ([`Submit`]) and Escape cancels ([`CancelEdit`]), where Enter inserts a newline.
pub const INSTRUCTION_EDITOR: &str = "InstructionEditor";

/// The key context the task sheet sets around a title or description being edited: Ctrl+Enter
/// saves ([`Submit`]) and Escape cancels ([`CancelEdit`]), instead of closing the sheet. In
/// the title's input, Enter saves too.
pub const TASK_EDITOR: &str = "TaskEditor";

/// The key context the task sheet's comment composer sets: Ctrl+Enter in its textarea sends
/// ([`Submit`]), where Enter inserts a newline.
pub const COMMENT_COMPOSER: &str = "CommentComposer";

actions!(
    agentdeck,
    [
        /// Quits the app.
        Quit,
        /// Shows the next tab, wrapping to the first.
        NextTab,
        /// Shows the previous tab, wrapping to the last.
        PreviousTab,
        /// Focuses the next pane of the tab, left to right and top to bottom, wrapping.
        NextPane,
        /// Focuses the previous pane of the tab, wrapping.
        PreviousPane,
        /// Opens a tab with a shell in the workspace's dir.
        NewTab,
        /// Splits the focused pane with a shell on its right.
        SplitRight,
        /// Splits the focused pane with a shell below it.
        SplitDown,
        /// Closes the focused pane, asking first while it runs.
        ClosePane,
        /// Shows or hides the right panel beside a workspace.
        ToggleRightPanel,
        /// Copies the terminal's selection to the clipboard.
        Copy,
        /// Pastes the clipboard into the terminal.
        Paste,
        /// Saves or sends what a textarea holds: Ctrl+Enter where Enter is a newline.
        Submit,
        /// Drops an edit, leaving the text as it was.
        CancelEdit,
    ]
);

/// Shows the tab at this index, counting from 0, if there is one.
#[derive(Clone, Debug, PartialEq, Action)]
#[action(namespace = agentdeck, no_json)]
pub struct SelectTab(pub usize);

/// Binds the shortcuts and handles the app-wide actions; call once, before the first window.
pub fn init(cx: &mut App) {
    let bindings = bindings(cx);
    cx.bind_keys(bindings);
    cx.on_action(|_: &Quit, cx| cx.quit());
}

fn bindings(cx: &App) -> Vec<KeyBinding> {
    let context = |name: &str| {
        Some(Rc::new(
            KeyBindingContextPredicate::parse(name).expect("a context name parses"),
        ))
    };
    let workspace = context(WORKSPACE);
    let terminal = context(TERMINAL);
    // Bound on the textarea itself and loaded after core-gpui's, so they win over its own
    // `ctrl-enter` (a newline) and `escape`.
    let instruction_editor = context(&format!("{INSTRUCTION_EDITOR} > Input"));
    let comment_composer = context(&format!("{COMMENT_COMPOSER} > Input"));
    let task_editor = context(&format!("{TASK_EDITOR} > Input"));
    let mut table: Vec<(String, Box<dyn Action>, _)> = vec![
        ("secondary-shift-q".into(), Box::new(Quit), None),
        ("ctrl-pagedown".into(), Box::new(NextTab), workspace.clone()),
        ("ctrl-tab".into(), Box::new(NextTab), workspace.clone()),
        (
            "ctrl-pageup".into(),
            Box::new(PreviousTab),
            workspace.clone(),
        ),
        (
            "ctrl-shift-tab".into(),
            Box::new(PreviousTab),
            workspace.clone(),
        ),
        (
            "secondary-shift-down".into(),
            Box::new(NextPane),
            workspace.clone(),
        ),
        (
            "secondary-shift-up".into(),
            Box::new(PreviousPane),
            workspace.clone(),
        ),
        // After Terminator: e splits side by side, o stacks.
        (
            "secondary-shift-t".into(),
            Box::new(NewTab),
            workspace.clone(),
        ),
        (
            "secondary-shift-e".into(),
            Box::new(SplitRight),
            workspace.clone(),
        ),
        (
            "secondary-shift-o".into(),
            Box::new(SplitDown),
            workspace.clone(),
        ),
        (
            "secondary-shift-w".into(),
            Box::new(ClosePane),
            workspace.clone(),
        ),
        ("secondary-shift-c".into(), Box::new(Copy), terminal.clone()),
        ("secondary-shift-v".into(), Box::new(Paste), terminal),
        (
            "secondary-enter".into(),
            Box::new(Submit),
            instruction_editor.clone(),
        ),
        ("escape".into(), Box::new(CancelEdit), instruction_editor),
        ("secondary-enter".into(), Box::new(Submit), comment_composer),
        (
            "secondary-enter".into(),
            Box::new(Submit),
            task_editor.clone(),
        ),
        ("escape".into(), Box::new(CancelEdit), task_editor),
    ];
    table.extend((1..=9).map(|n| {
        let action: Box<dyn Action> = Box::new(SelectTab(n - 1));
        (format!("alt-{n}"), action, workspace.clone())
    }));
    table
        .into_iter()
        .map(|(keys, action, context)| {
            KeyBinding::load(&keys, action, context, false, None, cx)
                .expect("the keymap's keystrokes parse")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use gpui::prelude::FluentBuilder as _;
    use gpui::{
        Context, FocusHandle, InteractiveElement as _, IntoElement, ParentElement as _, Render,
        Styled as _, TestAppContext, VisualTestContext, Window, div,
    };

    use super::*;

    /// A focused view in the workspace context that records each shortcut action it gets, and
    /// each key that falls through to it.
    struct Recorder {
        focus: FocusHandle,
        seen: Rc<RefCell<Vec<String>>>,
        /// Whether the focused element is a terminal inside the workspace.
        terminal: bool,
    }

    impl Recorder {
        fn record<A: Action>(
            &self,
            name: impl Fn(&A) -> String + 'static,
        ) -> impl Fn(&A, &mut Window, &mut App) + 'static {
            let seen = self.seen.clone();
            move |action, _, _| seen.borrow_mut().push(name(action))
        }
    }

    impl Render for Recorder {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let seen = self.seen.clone();
            let focused = div()
                .when(self.terminal, |div| div.key_context(TERMINAL))
                .track_focus(&self.focus)
                .size_full()
                .on_key_down(move |event, _, cx| {
                    let key = event.keystroke.unparse(cx.platform_info().os);
                    seen.borrow_mut().push(format!("key {key}"));
                })
                .child("");
            div()
                .key_context(WORKSPACE)
                .size_full()
                .on_action(self.record(|_: &Quit| "quit".into()))
                .on_action(self.record(|_: &NextTab| "next tab".into()))
                .on_action(self.record(|_: &PreviousTab| "previous tab".into()))
                .on_action(self.record(|SelectTab(n): &SelectTab| format!("tab {n}")))
                .on_action(self.record(|_: &NextPane| "next pane".into()))
                .on_action(self.record(|_: &PreviousPane| "previous pane".into()))
                .on_action(self.record(|_: &NewTab| "new tab".into()))
                .on_action(self.record(|_: &SplitRight| "split right".into()))
                .on_action(self.record(|_: &SplitDown| "split down".into()))
                .on_action(self.record(|_: &ClosePane| "close pane".into()))
                .on_action(self.record(|_: &Copy| "copy".into()))
                .on_action(self.record(|_: &Paste| "paste".into()))
                .child(focused)
        }
    }

    /// What typing `keystrokes` into a terminal in the workspace did.
    fn typed(keystrokes: &str, cx: &mut TestAppContext) -> Vec<String> {
        typed_in(true, keystrokes, cx)
    }

    /// What typing `keystrokes` into the workspace did, in a terminal or not.
    fn typed_in(terminal: bool, keystrokes: &str, cx: &mut TestAppContext) -> Vec<String> {
        cx.update(init);
        let seen = Rc::new(RefCell::new(Vec::new()));
        let recorded = seen.clone();
        let (view, cx) = cx.add_window_view(|_, cx| Recorder {
            focus: cx.focus_handle(),
            seen: recorded,
            terminal,
        });
        let cx: &mut VisualTestContext = cx;
        cx.update(|window, cx| window.focus(&view.read(cx).focus.clone(), cx));
        cx.simulate_keystrokes(keystrokes);
        seen.take()
    }

    mod bindings {
        use super::*;

        #[gpui::test]
        fn should_quit_on_secondary_shift_q(cx: &mut TestAppContext) {
            assert_eq!(typed("secondary-shift-q", cx), ["quit"]);
        }

        #[gpui::test]
        fn should_page_through_tabs(cx: &mut TestAppContext) {
            assert_eq!(
                typed("ctrl-pagedown ctrl-tab ctrl-pageup ctrl-shift-tab", cx),
                ["next tab", "next tab", "previous tab", "previous tab"]
            );
        }

        #[gpui::test]
        fn should_select_tabs_one_to_nine_by_alt_digit(cx: &mut TestAppContext) {
            assert_eq!(typed("alt-1 alt-9", cx), ["tab 0", "tab 8"]);
        }

        #[gpui::test]
        fn should_move_between_panes(cx: &mut TestAppContext) {
            assert_eq!(
                typed("secondary-shift-down secondary-shift-up", cx),
                ["next pane", "previous pane"]
            );
        }

        #[gpui::test]
        fn should_open_split_and_close_panes(cx: &mut TestAppContext) {
            assert_eq!(
                typed(
                    "secondary-shift-t secondary-shift-e secondary-shift-o secondary-shift-w",
                    cx
                ),
                ["new tab", "split right", "split down", "close pane"]
            );
        }

        #[gpui::test]
        fn should_copy_and_paste_in_a_terminal(cx: &mut TestAppContext) {
            assert_eq!(
                typed("secondary-shift-c secondary-shift-v", cx),
                ["copy", "paste"]
            );
        }

        #[gpui::test]
        fn should_leave_copy_and_paste_keys_alone_outside_a_terminal(cx: &mut TestAppContext) {
            let seen = typed_in(false, "secondary-shift-c", cx);

            assert!(
                matches!(&seen[..], [key] if key.starts_with("key ")),
                "{seen:?}"
            );
        }

        #[gpui::test]
        fn should_leave_the_shells_keys_alone(cx: &mut TestAppContext) {
            let keys = "ctrl-c ctrl-q ctrl-t ctrl-w alt-b alt-0 down pagedown ctrl-down";

            assert_eq!(
                typed(keys, cx),
                keys.split(' ')
                    .map(|key| format!("key {key}"))
                    .collect::<Vec<_>>()
            );
        }
    }
}
