# CLAUDE.md

## Working rules

- **Never commit on your own.** Leave changes uncommitted until the user asks for a
  commit, and never push unless asked.
- **Reinstall verdict after every change to the code.** The user reviews your work
  in verdict's own window, which the Claude Code hook runs from
  `~/.cargo/bin/verdict`. Run `cargo install --path .` before ending a turn that
  changed code, so the window that opens shows the new build. If the install
  fails, fix it or say so; don't end the turn on a stale binary silently.
