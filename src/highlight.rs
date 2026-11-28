// SPDX-License-Identifier: GPL-3.0-or-later
//! Syntax highlighting registry: builtin languages/themes plus user extensions.
//!
//! Layout of the config dir (`$XDG_CONFIG_HOME/verdict`, default `~/.config/verdict`):
//!
//!   config.toml       interface choice, theme, file associations
//!   grammars/         *.sublime-syntax, *.tmLanguage, *.tmLanguage.json
//!   themes/           *.tmTheme, VS Code *.json color themes
//!   extensions/       unpacked VS Code extensions (anything with a package.json
//!                     that contributes grammars/languages/themes)

use crate::tm;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use syntect::highlighting::{FontStyle, Theme, ThemeSet};
use syntect::parsing::{SyntaxDefinition, SyntaxReference, SyntaxSet};

pub const DEFAULT_THEME: &str = "base16-ocean.dark";

#[derive(Clone, Debug)]
pub struct Span {
    pub rgb: [u8; 3],
    pub italic: bool,
    pub underline: bool,
    pub text: String,
}

pub type Line = Vec<Span>;

pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".config"));
    base.join("verdict")
}

fn cache_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".cache"));
    base.join("verdict")
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

#[derive(Deserialize, Default)]
struct Config {
    #[serde(default)]
    highlight: HighlightCfg,
    /// glob -> language (syntax name, scope like `source.ts`, or an extension)
    #[serde(default)]
    languages: BTreeMap<String, String>,
}

#[derive(Deserialize, Default)]
struct HighlightCfg {
    theme: Option<String>,
}

pub const EXAMPLE_CONFIG: &str = r#"# verdict configuration

[ui]
# "gui" (native window), "tui" (terminal) or "auto": the window when a display is
# available, else the terminal. `--gui` / `--tui` and $VERDICT_UI override this.
mode = "auto"
# "unified" (one column, - and + lines interleaved) or "split" (old | new side by
# side). Toggle it while reviewing with `s`.
layout = "unified"
# From the Claude Code hook, the terminal interface can't use the agent's terminal.
# Inside tmux it opens in a popup. Elsewhere, give a command that opens a terminal
# emulator and runs the program appended to it:
# terminal = ["wezterm", "start", "--"]
# terminal = ["kitty"]
# terminal = ["gnome-terminal", "--wait", "--"]
# Inside tmux: "popup" (over the agent), "split" (a pane next to the agent, so you
# can read what it said while reviewing) or "window" (a new tmux window).
# tmux = "popup"

[highlight]
# Any builtin theme (`verdict themes` lists them) or the name of a theme you drop
# into themes/ or ship in an extension under extensions/.
theme = "base16-ocean.dark"

# File associations, like VS Code's "files.associations".
# Keys are globs: a pattern without "/" matches the file name, otherwise the
# repo-relative path ("**" crosses directories). Values are a language name
# (`verdict langs`), a scope such as "source.ts", or an extension such as "ts".
[languages]
# "*.mdx" = "Markdown"
# "Justfile" = "Makefile"
# "*.conf" = "INI"
# "docker/**/*.env" = "Bourne Again Shell (bash)"
"#;

enum Source {
    Sublime(PathBuf),
    Tm {
        path: PathBuf,
        name: Option<String>,
        extensions: Vec<String>,
        filenames: Vec<String>,
        first_line: Option<String>,
    },
}

struct ThemeSource {
    path: PathBuf,
    name: String,
}

#[derive(Serialize, Deserialize)]
struct Cached {
    ss: SyntaxSet,
    user_names: Vec<String>,
    filenames: Vec<(String, String)>,
    warnings: Vec<String>,
}

pub struct Registry {
    pub ss: SyntaxSet,
    pub theme: Theme,
    pub theme_name: String,
    /// (name, from user dir)
    pub themes: Vec<(String, bool)>,
    /// Syntax names that come from the user's config dir.
    pub user_names: Vec<String>,
    /// Exact file names (e.g. "Jenkinsfile") -> syntax name, from VS Code manifests.
    filenames: HashMap<String, String>,
    assoc: Vec<(String, String)>,
    pub warnings: Vec<String>,
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            if p.file_name()
                .is_some_and(|n| n == "node_modules" || n == ".git")
            {
                continue;
            }
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn discover(
    dir: &Path,
    warnings: &mut Vec<String>,
) -> (Vec<Source>, Vec<ThemeSource>, Vec<PathBuf>) {
    let mut sources = Vec::new();
    let mut themes = Vec::new();
    let mut fingerprint_files = Vec::new();

    let mut files = Vec::new();
    walk(&dir.join("grammars"), &mut files);
    for p in files {
        let n = name_of(&p);
        if n.ends_with(".sublime-syntax") {
            sources.push(Source::Sublime(p.clone()));
        } else if n.ends_with(".tmLanguage")
            || n.ends_with(".tmLanguage.json")
            || n.ends_with(".json")
        {
            sources.push(Source::Tm {
                path: p.clone(),
                name: None,
                extensions: vec![],
                filenames: vec![],
                first_line: None,
            });
        } else {
            continue;
        }
        fingerprint_files.push(p);
    }

    let mut files = Vec::new();
    walk(&dir.join("themes"), &mut files);
    for p in files {
        let n = name_of(&p);
        let stem = n
            .trim_end_matches(".tmTheme")
            .trim_end_matches(".json")
            .trim_end_matches("-color-theme")
            .to_owned();
        if n.ends_with(".tmTheme") || n.ends_with(".json") {
            themes.push(ThemeSource {
                path: p,
                name: stem,
            });
        }
    }

    // VS Code extensions: read package.json "contributes".
    let mut files = Vec::new();
    walk(&dir.join("extensions"), &mut files);
    for manifest in files.into_iter().filter(|p| name_of(p) == "package.json") {
        let Some(base) = manifest.parent().map(Path::to_path_buf) else {
            continue;
        };
        let pkg = match tm::read_doc(&manifest) {
            Ok(v) => v,
            Err(e) => {
                warnings.push(format!("{}: {e}", manifest.display()));
                continue;
            }
        };
        let contrib = &pkg["contributes"];
        if !contrib.is_object() {
            continue;
        }
        fingerprint_files.push(manifest.clone());
        let mut langs: HashMap<String, ExtLanguage> = HashMap::new();
        for l in contrib["languages"].as_array().into_iter().flatten() {
            let Some(id) = l["id"].as_str() else { continue };
            let strs = |k: &str| -> Vec<String> {
                l[k].as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect()
            };
            let e = langs.entry(id.to_owned()).or_default();
            e.extensions.extend(
                strs("extensions")
                    .into_iter()
                    .map(|x| x.trim_start_matches('.').to_owned()),
            );
            e.filenames.extend(strs("filenames"));
            if e.first_line.is_none() {
                e.first_line = l["firstLine"].as_str().map(str::to_owned);
            }
            if e.alias.is_none() {
                e.alias = l["aliases"][0].as_str().map(str::to_owned);
            }
        }
        for g in contrib["grammars"].as_array().into_iter().flatten() {
            // Injection grammars (no language of their own) aren't supported.
            let (Some(lang), Some(path)) = (g["language"].as_str(), g["path"].as_str()) else {
                continue;
            };
            let info = langs.get(lang).cloned().unwrap_or_default();
            let p = base.join(path);
            fingerprint_files.push(p.clone());
            sources.push(Source::Tm {
                path: p,
                name: Some(info.alias.unwrap_or_else(|| lang.to_owned())),
                extensions: info.extensions,
                filenames: info.filenames,
                first_line: info.first_line,
            });
        }
        for t in contrib["themes"].as_array().into_iter().flatten() {
            let (Some(label), Some(path)) =
                (t["label"].as_str().or(t["id"].as_str()), t["path"].as_str())
            else {
                continue;
            };
            themes.push(ThemeSource {
                path: base.join(path),
                name: label.to_owned(),
            });
        }
    }
    (sources, themes, fingerprint_files)
}

/// A language an extension declares in `contributes.languages`.
#[derive(Clone, Default)]
struct ExtLanguage {
    extensions: Vec<String>,
    filenames: Vec<String>,
    first_line: Option<String>,
    alias: Option<String>,
}

fn fingerprint(files: &[PathBuf]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    env!("CARGO_PKG_VERSION").hash(&mut h);
    for f in files {
        f.hash(&mut h);
        if let Ok(m) = std::fs::metadata(f) {
            m.len().hash(&mut h);
            if let Ok(t) = m.modified() {
                t.hash(&mut h);
            }
        }
    }
    h.finish()
}

fn build_user_syntaxes(sources: Vec<Source>, warnings: &mut Vec<String>) -> Cached {
    let base = two_face::syntax::extra_newlines();
    // Scopes we can resolve `include: source.x` against. true = converted from TextMate.
    let mut known: HashMap<String, bool> = base
        .syntaxes()
        .iter()
        .map(|s| (s.scope.build_string(), false))
        .collect();

    // Parse everything first so cross-grammar includes between user grammars resolve.
    let mut defs: Vec<(String, Result<SyntaxDefinition, String>)> = Vec::new();
    let mut tms = Vec::new();
    let mut filenames = Vec::new();
    for src in sources {
        match src {
            Source::Sublime(p) => {
                let r = std::fs::read_to_string(&p)
                    .map_err(|e| e.to_string())
                    .and_then(|t| {
                        let stem = name_of(&p).trim_end_matches(".sublime-syntax").to_owned();
                        SyntaxDefinition::load_from_str(&t, true, Some(&stem))
                            .map_err(|e| e.to_string())
                    });
                if let Ok(d) = &r {
                    known.insert(d.scope.build_string(), false);
                }
                defs.push((p.display().to_string(), r));
            }
            Source::Tm {
                path,
                name,
                extensions,
                filenames: fnames,
                first_line,
            } => {
                let g =
                    tm::read_doc(&path).and_then(|v| tm::TmGrammar::from_value(v, name.as_deref()));
                match g {
                    Ok(mut g) => {
                        for e in extensions {
                            if !g.extensions.contains(&e) {
                                g.extensions.push(e);
                            }
                        }
                        if first_line.is_some() {
                            g.first_line = first_line;
                        }
                        for f in fnames {
                            filenames.push((f, g.name.clone()));
                        }
                        known.insert(g.scope.clone(), true);
                        tms.push((path, g));
                    }
                    Err(e) => warnings.push(format!("{}: {e}", path.display())),
                }
            }
        }
    }
    // A grammar listed twice (standalone + extension) loads once; the last wins.
    let mut by_scope: BTreeMap<String, (PathBuf, tm::TmGrammar)> = BTreeMap::new();
    for (p, g) in tms {
        by_scope.insert(g.scope.clone(), (p, g));
    }
    for (_, (path, g)) in by_scope {
        let (text, dropped) = tm::convert(&g, &known);
        if dropped > 0 {
            warnings.push(format!(
                "{}: {dropped} rule(s) skipped (unsupported regex or include)",
                path.display()
            ));
        }
        let r =
            SyntaxDefinition::load_from_str(&text, true, Some(&g.name)).map_err(|e| e.to_string());
        defs.push((path.display().to_string(), r));
    }

    let mut builder = base.into_builder();
    let mut user_names = Vec::new();
    for (path, r) in defs {
        match r {
            Ok(d) => {
                user_names.push(d.name.clone());
                builder.add(d);
            }
            Err(e) => warnings.push(format!("{path}: {e}")),
        }
    }
    Cached {
        ss: builder.build(),
        user_names,
        filenames,
        warnings: warnings.clone(),
    }
}

fn load_theme_set(themes: &[ThemeSource], warnings: &mut Vec<String>) -> (ThemeSet, Vec<String>) {
    let mut set: ThemeSet = two_face::theme::extra().into();
    let mut user = Vec::new();
    for t in themes {
        let n = name_of(&t.path);
        let theme = if n.ends_with(".tmTheme") {
            ThemeSet::get_theme(&t.path).map_err(|e| e.to_string())
        } else {
            tm::load_vscode_theme(&t.path, &t.name)
        };
        match theme {
            Ok(th) => {
                set.themes.insert(t.name.clone(), th);
                user.push(t.name.clone());
            }
            Err(e) => warnings.push(format!("theme {}: {e}", t.path.display())),
        }
    }
    (set, user)
}

impl Registry {
    pub fn load() -> Registry {
        let dir = config_dir();
        let mut warnings = Vec::new();
        let cfg: Config = match std::fs::read_to_string(dir.join("config.toml")) {
            Ok(t) => toml::from_str(&t).unwrap_or_else(|e| {
                warnings.push(format!("config.toml: {e}"));
                Config::default()
            }),
            Err(_) => Config::default(),
        };
        let (sources, theme_sources, fp_files) = discover(&dir, &mut warnings);

        let cached = if sources.is_empty() {
            Cached {
                ss: two_face::syntax::extra_newlines(),
                user_names: vec![],
                filenames: vec![],
                warnings: vec![],
            }
        } else {
            let key = fingerprint(&fp_files);
            let cdir = cache_dir();
            let cfile = cdir.join(format!("syntaxes-{key:016x}.bin"));
            match syntect::dumps::from_dump_file::<Cached, _>(&cfile) {
                Ok(c) => c,
                Err(_) => {
                    let mut w = Vec::new();
                    let c = build_user_syntaxes(sources, &mut w);
                    if std::fs::create_dir_all(&cdir).is_ok() {
                        // drop stale caches
                        for old in std::fs::read_dir(&cdir).into_iter().flatten().flatten() {
                            let n = name_of(&old.path());
                            if n.starts_with("syntaxes-") && n.ends_with(".bin") {
                                let _ = std::fs::remove_file(old.path());
                            }
                        }
                        let _ = syntect::dumps::dump_to_file(&c, &cfile);
                    }
                    c
                }
            }
        };
        warnings.extend(cached.warnings.iter().cloned());

        let (mut set, user_themes) = load_theme_set(&theme_sources, &mut warnings);
        let wanted = cfg
            .highlight
            .theme
            .clone()
            .unwrap_or_else(|| DEFAULT_THEME.to_owned());
        let theme_name = if set.themes.contains_key(&wanted) {
            wanted
        } else {
            warnings.push(format!(
                "theme \"{wanted}\" not found, using {DEFAULT_THEME}"
            ));
            DEFAULT_THEME.to_owned()
        };
        let mut themes: Vec<(String, bool)> = set
            .themes
            .keys()
            .map(|k| (k.clone(), user_themes.contains(k)))
            .collect();
        themes.sort_by_key(|t| t.0.to_lowercase());
        let theme = set
            .themes
            .remove(&theme_name)
            .expect("theme_name is picked from set.themes above");

        Registry {
            ss: cached.ss,
            theme,
            theme_name,
            themes,
            user_names: cached.user_names,
            filenames: cached.filenames.into_iter().collect(),
            assoc: cfg.languages.into_iter().collect(),
            warnings,
        }
    }

    fn resolve(&self, target: &str) -> Option<&SyntaxReference> {
        let ss = &self.ss;
        ss.find_syntax_by_name(target)
            .or_else(|| {
                ss.syntaxes()
                    .iter()
                    .rev()
                    .find(|s| s.scope.build_string() == target)
            })
            .or_else(|| {
                ss.syntaxes()
                    .iter()
                    .rev()
                    .find(|s| s.name.eq_ignore_ascii_case(target))
            })
            .or_else(|| ss.find_syntax_by_extension(target.trim_start_matches('.')))
    }

    /// Pick the language for a repo-relative path, VS Code style:
    /// user associations > exact file names > extension > first line.
    pub fn syntax_for(&self, path: &str, first_line: Option<&str>) -> Option<&SyntaxReference> {
        let fname = path.rsplit('/').next().unwrap_or(path);
        for (glob, target) in &self.assoc {
            let subject = if glob.contains('/') { path } else { fname };
            if glob_match(glob.as_bytes(), subject.as_bytes()) {
                if let Some(s) = self.resolve(target) {
                    return Some(s);
                }
            }
        }
        if let Some(s) = self
            .filenames
            .get(fname)
            .and_then(|n| self.ss.find_syntax_by_name(n))
        {
            return Some(s);
        }
        if let Some(s) = self.ss.find_syntax_by_extension(fname) {
            return Some(s);
        }
        // Longest compound extension first: "d.ts" before "ts".
        let mut rest = fname;
        while let Some((_, ext)) = rest.split_once('.') {
            if let Some(s) = self.ss.find_syntax_by_extension(ext) {
                return Some(s);
            }
            rest = ext;
        }
        first_line.and_then(|l| self.ss.find_syntax_by_first_line(l))
    }

    /// Highlight `lines` (each ending in '\n') from the top, stopping after `upto` lines.
    pub fn highlight(&self, syn: &SyntaxReference, lines: &[String], upto: usize) -> Vec<Line> {
        let mut hl = syntect::easy::HighlightLines::new(syn, &self.theme);
        let mut out = Vec::with_capacity(upto.min(lines.len()));
        for line in lines.iter().take(upto) {
            // Minified/generated files: give up rather than stall.
            if line.len() > 5000 {
                break;
            }
            let spans = match hl.highlight_line(line, &self.ss) {
                Ok(v) => v,
                Err(_) => break,
            };
            out.push(
                spans
                    .into_iter()
                    .map(|(st, t)| Span {
                        rgb: [st.foreground.r, st.foreground.g, st.foreground.b],
                        italic: st.font_style.contains(FontStyle::ITALIC),
                        underline: st.font_style.contains(FontStyle::UNDERLINE),
                        text: t.trim_end_matches(['\n', '\r']).to_owned(),
                    })
                    .collect(),
            );
        }
        out
    }
}

/// Glob with `*` (no '/'), `**` (anything), `?`.
pub fn glob_match(p: &[u8], s: &[u8]) -> bool {
    match (p.first(), s.first()) {
        (None, None) => true,
        (Some(b'*'), _) if p.get(1) == Some(&b'*') => {
            let rest = if p.get(2) == Some(&b'/') {
                &p[3..]
            } else {
                &p[2..]
            };
            (0..=s.len()).any(|i| glob_match(rest, &s[i..]))
        }
        (Some(b'*'), _) => {
            for i in 0..=s.len() {
                if glob_match(&p[1..], &s[i..]) {
                    return true;
                }
                if i < s.len() && s[i] == b'/' {
                    break;
                }
            }
            false
        }
        (Some(b'?'), Some(c)) if *c != b'/' => glob_match(&p[1..], &s[1..]),
        (Some(a), Some(b)) if a == b => glob_match(&p[1..], &s[1..]),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::glob_match;

    #[test]
    fn globs() {
        let g = |p: &str, s: &str| glob_match(p.as_bytes(), s.as_bytes());
        assert!(g("*.mdx", "a.mdx"));
        assert!(!g("*.mdx", "dir/a.mdx"));
        assert!(g("Justfile", "Justfile"));
        assert!(g("docker/**/*.env", "docker/a/b/x.env"));
        assert!(g("docker/**/*.env", "docker/x.env"));
        assert!(g("src/?.rs", "src/a.rs"));
        assert!(!g("src/*.rs", "src/a/b.rs"));
    }
}
