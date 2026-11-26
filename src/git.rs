//! Thin wrapper around the `git` CLI. We shell out instead of linking libgit2:
//! zero build deps, and git itself is always the source of truth.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub type Result<T> = std::result::Result<T, String>;

fn git(repo: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("-C").arg(repo);
    c
}

pub fn run(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = git(repo)
        .args(args)
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

fn run_stdin(repo: &Path, args: &[&str], input: &[u8]) -> Result<Vec<u8>> {
    let mut child = git(repo)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to run git: {e}"))?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input)
        .map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
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
}

impl Status {
    pub fn letter(self) -> &'static str {
        match self {
            Status::Modified => "M",
            Status::Deleted => "D",
            Status::Untracked => "A",
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

pub fn index_blob(repo: &Path, path: &str) -> Result<Vec<u8>> {
    run(repo, &["cat-file", "blob", &format!(":{path}")])
}

/// Mode of the path in the index ("100644", "100755", "120000", ...).
pub fn index_mode(repo: &Path, path: &str) -> Option<String> {
    let out = run(repo, &["ls-files", "-s", "-z", "--", path]).ok()?;
    let s = String::from_utf8_lossy(&out);
    s.split_whitespace().next().map(str::to_owned)
}

/// Write `content` as the index version of `path`, without touching the working tree.
pub fn stage(repo: &Path, path: &str, content: &[u8], mode: &str) -> Result<()> {
    let sha = run_stdin(repo, &["hash-object", "-w", "--stdin"], content)?;
    let sha = String::from_utf8_lossy(&sha).trim().to_owned();
    run(
        repo,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("{mode},{sha},{path}"),
        ],
    )?;
    Ok(())
}

pub fn unstage_remove(repo: &Path, path: &str) -> Result<()> {
    run(repo, &["update-index", "--force-remove", "--", path])?;
    Ok(())
}
