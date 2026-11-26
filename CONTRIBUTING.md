# Contributing to verdict

Thanks for helping. Bug reports, fixes, docs and new features are all welcome.
This guide covers how to report problems, how the code is organized, what a pull
request needs before it's merged, and the development tools that verdict's users
never need.

## Reporting a bug

Open a GitHub issue that includes:

- **What you ran and what happened.** Say what you expected instead.
- **Your setup.** The verdict version or commit, your OS (and whether it's WSL2),
  X11 or Wayland, and which interface you used (`--gui` or `--tui`).
- **How verdict started.** Standalone (`verdict`, `--staged`, a range) or from the
  Claude Code hook. For hook problems, include your `hooks` section of
  `settings.json`.
- **For highlighting problems:** the file's language (`verdict lang FILE`) and any
  warnings `verdict langs` prints for it. If it's a grammar from an extension,
  say which one.
- **A small repo that shows the problem**, if you can make one. A diff that breaks
  verdict is often enough on its own.

## Before you start on a change

- **Small fixes** (bugs, typos, docs) can go straight to a pull request.
- **Bigger changes** (new features, new config options, new key bindings,
  changes to the feedback message format) should start as an issue, so we can
  agree on the approach before you write it. The [Roadmap](README.md#roadmap)
  lists what's already planned.

## Setting up

You'll need `git`, Rust 1.85 or newer (edition 2024) and a C compiler (for
Oniguruma). The window on Linux also needs `libxkbcommon-x11-0` for X11.

```sh
git clone https://github.com/ZamboniL/verdict
cd verdict
cargo build
cargo run -- .         # review this checkout's own uncommitted changes
cargo run -- --tui .   # the same, in the terminal
```

To try the Claude Code hook with your local build, run `cargo install --path .`
and set up the hooks as described in the README.

## How the code is organized

| File | What it does |
|---|---|
| `src/main.rs` | CLI, Claude Code hook protocol |
| `src/git.rs` | thin wrapper over the `git` CLI (no libgit2) |
| `src/model.rs` | diffs, hunks, applying decisions, the feedback message |
| `src/session.rs` | per-session turn snapshots, transcript analysis |
| `src/review.rs` | review state shared by both interfaces: decisions, selection, folds |
| `src/draft.rs` | saving unsubmitted reviews and restoring them |
| `src/frontend.rs` | choosing the window or the terminal, the background highlighter |
| `src/gui.rs` | the egui review window |
| `src/tui.rs` | the ratatui terminal interface |
| `src/highlight.rs` | language and theme registry, user extensions, caching |
| `src/tm.rs` | TextMate grammar and VS Code theme conversion |

Some rules that keep it this way:

- **Review behavior belongs in `review.rs`, not in an interface.** The window
  and the terminal UI are two views of the same review. A new action or decision
  rule goes in `review.rs` and is then wired into both `gui.rs` and `tui.rs`.
  A feature that only works in one interface needs a good reason, written in the
  pull request.
- **Keys are the same in both interfaces.** A new or changed key goes in the
  README's key tables and in the terminal's `?` help and hint bar.
- **Git goes through `git.rs`,** which calls the `git` CLI. Don't add libgit2 or
  other git libraries.
- **Nothing on disk changes until submit.** Staging and reverting happen only
  when the review is submitted, and reviews of `--staged` changes or commit
  ranges never change git at all. Keep it that way.
- **Both features have to build alone.** `gui` and `tui` are Cargo features, and
  people build without the window (`--no-default-features --features tui`).
  Put interface-specific code behind the matching `#[cfg(feature = ...)]`.

## Tests

Unit tests live in a `#[cfg(test)]` module at the bottom of each file. The
`model.rs` and `session.rs` tests build real git repositories in a temp
directory, so they exercise actual `git` behavior. Follow that pattern for
anything that touches git.

Add tests when you change how diffs are parsed, how decisions are applied, the
feedback message, session tracking, or grammar conversion. A bug fix should come
with a test that fails without it.

## Before you open a pull request

Run all of these. Each must pass with **no warnings**:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --no-default-features --features tui -- -D warnings
cargo clippy --all-targets --no-default-features --features gui -- -D warnings
cargo test
```

The code is formatted with `rustfmt` and linted with `clippy`, both with their
default settings. Run `cargo fmt` to fix formatting. Clippy checks each feature
set separately because code behind `#[cfg(feature = ...)]` is only linted when
that feature is on. If you think a clippy lint is wrong for a specific piece of
code, silence it there with `#[allow(clippy::...)]` and add a comment saying why.

CI runs the same checks on every pull request
([`.github/workflows/ci.yml`](.github/workflows/ci.yml)).

The actions the workflows use are locked to exact commits in
`.github/workflows/actions.lock`, and CI fails if it's out of date. Dependabot
bumps the actions and updates the lockfile with them. If you add or change a
`uses:` line yourself, regenerate it with the
[`gh actions-lock`](https://github.com/github/gh-actions-lock) extension:

```sh
gh extension install github/gh-actions-lock
gh actions-lock
```

The commit that first ran `rustfmt` over everything is listed in
`.git-blame-ignore-revs`. GitHub's blame view skips it automatically. For local
`git blame`, run once:

```sh
git config blame.ignoreRevsFile .git-blame-ignore-revs
```

Also:

- **UI changes need a picture.** For the window, use the
  [scripted screenshots](#scripted-screenshots) below. For the terminal, add a
  screenshot or a short recording.
- **Update the README** if you changed something users see: a flag, a key,
  a config option, the feedback message, or a limitation.

## Commits and pull requests

- Keep each pull request to one concern. A fix and an unrelated refactor are
  two pull requests.
- Write commit summaries in the imperative, like the existing history: "Add
  full-file view", "Share review logic between interfaces".
- In the description, say what changed and why, and how you tested it.

Code written with an AI agent is fine, as long as you've reviewed it yourself.
verdict is a good tool for that.

## Releasing

For maintainers: tag a commit on `main` and push the tag:

```sh
git tag v0.2.0
git push origin v0.2.0
```

[`.github/workflows/release.yml`](.github/workflows/release.yml) builds
`verdict` for Linux (x86_64, aarch64) and macOS (Apple Silicon, Intel). It
publishes a GitHub release with a `.tar.gz` and a SHA-256 checksum for each,
plus generated release notes. The version comes from the tag: the workflow
writes it into `Cargo.toml` for the build, so there's no version bump to commit.

## License

verdict is licensed under the GNU GPL, version 3 or later. By submitting a
contribution, you agree that it's licensed under the same terms.

# Development tools

## Scripted screenshots

The window can drive itself and save a screenshot. That gives you a picture of a
UI change without clicking through it by hand. It also gives a coding agent
working on verdict a way to see the window it's changing, since an agent can't
look at a screen.

```sh
VERDICT_SCREENSHOT=/tmp/shot.png \
VERDICT_KEYS='A r "Remove the debug println" Escape j' \
  cargo run -- --gui path/to/repo
```

### What happens

1. The window opens on the repo's changes as usual.
2. verdict waits until every file is syntax highlighted, so the picture doesn't
   show half-colored code. If highlighting hasn't finished after about 600 frames,
   it goes on anyway.
3. It presses the keys in `VERDICT_KEYS`, one per frame. They go through the same
   keyboard handling as real key presses, so a script behaves exactly like you
   typing them.
4. It waits a few more frames for focus and scrolling to settle, saves the window
   as a PNG and prints `verdict: screenshot saved to ...` on stderr.
5. It closes **without submitting**. Nothing is staged, reverted or printed on
   stdout, so you can safely point it at a repo you care about.

### `VERDICT_KEYS`

A space-separated list of steps:

| Step | Does |
|---|---|
| `a`, `j`, `]`, ... | presses that key, as listed in the README's key table |
| `A`, `R`, `C` | presses Shift+A, Shift+R, Shift+C (one uppercase letter means Shift) |
| `Space`, `Escape`, `Enter`, `Tab`, `Down`, `Up`, `Home`, `End` | named keys |
| `"some text"` | types the text into the focused text box |

Text goes wherever the focus is. Rejecting (`r`) or commenting (`c`) focuses the
note box, so put the reason right after it, then `Escape` to leave the box before
the next shortcut. For example `r "Use tracing instead" Escape j` rejects the
current hunk with a reason and moves to the next one.

Ctrl isn't available, so a script can't submit (Ctrl+Enter). An unknown key name
or an unclosed quote stops verdict before the window opens, with exit code 2.

Leave `VERDICT_KEYS` unset to capture the review as it first opens.

### Limits

- **Window only.** The terminal interface has no screenshot mode. Use a terminal
  recorder for it, like the one behind `docs/tui-demo.svg`.
- **It needs a display.** On Linux that's X11 or Wayland (WSLg works). On a
  headless machine there's no window to capture.
- **The size is the window's.** The window opens at 1280×860 logical pixels.
  With display scaling, the PNG is larger by the scale factor.

### Regenerating `docs/screenshot.png`

The README screenshot is a small two-file change in a repo named `ratelimit`.
These commands rebuild it from scratch:

```sh
D=$(mktemp -d)/ratelimit && mkdir -p "$D/src" && cd "$D" && git init -q
cat > Cargo.toml <<'TOML'
[package]
name = "ratelimit"
version = "0.1.0"
edition = "2021"

[dependencies]
TOML
cat > src/limiter.rs <<'RS'
use std::time::{Duration, Instant};

/// A token bucket.
pub struct Limiter {
    tokens: u32,
    capacity: u32,
    refill_every: Duration,
    last: Instant,
}

impl Limiter {
    pub fn new(capacity: u32, refill_every: Duration) -> Self {
        Limiter { tokens: capacity, capacity, refill_every, last: Instant::now() }
    }

    /// Try to take one token.
    pub fn try_acquire(&mut self) -> bool {
        self.refill();
        if self.tokens > 0 {
            self.tokens -= 1;
            true
        } else {
            false
        }
    }

    fn refill(&mut self) {
        let elapsed = self.last.elapsed();
        if elapsed >= self.refill_every {
            self.tokens = self.capacity;
            self.last = Instant::now();
        }
    }
}
RS
git add -A && git -c user.name=demo -c user.email=demo@example.com commit -qm init

# The change under review: a dependency, a debug println, a new refill rule.
sed -i 's/^\[dependencies\]/[dependencies]\ntracing = "0.1"/' Cargo.toml
sed -i 's/^        self.refill();/        println!("acquire: {} tokens left", self.tokens);\n        self.refill();/' src/limiter.rs
sed -i 's|^        if elapsed >= self.refill_every {|        // Refill proportionally instead of all at once.\n        let earned = (elapsed.as_millis() / self.refill_every.as_millis().max(1)) as u32;\n        if earned > 0 {|' src/limiter.rs
sed -i 's/^            self.tokens = self.capacity;/            self.tokens = (self.tokens + earned).min(self.capacity);/' src/limiter.rs

VERDICT_SCREENSHOT="$OLDPWD/docs/screenshot.png" \
VERDICT_KEYS='A r "Remove the debug println, use the tracing crate if you need logs here." Escape j' \
  verdict --gui .
```

Run it from the root of the verdict checkout (it writes to `$OLDPWD/docs`), with
the `verdict` you want to picture on your `PATH`, for example after
`cargo install --path .`.
