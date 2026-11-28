// SPDX-License-Identifier: GPL-3.0-or-later
//! Thin wrapper around the `git` CLI. We shell out instead of linking libgit2:
//! zero build deps, and git itself is always the source of truth.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    /// git couldn't be started: not installed, or not on PATH.
    Spawn(std::io::Error),
    /// git ran and failed.
    Git { args: String, stderr: String },
    /// Reading or writing a file failed.
    Io { path: String, err: std::io::Error },
    /// Something git said that we can't use, or input that isn't valid, such as a
    /// range that isn't one.
    Invalid(String),
    /// Submitting stopped partway: the index was updated and the working tree was
    /// changed for `done` but not for `left`.
    Partial {
        err: Box<Error>,
        done: Vec<String>,
        left: Vec<String>,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Spawn(e) => write!(f, "failed to run git: {e}"),
            Error::Git { args, stderr } => write!(f, "git {args}: {stderr}"),
            Error::Io { path, err } => write!(f, "{path}: {err}"),
            Error::Invalid(m) => f.write_str(m),
            Error::Partial { err, done, left } => {
                write!(f, "{err}\nThe index was updated. ")?;
                if !done.is_empty() {
                    write!(f, "The working tree was changed for {}; ", done.join(", "))?;
                }
                write!(f, "not yet for {}.", left.join(", "))
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<Error> for String {
    fn from(e: Error) -> String {
        e.to_string()
    }
}

impl Error {
    /// A failed file operation on `path`.
    pub fn io(path: &str) -> impl FnOnce(std::io::Error) -> Error + '_ {
        move |err| Error::Io {
            path: path.to_owned(),
            err,
        }
    }
}

fn git(repo: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("-C").arg(repo);
    c
}

fn check(args: &[&str], out: std::process::Output) -> Result<Vec<u8>> {
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(Error::Git {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        })
    }
}

pub fn run(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = git(repo).args(args).output().map_err(Error::Spawn)?;
    check(args, out)
}

/// Run git with `input` on its stdin. The input is written on its own thread while
/// the output is read: git may fill the stdout pipe before it has read all of its
/// input, and writing everything first would then wait on it forever.
pub fn run_stdin(repo: &Path, args: &[&str], input: &[u8]) -> Result<Vec<u8>> {
    let mut child = git(repo)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(Error::Spawn)?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let (wrote, out) = std::thread::scope(|s| {
        // Dropping `stdin` when the write is done closes it, so git sees the end.
        let writer = s.spawn(move || stdin.write_all(input));
        let out = child.wait_with_output();
        (writer.join(), out)
    });
    let out = check(args, out.map_err(Error::Spawn)?)?;
    // A write error with a successful exit means git stopped reading early, which
    // it doesn't do for the commands we run: something is wrong.
    match wrote {
        Ok(Ok(())) => Ok(out),
        Ok(Err(e)) => Err(Error::Invalid(format!("git {}: {e}", args.join(" ")))),
        Err(_) => Err(Error::Invalid("writing to git panicked".into())),
    }
}

/// A file mode as git records it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// 100644
    Regular,
    /// 100755
    Executable,
    /// 120000
    Symlink,
    /// 160000: a submodule.
    Gitlink,
}

impl Mode {
    /// Parse git's octal mode. Unusual regular-file modes that old gits wrote, such
    /// as 100664, count as regular files.
    pub fn parse(s: &str) -> Option<Mode> {
        Some(match s {
            "100755" => Mode::Executable,
            "120000" => Mode::Symlink,
            "160000" => Mode::Gitlink,
            _ if s.starts_with("100") => Mode::Regular,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Regular => "100644",
            Mode::Executable => "100755",
            Mode::Symlink => "120000",
            Mode::Gitlink => "160000",
        }
    }

    /// A file verdict can review: not a symlink or a submodule.
    pub fn is_file(self) -> bool {
        matches!(self, Mode::Regular | Mode::Executable)
    }
}

/// An entry of a tree or of the index.
pub struct Item {
    pub mode: Mode,
    /// The blob's object id.
    pub oid: String,
}

/// The entries for `paths` in `rev` ("" is the index), in a few git calls however
/// many paths there are. Paths that aren't there are left out.
pub fn items(repo: &Path, rev: &str, paths: &[&str]) -> Result<HashMap<String, Item>> {
    let mut found = HashMap::new();
    // Pathspecs go on the command line, so keep each call well under its limit.
    for chunk in paths.chunks(500) {
        let mut args = vec!["--literal-pathspecs"];
        if rev.is_empty() {
            args.extend(["ls-files", "--stage", "-z", "--"]);
        } else {
            args.extend(["ls-tree", "-r", "-z", "--full-tree", rev, "--"]);
        }
        args.extend_from_slice(chunk);
        let out = run(repo, &args)?;
        for rec in out.split(|b| *b == 0).filter(|r| !r.is_empty()) {
            // ls-files: "<mode> <oid> <stage>\t<path>"; ls-tree: "<mode> <type> <oid>\t<path>".
            let rec = String::from_utf8_lossy(rec);
            let Some((meta, path)) = rec.split_once('\t') else {
                continue;
            };
            let f: Vec<&str> = meta.split(' ').collect();
            let (mode, oid) = match (rev.is_empty(), f.as_slice()) {
                (true, [m, o, _]) => (m, o),
                (false, [m, _, o]) => (m, o),
                _ => continue,
            };
            let Some(mode) = Mode::parse(mode) else {
                continue;
            };
            // With a conflict the index has several stages; the first will do.
            found.entry(path.to_owned()).or_insert(Item {
                mode,
                oid: (*oid).to_owned(),
            });
        }
    }
    Ok(found)
}

/// The contents of each blob in `oids`, in order, in one git call.
pub fn blobs(repo: &Path, oids: &[&str]) -> Result<Vec<Vec<u8>>> {
    if oids.is_empty() {
        return Ok(Vec::new());
    }
    let input = oids.join("\n") + "\n";
    let out = run_stdin(repo, &["cat-file", "--batch"], input.as_bytes())?;
    let bad = || Error::Invalid("git cat-file --batch: unexpected output".into());
    let mut rest = out.as_slice();
    let mut blobs = Vec::with_capacity(oids.len());
    for oid in oids {
        // "<oid> <type> <size>\n<contents>\n", or "<oid> missing\n".
        let nl = rest.iter().position(|b| *b == b'\n').ok_or_else(bad)?;
        let header = String::from_utf8_lossy(&rest[..nl]).into_owned();
        rest = &rest[nl + 1..];
        let size: usize = match header.rsplit(' ').next().map(str::parse) {
            Some(Ok(n)) if header.split(' ').count() == 3 => n,
            _ => return Err(Error::Invalid(format!("object {oid}: {header}"))),
        };
        if rest.len() < size + 1 {
            return Err(bad());
        }
        blobs.push(rest[..size].to_vec());
        rest = &rest[size + 1..];
    }
    Ok(blobs)
}

pub fn toplevel(dir: &Path) -> Option<PathBuf> {
    let out = run(dir, &["rev-parse", "--show-toplevel"]).ok()?;
    let s = String::from_utf8(out).ok()?;
    Some(PathBuf::from(s.trim_end_matches(['\n', '\r'])))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Modified,
    Deleted,
    Untracked,
    /// Deleted at one path and added at another with similar content. verdict pairs
    /// these itself (see `pair_renames` in `diff/load.rs`); git never reports it to us.
    Renamed,
}

impl Status {
    pub fn letter(self) -> &'static str {
        match self {
            Status::Modified => "M",
            Status::Deleted => "D",
            Status::Untracked => "A",
            Status::Renamed => "R",
        }
    }
}

pub struct Entry {
    pub path: String,
    pub status: Status,
}

/// Files whose working-tree content differs from the index (plus untracked files).
pub fn changed(repo: &Path) -> Result<Vec<Entry>> {
    let out = run(
        repo,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    let mut fields = out.split(|b| *b == 0).filter(|f| !f.is_empty());
    let mut entries = Vec::new();
    while let Some(f) = fields.next() {
        if f.len() < 4 {
            continue;
        }
        let (x, y) = (f[0], f[1]);
        let path = String::from_utf8_lossy(&f[3..]).into_owned();
        if x == b'R' || x == b'C' {
            fields.next(); // rename source path
        }
        let status = match (x, y) {
            (b'?', b'?') => Status::Untracked,
            (_, b'M') | (_, b'T') => Status::Modified,
            (_, b'D') => Status::Deleted,
            _ => continue,
        };
        entries.push(Entry { path, status });
    }
    Ok(entries)
}

/// What a review compares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// Index -> working tree, plus untracked files. Decisions are applied to git.
    Worktree,
    /// HEAD -> index. Feedback only.
    Staged,
    /// Commit `old` -> commit `new` (resolved ids). Feedback only. `label` is what
    /// the user typed, for display.
    Range {
        old: String,
        new: String,
        label: String,
    },
}

impl Source {
    /// Decisions only produce feedback; nothing in git changes.
    pub fn read_only(&self) -> bool {
        !matches!(self, Source::Worktree)
    }

    /// Short description for headers and the feedback message.
    pub fn describe(&self) -> String {
        match self {
            Source::Worktree => "unstaged changes".into(),
            Source::Staged => "staged changes".into(),
            Source::Range { label, .. } => label.clone(),
        }
    }

    /// The old and new side as revisions for `blob` / `mode_at`: "" means the index.
    pub fn revs(&self) -> (&str, &str) {
        match self {
            Source::Worktree => ("", ""),
            Source::Staged => ("HEAD", ""),
            Source::Range { old, new, .. } => (old, new),
        }
    }
}

/// Parse a range: `A..B`, `A..` (to HEAD), `..B` (from HEAD) or `A...B` (from the
/// merge base of A and B, like `git diff A...B`).
pub fn parse_range(repo: &Path, spec: &str) -> Result<Source> {
    let commit = |r: &str| -> Result<String> {
        let r = if r.is_empty() { "HEAD" } else { r };
        let out = run(
            repo,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{r}^{{commit}}"),
            ],
        )
        .map_err(|_| Error::Invalid(format!("{r}: not a commit")))?;
        Ok(String::from_utf8_lossy(&out).trim().to_owned())
    };
    let (old, new) = if let Some((a, b)) = spec.split_once("...") {
        let (a, b) = (commit(a)?, commit(b)?);
        let base = run(repo, &["merge-base", &a, &b])
            .map_err(|e| Error::Invalid(format!("{spec}: {e}")))?;
        (String::from_utf8_lossy(&base).trim().to_owned(), b)
    } else if let Some((a, b)) = spec.split_once("..") {
        (commit(a)?, commit(b)?)
    } else {
        return Err(Error::Invalid(format!("{spec}: not a range (use A..B)")));
    };
    Ok(Source::Range {
        old,
        new,
        label: spec.to_owned(),
    })
}

/// Files that differ between the two sides of `source`.
pub fn changed_in(repo: &Path, source: &Source) -> Result<Vec<Entry>> {
    let mut args = vec![
        "diff",
        "--name-status",
        "-z",
        // Renames are paired by verdict, the same way for every source.
        "--no-renames",
        "--no-ext-diff",
    ];
    match source {
        Source::Worktree => return changed(repo),
        Source::Staged => args.push("--cached"),
        Source::Range { old, new, .. } => {
            args.push(old);
            args.push(new);
        }
    }
    let out = run(repo, &args)?;
    let mut fields = out.split(|b| *b == 0).filter(|f| !f.is_empty());
    let mut entries = Vec::new();
    while let (Some(st), Some(path)) = (fields.next(), fields.next()) {
        let status = match st.first() {
            Some(b'A') => Status::Untracked,
            Some(b'D') => Status::Deleted,
            Some(b'M' | b'T') => Status::Modified,
            _ => continue,
        };
        entries.push(Entry {
            path: String::from_utf8_lossy(path).into_owned(),
            status,
        });
    }
    Ok(entries)
}

/// The repository's git directory (`.git`, or the worktree's own git dir).
pub fn git_dir(repo: &Path) -> Option<PathBuf> {
    let out = run(repo, &["rev-parse", "--absolute-git-dir"]).ok()?;
    Some(PathBuf::from(String::from_utf8(out).ok()?.trim()))
}

#[cfg(test)]
pub fn index_blob(repo: &Path, path: &str) -> Result<Vec<u8>> {
    run(repo, &["cat-file", "blob", &format!(":{path}")])
}

/// Mode of the path in the index.
pub fn index_mode(repo: &Path, path: &str) -> Option<Mode> {
    items(repo, "", &[path]).ok()?.remove(path).map(|i| i.mode)
}

/// Write `content` to the object database; return its object id. Nothing refers to
/// the object yet, so this changes nothing anyone can see.
pub fn hash_object(repo: &Path, content: &[u8]) -> Result<String> {
    let oid = run_stdin(repo, &["hash-object", "-w", "--stdin"], content)?;
    Ok(String::from_utf8_lossy(&oid).trim().to_owned())
}

/// A change to one index entry.
pub enum IndexOp {
    /// Set `path` to the blob `oid`.
    Set {
        path: String,
        mode: Mode,
        oid: String,
    },
    /// Take `path` out of the index.
    Remove { path: String },
}

/// Make every change in `ops` to the index in one git call: git writes the new index
/// only if it can make all of them.
pub fn update_index(repo: &Path, ops: &[IndexOp]) -> Result<()> {
    if ops.is_empty() {
        return Ok(());
    }
    // Removing an entry is mode 0 with an all-zero id as long as the repository's.
    let zero = if ops.iter().any(|o| matches!(o, IndexOp::Remove { .. })) {
        let fmt = run(repo, &["rev-parse", "--show-object-format"])?;
        let len = if String::from_utf8_lossy(&fmt).trim() == "sha256" {
            64
        } else {
            40
        };
        "0".repeat(len)
    } else {
        String::new()
    };
    let mut input = String::new();
    for op in ops {
        let (mode, oid, path) = match op {
            IndexOp::Set { path, mode, oid } => (mode.as_str(), oid.as_str(), path),
            IndexOp::Remove { path } => ("0", zero.as_str(), path),
        };
        input.push_str(&format!("{mode} {oid}\t{path}\0"));
    }
    run_stdin(
        repo,
        &["update-index", "-z", "--index-info"],
        input.as_bytes(),
    )?;
    Ok(())
}
