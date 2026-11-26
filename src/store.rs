// SPDX-License-Identifier: GPL-3.0-or-later
//! Where verdict keeps its state: `.git/verdict` in the repository, written so a
//! reader never sees half a file.

use crate::git;
use std::path::{Path, PathBuf};

/// `.git/verdict` (or the worktree's own git dir's). `None` outside a repository.
pub fn dir(repo: &Path) -> Option<PathBuf> {
    Some(git::git_dir(repo)?.join("verdict"))
}

/// Write `contents` to `path` through a temporary file and a rename, so a crash or
/// a concurrent reader never sees it half written. Creates the parent directory.
pub fn write_atomic(path: &Path, contents: impl AsRef<[u8]>) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    #[test]
    fn writes_whole_files_and_creates_directories() {
        let dir = testutil::temp_dir("store");
        let p = dir.join("a/b/c.json");
        write_atomic(&p, "one").unwrap();
        write_atomic(&p, "two").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "two");
        // Nothing left behind but the file itself.
        assert_eq!(std::fs::read_dir(p.parent().unwrap()).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
