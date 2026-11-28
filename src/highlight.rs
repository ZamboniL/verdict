// SPDX-License-Identifier: GPL-3.0-or-later
//! Syntax highlighting with giallo, which runs VS Code's TextMate grammars and
//! themes as they are, plus the user's own.
//!
//! Layout of the config dir (`$XDG_CONFIG_HOME/verdict`, default `~/.config/verdict`):
//!
//!   config.toml       interface choice, theme, file associations
//!   grammars/         *.tmLanguage, *.tmLanguage.json
//!   themes/           VS Code *.json color themes, *.tmTheme
//!   extensions/       unpacked VS Code extensions (anything with a package.json
//!                     that contributes grammars/languages/themes)

use crate::{langs, tm};
use fancy_regex::Regex;
use giallo::{FontStyle, HighlightOptions, ThemeVariant};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// VS Code's default dark theme.
pub const DEFAULT_THEME: &str = "dark-plus";

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

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

#[derive(Deserialize, Default)]
struct Config {
    #[serde(default)]
    highlight: HighlightCfg,
    /// glob -> language (a name or alias from `verdict langs`, or an extension)
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
theme = "dark-plus"

# File associations, like VS Code's "files.associations".
# Keys are globs: a pattern without "/" matches the file name, otherwise the
# repo-relative path ("**" crosses directories). Values are a language name or
# alias (`verdict langs`) or an extension such as "ts".
[languages]
# "*.mdx" = "markdown"
# "Justfile" = "make"
# "*.conf" = "ini"
# "docker/**/*.env" = "shellscript"
"#;

/// A TextMate grammar the user added, with what an extension says about it.
struct Source {
    path: PathBuf,
    name: Option<String>,
    extensions: Vec<String>,
    filenames: Vec<String>,
    first_line: Option<String>,
}

struct ThemeSource {
    path: PathBuf,
    name: String,
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

fn discover(dir: &Path, warnings: &mut Vec<String>) -> (Vec<Source>, Vec<ThemeSource>) {
    let mut sources = Vec::new();
    let mut themes = Vec::new();

    let mut files = Vec::new();
    walk(&dir.join("grammars"), &mut files);
    for p in files {
        let n = name_of(&p);
        if n.ends_with(".sublime-syntax") {
            warnings.push(format!(
                "{}: .sublime-syntax grammars aren't supported, use a TextMate grammar",
                p.display()
            ));
            continue;
        } else if n.ends_with(".tmLanguage")
            || n.ends_with(".tmLanguage.json")
            || n.ends_with(".json")
        {
            sources.push(Source {
                path: p,
                name: None,
                extensions: vec![],
                filenames: vec![],
                first_line: None,
            });
        }
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
            sources.push(Source {
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
    (sources, themes)
}

/// A language an extension declares in `contributes.languages`.
#[derive(Clone, Default)]
struct ExtLanguage {
    extensions: Vec<String>,
    filenames: Vec<String>,
    first_line: Option<String>,
    alias: Option<String>,
}

pub struct Registry {
    hl: giallo::Registry,
    pub theme_name: String,
    /// (name, from user dir)
    pub themes: Vec<(String, bool)>,
    /// Languages that come from the user's config dir.
    pub user_names: Vec<String>,
    /// Name or alias (lowercase) -> the grammar's display name, so "rs" shows as "rust".
    canonical: HashMap<String, String>,
    /// From user grammars and extensions: extension / file name / scope -> grammar.
    user_exts: HashMap<String, String>,
    user_filenames: HashMap<String, String>,
    user_scopes: HashMap<String, String>,
    user_first_lines: Vec<(Regex, String)>,
    assoc: Vec<(String, String)>,
    pub warnings: Vec<String>,
}

impl Registry {
    pub fn load() -> Registry {
        Registry::load_from(&config_dir())
    }

    /// Load the builtin set plus what the config dir `dir` adds.
    fn load_from(dir: &Path) -> Registry {
        let mut warnings = Vec::new();
        let cfg: Config = match std::fs::read_to_string(dir.join("config.toml")) {
            Ok(t) => toml::from_str(&t).unwrap_or_else(|e| {
                warnings.push(format!("config.toml: {e}"));
                Config::default()
            }),
            Err(_) => Config::default(),
        };
        let (sources, theme_sources) = discover(dir, &mut warnings);

        let hl = giallo::Registry::builtin().unwrap_or_else(|e| {
            warnings.push(format!("builtin grammars failed to load: {e}"));
            giallo::Registry::default()
        });
        let mut r = Registry {
            hl,
            theme_name: String::new(),
            themes: Vec::new(),
            user_names: Vec::new(),
            canonical: HashMap::new(),
            user_exts: HashMap::new(),
            user_filenames: HashMap::new(),
            user_scopes: HashMap::new(),
            user_first_lines: Vec::new(),
            assoc: cfg.languages.into_iter().collect(),
            warnings: Vec::new(),
        };
        for src in sources {
            r.add_grammar(src, &mut warnings);
        }
        r.hl.link_grammars();

        let mut user_themes = Vec::new();
        for t in theme_sources {
            let added = tm::theme_json(&t.path, &t.name).and_then(|doc| {
                let bytes = serde_json::to_vec(&doc).map_err(|e| e.to_string())?;
                r.hl.add_theme_from_bytes(&bytes).map_err(|e| e.to_string())
            });
            match added {
                Ok(()) => user_themes.push(t.name),
                Err(e) => warnings.push(format!("{}: {e}", t.path.display())),
            }
        }

        for n in r.hl.get_grammar_names() {
            if n.name.starts_with(USER_PREFIX) {
                continue;
            }
            for key in std::iter::once(n.name).chain(n.aliases.iter().map(String::as_str)) {
                r.canonical
                    .entry(key.to_lowercase())
                    .or_insert_with(|| n.name.to_owned());
            }
        }

        r.theme_name = match cfg.highlight.theme {
            Some(t) if r.hl.contains_theme(&t) => t,
            Some(t) => {
                warnings.push(format!("theme \"{t}\" not found, using {DEFAULT_THEME}"));
                DEFAULT_THEME.to_owned()
            }
            None => DEFAULT_THEME.to_owned(),
        };
        r.themes = langs::BUILTIN_THEMES
            .iter()
            .map(|t| (t.to_string(), false))
            .chain(user_themes.into_iter().map(|t| (t, true)))
            .collect();
        r.themes.sort_by_key(|t| t.0.to_lowercase());

        for (glob, target) in &r.assoc {
            if r.resolve(target).is_none() {
                warnings.push(format!(
                    "[languages] \"{glob}\": no language \"{target}\" (see `verdict langs`)"
                ));
            }
        }
        r.warnings = warnings;
        r
    }

    /// Add a user grammar. A builtin can't be replaced once linked, so each gets its
    /// own name and takes over its label as an alias.
    fn add_grammar(&mut self, src: Source, warnings: &mut Vec<String>) {
        let path = src.path.display();
        let mut doc = match tm::read_doc(&src.path) {
            Ok(d) => d,
            Err(e) => return warnings.push(format!("{path}: {e}")),
        };
        let Some(label) = src
            .name
            .or_else(|| doc["name"].as_str().map(str::to_owned))
            .or_else(|| doc["scopeName"].as_str().map(str::to_owned))
        else {
            return warnings.push(format!("{path}: grammar has no name"));
        };
        let id = format!("{USER_PREFIX}{}", label.to_lowercase());
        doc["name"] = Value::String(id.clone());
        let bytes = serde_json::to_vec(&doc).unwrap_or_default();
        if let Err(e) = self.hl.add_grammar_from_bytes(&bytes) {
            return warnings.push(format!("{path}: {e}"));
        }
        self.hl.add_alias(&id, &label);

        let key = label.to_lowercase();
        let file_types = doc["fileTypes"].as_array().into_iter().flatten();
        let exts = src
            .extensions
            .into_iter()
            .chain(file_types.filter_map(|v| v.as_str().map(str::to_owned)));
        for e in exts {
            self.user_exts
                .insert(e.trim_start_matches('.').to_lowercase(), key.clone());
        }
        for f in src.filenames {
            self.user_filenames.insert(f, key.clone());
        }
        if let Some(scope) = doc["scopeName"].as_str() {
            self.user_scopes.insert(scope.to_owned(), key.clone());
        }
        let first_line = src
            .first_line
            .or_else(|| doc["firstLineMatch"].as_str().map(str::to_owned));
        if let Some(l) = first_line {
            match Regex::new(&l) {
                Ok(re) => self.user_first_lines.push((re, key.clone())),
                Err(e) => warnings.push(format!("{path}: first line pattern: {e}")),
            }
        }
        self.canonical.insert(key, label.clone());
        self.user_names.push(label);
    }

    /// Every language, with its aliases.
    pub fn grammars(&self) -> Vec<(String, Vec<String>)> {
        let mut v: Vec<_> = self
            .hl
            .get_grammar_names()
            .into_iter()
            .filter(|n| n.name != giallo::PLAIN_GRAMMAR_NAME)
            .map(|n| match n.name.strip_prefix(USER_PREFIX) {
                // Its first alias is the label, lowercased by giallo.
                Some(_) => {
                    let a = n.aliases.first().cloned().unwrap_or_default();
                    (self.canonical.get(&a).cloned().unwrap_or(a), vec![])
                }
                None => (n.name.to_owned(), n.aliases.to_vec()),
            })
            .collect();
        v.sort_by_key(|g| g.0.to_lowercase());
        v
    }

    /// A language name, alias, user grammar scope or extension -> its display name.
    fn resolve(&self, target: &str) -> Option<String> {
        if let Some(k) = self.user_scopes.get(target) {
            return self.canonical.get(k).cloned();
        }
        let t = target.trim_start_matches('.').to_lowercase();
        self.canonical.get(&t).cloned().or_else(|| self.by_ext(&t))
    }

    fn by_ext(&self, ext: &str) -> Option<String> {
        let ext = ext.to_lowercase();
        if let Some(k) = self.user_exts.get(&ext) {
            return self.canonical.get(k).cloned();
        }
        if let Some(c) = self.canonical.get(&ext) {
            return Some(c.clone());
        }
        let (_, lang) = langs::EXTENSIONS.iter().find(|(e, _)| *e == ext)?;
        self.canonical.get(*lang).cloned()
    }

    /// Pick the language for a repo-relative path, VS Code style:
    /// user associations > exact file names > extension > first line.
    pub fn lang_for(&self, path: &str, first_line: Option<&str>) -> Option<String> {
        let fname = path.rsplit('/').next().unwrap_or(path);
        for (glob, target) in &self.assoc {
            let subject = if glob.contains('/') { path } else { fname };
            if glob_match(glob.as_bytes(), subject.as_bytes()) {
                if let Some(s) = self.resolve(target) {
                    return Some(s);
                }
            }
        }
        if let Some(k) = self.user_filenames.get(fname) {
            return self.canonical.get(k).cloned();
        }
        if let Some((_, l)) = langs::FILENAMES.iter().find(|(f, _)| *f == fname) {
            return self.canonical.get(*l).cloned();
        }
        // Longest compound extension first: "d.ts" before "ts".
        let mut rest = fname;
        while let Some((_, ext)) = rest.split_once('.') {
            if let Some(s) = self.by_ext(ext) {
                return Some(s);
            }
            rest = ext;
        }
        let line = first_line?;
        for (re, k) in &self.user_first_lines {
            if re.is_match(line).unwrap_or(false) {
                return self.canonical.get(k).cloned();
            }
        }
        self.resolve(langs::from_first_line(line)?)
    }

    /// Highlight `lines` (each ending in '\n') from the top, stopping after `upto` lines.
    pub fn highlight(&self, lang: &str, lines: &[String], upto: usize) -> Vec<Line> {
        let n = upto.min(lines.len());
        // Minified/generated files: stop rather than stall.
        let n = lines[..n].iter().position(|l| l.len() > 5000).unwrap_or(n);
        if n == 0 {
            return Vec::new();
        }
        // giallo breaks lines at a lone '\r' too; blank those to keep our numbering.
        let text: String = lines[..n]
            .iter()
            .map(|l| {
                let body = l.trim_end_matches(['\n', '\r']);
                format!("{}\n", body.replace('\r', " "))
            })
            .collect();
        let opts = HighlightOptions::new(lang, ThemeVariant::Single(&self.theme_name));
        let Ok(hl) = self.hl.highlight(&text, &opts) else {
            return Vec::new();
        };
        let mut out: Vec<Line> = hl
            .tokens
            .into_iter()
            .take(n)
            .map(|toks| {
                toks.into_iter()
                    .filter_map(|t| {
                        let ThemeVariant::Single(st) = t.style else {
                            return None;
                        };
                        Some(Span {
                            rgb: hex_rgb(&st.foreground.as_hex()),
                            italic: st.font_style.contains(FontStyle::ITALIC),
                            underline: st.font_style.contains(FontStyle::UNDERLINE),
                            text: t.text.trim_end_matches('\n').to_owned(),
                        })
                    })
                    .collect()
            })
            .collect();
        // Put back the '\r's we blanked, so spans reproduce the source exactly.
        for (line, src) in out.iter_mut().zip(lines) {
            let body = src.trim_end_matches(['\n', '\r']);
            if body.contains('\r') {
                let mut at = 0;
                for sp in line.iter_mut() {
                    let len = sp.text.len();
                    if let Some(orig) = body.get(at..at + len) {
                        sp.text = orig.to_owned();
                    }
                    at += len;
                }
            }
        }
        out
    }
}

/// Names user grammars get inside giallo, so they never clash with a builtin.
const USER_PREFIX: &str = "user:";

fn hex_rgb(h: &str) -> [u8; 3] {
    let h = h.trim_start_matches('#');
    let c = |i: usize| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok());
    [
        c(0).unwrap_or(0xd4),
        c(2).unwrap_or(0xd4),
        c(4).unwrap_or(0xd4),
    ]
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
    use super::*;

    /// The builtin set only: no user config.
    fn builtin() -> Registry {
        let hl = giallo::Registry::builtin().unwrap();
        let mut r = Registry {
            hl,
            theme_name: DEFAULT_THEME.to_owned(),
            themes: Vec::new(),
            user_names: Vec::new(),
            canonical: HashMap::new(),
            user_exts: HashMap::new(),
            user_filenames: HashMap::new(),
            user_scopes: HashMap::new(),
            user_first_lines: Vec::new(),
            assoc: Vec::new(),
            warnings: Vec::new(),
        };
        for n in r.hl.get_grammar_names() {
            for key in std::iter::once(n.name).chain(n.aliases.iter().map(String::as_str)) {
                r.canonical
                    .entry(key.to_lowercase())
                    .or_insert_with(|| n.name.to_owned());
            }
        }
        r
    }

    fn lines(src: &[&str]) -> Vec<String> {
        src.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn detects_languages() {
        let mut r = builtin();
        let l = |r: &Registry, p: &str, f: Option<&str>| r.lang_for(p, f);
        assert_eq!(l(&r, "src/main.rs", None).as_deref(), Some("rust"));
        assert_eq!(l(&r, "a/b.d.ts", None).as_deref(), Some("typescript"));
        assert_eq!(l(&r, "x.hpp", None).as_deref(), Some("cpp"));
        assert_eq!(l(&r, "Dockerfile", None).as_deref(), Some("docker"));
        assert_eq!(
            l(&r, "run", Some("#!/usr/bin/env python3\n")).as_deref(),
            Some("python")
        );
        assert_eq!(l(&r, "notes.unknownext", None), None);
        r.assoc = vec![("*.mdx".to_owned(), "md".to_owned())];
        assert_eq!(l(&r, "a.mdx", None).as_deref(), Some("markdown"));
    }

    #[test]
    fn spans_reproduce_lines() {
        let r = builtin();
        let src = lines(&[
            "fn main() {\n",
            "    let x = 1;\r\n",
            "\tlet y = \"a\rb\";\n",
            "}\n",
        ]);
        let hl = r.highlight("rust", &src, src.len());
        assert_eq!(hl.len(), src.len());
        for (spans, line) in hl.iter().zip(&src) {
            let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
            assert_eq!(joined, line.trim_end_matches(['\n', '\r']));
        }
        assert!(hl[0].len() > 1, "fn main should get more than one color");
    }

    /// PHP 8 attributes aren't comments, and the code after one on its line isn't
    /// either; a `#` comment still is.
    #[test]
    fn php_attributes() {
        let r = builtin();
        let src = lines(&[
            "<?php\n",
            "#[Route('/a', methods: ['GET'])] public function bar() {}\n",
            "# a comment\n",
        ]);
        let hl = r.highlight("php", &src, src.len());
        let color = |line: usize, text: &str| {
            hl[line]
                .iter()
                .find(|s| s.text.contains(text))
                .unwrap_or_else(|| panic!("{text:?} not in {:?}", hl[line]))
                .rgb
        };
        let comment = color(2, "a comment");
        for t in ["'/a'", "public", "function", "bar"] {
            assert_ne!(color(1, t), comment, "{t} is comment-colored");
        }
    }

    #[test]
    fn user_grammars_and_themes() {
        let dir = crate::testutil::temp_dir("hl-user");
        let w = |p: &str, s: &str| {
            let p = dir.join(p);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, s).unwrap();
        };
        w(
            "grammars/verd.tmLanguage.json",
            r#"{ "name": "Verd", "scopeName": "source.verd", "fileTypes": ["verd"],
                 "patterns": [{"match": "\\bapprove\\b", "name": "keyword.control.verd"},
                              {"begin": "```js", "end": "```", "patterns": [{"include": "source.js"}]}] }"#,
        );
        w(
            "themes/base.json",
            r##"{ "colors": {"editor.foreground": "#aaaaaa"},
                  "tokenColors": [{"scope": "keyword", "settings": {"foreground": "#ff0000"}}] }"##,
        );
        w(
            "themes/mine-color-theme.json",
            "{ // jsonc, with an include\n \"include\": \"./base.json\", \"tokenColors\": [], }",
        );
        w("grammars/old.sublime-syntax", "");
        w(
            "config.toml",
            "[highlight]\ntheme = \"mine\"\n[languages]\n\"*.vd\" = \"source.verd\"\n\"*.x\" = \"nope\"\n",
        );
        let r = Registry::load_from(&dir);
        assert_eq!(r.theme_name, "mine");
        assert!(r.themes.contains(&("mine".to_owned(), true)));
        assert_eq!(r.user_names, vec!["Verd"]);
        assert_eq!(r.lang_for("t.verd", None).as_deref(), Some("Verd"));
        assert_eq!(r.lang_for("t.vd", None).as_deref(), Some("Verd"));
        let w = r.warnings.join("\n");
        assert!(w.contains("old.sublime-syntax"), "{w}");
        assert!(w.contains("\"nope\""), "{w}");

        let hl = r.highlight(
            "Verd",
            &lines(&["approve\n", "```js\n", "if (x) {}\n", "```\n"]),
            4,
        );
        assert_eq!(
            hl[0][0].rgb,
            [0xff, 0, 0],
            "keyword color from the included theme"
        );
        assert_eq!(hl[2][0].text, "if", "embedded JavaScript is tokenized");
        assert_eq!(hl[2][0].rgb, [0xff, 0, 0]);
    }

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
