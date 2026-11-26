// SPDX-License-Identifier: GPL-3.0-or-later
//! Owning the terminal: raw mode and the alternate screen on /dev/tty, and
//! handing it to $EDITOR for a while.

use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::{cursor, execute, terminal};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::fs::File;

pub(super) type Term = Terminal<CrosstermBackend<File>>;

pub(super) struct Tty {
    pub term: Term,
    kbd: bool,
}

impl Tty {
    pub fn open() -> Result<Tty, String> {
        let mut t = Tty {
            term: Self::new_term().map_err(|e| format!("no terminal available: {e}"))?,
            kbd: false,
        };
        t.enter().map_err(|e| e.to_string())?;
        Ok(t)
    }

    fn new_term() -> std::io::Result<Term> {
        let out = File::options().read(true).write(true).open("/dev/tty")?;
        Terminal::new(CrosstermBackend::new(out))
    }

    pub fn enter(&mut self) -> std::io::Result<()> {
        terminal::enable_raw_mode()?;
        execute!(
            self.term.backend_mut(),
            terminal::EnterAlternateScreen,
            EnableMouseCapture,
            terminal::Clear(terminal::ClearType::All)
        )?;
        // Lets us tell Ctrl+Enter from Enter, where the terminal supports it. crossterm
        // sends the query to stdout, so only ask when stdout is the terminal.
        use std::io::IsTerminal;
        self.kbd = std::io::stdout().is_terminal()
            && matches!(terminal::supports_keyboard_enhancement(), Ok(true));
        if self.kbd {
            execute!(
                self.term.backend_mut(),
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )?;
        }
        // A fresh Terminal redraws everything on the next frame. (Terminal::clear would
        // ask the terminal for the cursor position, again through stdout.)
        self.term = Self::new_term()?;
        Ok(())
    }

    pub fn leave(&mut self) {
        if self.kbd {
            let _ = execute!(self.term.backend_mut(), PopKeyboardEnhancementFlags);
        }
        let _ = execute!(
            self.term.backend_mut(),
            DisableMouseCapture,
            terminal::LeaveAlternateScreen,
            cursor::Show
        );
        let _ = terminal::disable_raw_mode();
    }
}

impl Drop for Tty {
    fn drop(&mut self) {
        self.leave();
    }
}

/// Restore the terminal before a panic message is printed, or it's lost.
pub(super) fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Ok(mut tty) = File::options().write(true).open("/dev/tty") {
            let _ = execute!(
                tty,
                DisableMouseCapture,
                terminal::LeaveAlternateScreen,
                cursor::Show
            );
        }
        let _ = terminal::disable_raw_mode();
        prev(info);
    }));
}

/// Edit `text` in $VISUAL / $EDITOR (default vi). An error if the editor failed.
pub(super) fn run_editor(tty: &mut Tty, text: &str, ext: &str) -> Result<String, String> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let ext = if ext.is_empty() { "txt" } else { ext };
    let path = std::env::temp_dir().join(format!(
        "verdict-{}-{}.{ext}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    let editor = ["VISUAL", "EDITOR"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "vi".into());
    tty.leave();
    let tty_io = |w: bool| File::options().read(!w).write(w).open("/dev/tty");
    let status = (|| {
        // Through sh, so EDITOR may carry arguments ("code --wait"), as git allows.
        std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("{editor} \"$1\""))
            .arg("sh")
            .arg(&path)
            .stdin(tty_io(false)?)
            .stdout(tty_io(true)?)
            .stderr(tty_io(true)?)
            .status()
    })();
    let _ = tty.enter();
    let out = std::fs::read_to_string(&path);
    let _ = std::fs::remove_file(&path);
    match status {
        Ok(s) if s.success() => out.map_err(|e| e.to_string()),
        Ok(s) => Err(format!("{editor} exited with {s}")),
        Err(e) => Err(format!("could not run {editor}: {e}")),
    }
}
