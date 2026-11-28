// SPDX-License-Identifier: GPL-3.0-or-later
//! The review's keyboard shortcuts, shared by both interfaces: each one turns its
//! own key events into a [`Key`], looks up the [`Command`] here, and carries it out.
//! The help each interface shows comes from [`HELP`], next to the bindings, so the
//! two can't drift apart.

use crate::diff::Decision;

/// A key press, whatever interface it came from. Letters keep their case: Shift+a
/// is `Char('A')`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Char(char),
    Enter,
    Esc,
    Tab,
    BackTab,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
}

/// What a shortcut does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command {
    Next,
    Prev,
    First,
    Last,
    NextFile,
    PrevFile,
    NextPending,
    /// Decide the selected hunk.
    Decide(Decision),
    /// Act on the whole file.
    File(Decision),
    Clear,
    ApproveAllPending,
    ToggleFull,
    ToggleSplit,
    ToggleFold,
    /// Enter or leave line mode.
    ToggleLines,
    /// In line mode: move the picked line, or extend the pick.
    MoveLine {
        down: bool,
        extend: bool,
    },
    /// In line mode: comment on the picked lines.
    CommentLines,
    LeaveLines,
    AgentMessage,
    /// Write or change the note for the selected hunk or its file.
    Note,
    /// The general message to the agent.
    GeneralNote,
    ScrollPage {
        down: bool,
    },
    ScrollSideways {
        right: bool,
    },
    ScrollHome,
    Help,
    /// Submit, asking first.
    AskSubmit,
    /// Submit at once.
    Submit,
    Quit,
}

/// The command for `key` (`ctrl` when Ctrl, or Cmd on macOS, is held). In line mode
/// some keys move between lines instead; the rest keep their usual meaning.
pub fn command(key: Key, ctrl: bool, line_mode: bool) -> Option<Command> {
    use Command as C;
    use Key as K;
    if ctrl {
        return match key {
            K::Enter | K::Char('s') => Some(C::Submit),
            K::Char('a') => Some(C::ApproveAllPending),
            K::Char('d') => Some(C::ScrollPage { down: true }),
            K::Char('u') => Some(C::ScrollPage { down: false }),
            K::Char('c') => Some(C::Quit),
            _ => None,
        };
    }
    if line_mode {
        let line = match key {
            K::Char('j') | K::Down => Some(C::MoveLine {
                down: true,
                extend: false,
            }),
            K::Char('k') | K::Up => Some(C::MoveLine {
                down: false,
                extend: false,
            }),
            K::Char('J') => Some(C::MoveLine {
                down: true,
                extend: true,
            }),
            K::Char('K') => Some(C::MoveLine {
                down: false,
                extend: true,
            }),
            K::Char('c' | 'i') | K::Enter => Some(C::CommentLines),
            K::Esc => Some(C::LeaveLines),
            _ => None,
        };
        if line.is_some() {
            return line;
        }
    }
    Some(match key {
        K::Char('j') | K::Down => C::Next,
        K::Char('k') | K::Up => C::Prev,
        K::Char('g') | K::Home => C::First,
        K::Char('G') | K::End => C::Last,
        K::Char(']') | K::Tab => C::NextFile,
        K::Char('[') | K::BackTab => C::PrevFile,
        K::Char('n') => C::NextPending,
        K::Char('a') => C::Decide(Decision::Approve),
        K::Char('r') => C::Decide(Decision::Reject),
        K::Char('e') => C::Decide(Decision::Edit),
        K::Char('c') => C::Decide(Decision::Comment),
        K::Char('A') => C::File(Decision::Approve),
        K::Char('R') => C::File(Decision::Reject),
        K::Char('C') => C::File(Decision::Comment),
        K::Char('x') => C::Clear,
        K::Char('v') => C::ToggleLines,
        K::Char('f') => C::ToggleFull,
        K::Char('s') => C::ToggleSplit,
        K::Char(' ') => C::ToggleFold,
        K::Char('t') => C::AgentMessage,
        K::Enter | K::Char('i') => C::Note,
        K::Char('m') => C::GeneralNote,
        K::Char('h') | K::Left => C::ScrollSideways { right: false },
        K::Char('l') | K::Right => C::ScrollSideways { right: true },
        K::Char('0') => C::ScrollHome,
        K::PageDown => C::ScrollPage { down: true },
        K::PageUp => C::ScrollPage { down: false },
        K::Char('?') => C::Help,
        // Next to A (approve file), so it asks first. Ctrl+S submits at once.
        K::Char('S') => C::AskSubmit,
        K::Char('q') => C::Quit,
        _ => return None,
    })
}

/// Which interface a row of [`HELP`] applies to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Where {
    Both,
    Tui,
    Gui,
}

/// One row of the help: the keys, what they do, and where.
pub struct HelpRow {
    pub keys: &'static str,
    pub what: &'static str,
    pub on: Where,
}

const fn row(keys: &'static str, what: &'static str, on: Where) -> HelpRow {
    HelpRow { keys, what, on }
}

/// Every shortcut, for the help. Rows whose keys start with two spaces belong to
/// the row above them (line mode).
pub const HELP: &[HelpRow] = {
    use Where::*;
    &[
        row("j / k, ↓ / ↑", "next / previous hunk", Both),
        row("] / [, Tab", "next / previous file", Both),
        row("g / G", "first / last hunk", Both),
        row("n", "next undecided hunk", Both),
        row("a", "approve hunk (stage it)", Both),
        row("r", "reject hunk (revert it) and give a reason", Both),
        row("e", "edit the hunk", Both),
        row("c", "comment on the hunk", Both),
        row(
            "v",
            "line mode: pick lines to comment on (or click a line)",
            Both,
        ),
        row("  j / k", "  move to the next / previous line", Both),
        row("  J / K", "  extend the picked lines down / up", Both),
        row("  c, Enter", "  comment on the picked lines", Both),
        row("  Esc", "  leave line mode", Both),
        row("x", "clear the hunk's decision", Both),
        row(
            "A / R / C",
            "approve / reject / comment on the whole file",
            Both,
        ),
        row(
            "Enter, i",
            "write or change the note for the hunk or file",
            Tui,
        ),
        row("m", "general message to the agent", Tui),
        row("t", "what the agent said", Both),
        row("Ctrl+A", "approve every pending hunk", Both),
        row("f", "full file: show unchanged code between hunks", Both),
        row("s", "split view: old and new side by side", Both),
        row("Space", "fold / unfold the file", Both),
        row("h / l, ← / →, 0", "scroll sideways / back", Tui),
        row("Ctrl+D / Ctrl+U", "scroll half a page", Tui),
        row("S", "submit: apply and send the review (asks first)", Tui),
        row("Ctrl+S, Ctrl+Enter", "submit without asking", Both),
        row("q, Ctrl+C", "quit without applying anything", Tui),
        row("Esc", "leave a text box", Gui),
    ]
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_mode_takes_over_only_its_own_keys() {
        let down = Command::MoveLine {
            down: true,
            extend: false,
        };
        assert_eq!(command(Key::Char('j'), false, true), Some(down));
        assert_eq!(command(Key::Char('j'), false, false), Some(Command::Next));
        assert_eq!(
            command(Key::Enter, false, true),
            Some(Command::CommentLines)
        );
        assert_eq!(command(Key::Enter, false, false), Some(Command::Note));
        // Not a line-mode key: same as outside it.
        assert_eq!(
            command(Key::Char('a'), false, true),
            Some(Command::Decide(Decision::Approve))
        );
    }

    #[test]
    fn ctrl_keys_have_their_own_meaning() {
        assert_eq!(command(Key::Char('s'), true, false), Some(Command::Submit));
        assert_eq!(
            command(Key::Char('s'), false, false),
            Some(Command::ToggleSplit)
        );
        assert_eq!(command(Key::Char('j'), true, true), None);
    }

    #[test]
    fn every_help_key_is_bound() {
        // The first key of each row, as typed.
        for r in HELP {
            let first = r.keys.trim().split([',', ' ']).next().unwrap();
            let key = match first {
                "Enter" => Key::Enter,
                "Esc" | "Space" | "Tab" => continue,
                k if k.starts_with("Ctrl+") => {
                    let c = k["Ctrl+".len()..].chars().next().unwrap();
                    assert!(
                        command(Key::Char(c.to_ascii_lowercase()), true, false).is_some(),
                        "{k}"
                    );
                    continue;
                }
                k => Key::Char(k.chars().next().unwrap()),
            };
            let line = r.keys.starts_with("  ");
            assert!(command(key, false, line).is_some(), "{}", r.keys);
        }
    }
}
