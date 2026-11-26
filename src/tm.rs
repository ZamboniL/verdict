//! Loading the VS Code / TextMate ecosystem into syntect:
//! - TextMate grammars (`.tmLanguage.json`, `.tmLanguage` plist) are converted to
//!   Sublime's `.sublime-syntax` model, which is what syntect understands.
//! - VS Code color themes (JSON with `tokenColors`) are converted to syntect themes.

use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::str::FromStr;
use syntect::highlighting::{
    Color, FontStyle, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings,
};

// ---------------------------------------------------------------- file reading

/// Parse JSON that may contain comments and trailing commas (VS Code's "jsonc").
pub fn parse_jsonc(src: &str) -> Result<Value, String> {
    // Walks bytes outside string literals; `f` decides what to emit at position i
    // and returns how many bytes it consumed.
    fn scan(b: &[u8], mut f: impl FnMut(&[u8], usize, &mut Vec<u8>) -> usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(b.len());
        let (mut i, mut in_str) = (0, false);
        while i < b.len() {
            let c = b[i];
            if in_str {
                out.push(c);
                if c == b'\\' && i + 1 < b.len() {
                    out.push(b[i + 1]);
                    i += 1;
                } else if c == b'"' {
                    in_str = false;
                }
                i += 1;
            } else if c == b'"' {
                in_str = true;
                out.push(c);
                i += 1;
            } else {
                i += f(b, i, &mut out);
            }
        }
        out
    }
    // Pass 1: strip comments.
    let no_comments = scan(src.as_bytes(), |b, i, out| {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
            let mut j = i;
            while j < b.len() && b[j] != b'\n' {
                j += 1;
            }
            j - i
        } else if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            let mut j = i + 2;
            while j + 1 < b.len() && !(b[j] == b'*' && b[j + 1] == b'/') {
                j += 1;
            }
            (j + 2).min(b.len()) - i
        } else {
            out.push(b[i]);
            1
        }
    });
    // Pass 2: drop trailing commas.
    let out = scan(&no_comments, |b, i, out| {
        if b[i] == b',' {
            let mut j = i + 1;
            while j < b.len() && b[j].is_ascii_whitespace() {
                j += 1;
            }
            if matches!(b.get(j), Some(b'}') | Some(b']')) {
                return 1;
            }
        }
        out.push(b[i]);
        1
    });
    let s = String::from_utf8(out).map_err(|e| e.to_string())?;
    serde_json::from_str(&s).map_err(|e| e.to_string())
}

fn plist_to_json(v: plist::Value) -> Value {
    match v {
        plist::Value::Dictionary(d) => {
            Value::Object(d.into_iter().map(|(k, v)| (k, plist_to_json(v))).collect())
        }
        plist::Value::Array(a) => Value::Array(a.into_iter().map(plist_to_json).collect()),
        plist::Value::String(s) => Value::String(s),
        plist::Value::Boolean(b) => Value::Bool(b),
        plist::Value::Integer(i) => i.as_signed().map(Value::from).unwrap_or(Value::Null),
        plist::Value::Real(f) => Value::from(f),
        _ => Value::Null,
    }
}

/// Read a JSON (jsonc) or plist-XML document.
pub fn read_doc(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    if text.trim_start().starts_with('<') {
        plist::Value::from_reader_xml(text.as_bytes())
            .map(plist_to_json)
            .map_err(|e| e.to_string())
    } else {
        parse_jsonc(&text)
    }
}

// ---------------------------------------------------------------- tiny YAML emitter

/// We emit YAML flow style (JSON-like) because syntect only parses `.sublime-syntax` text.
enum Y {
    S(String),
    I(i64),
    B(bool),
    L(Vec<Y>),
    M(Vec<(Y, Y)>),
}

fn s(x: impl Into<String>) -> Y {
    Y::S(x.into())
}

fn emit(y: &Y, out: &mut String) {
    match y {
        Y::S(x) => out.push_str(&serde_json::to_string(x).unwrap()),
        Y::I(i) => out.push_str(&i.to_string()),
        Y::B(b) => out.push_str(if *b { "true" } else { "false" }),
        Y::L(v) => {
            out.push('[');
            for (i, x) in v.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                emit(x, out);
            }
            out.push(']');
        }
        Y::M(v) => {
            out.push('{');
            for (i, (k, x)) in v.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                emit(k, out);
                out.push_str(": ");
                emit(x, out);
            }
            out.push('}');
        }
    }
}

// ---------------------------------------------------------------- grammar conversion

/// A TextMate grammar plus the language metadata that VS Code keeps in package.json.
pub struct TmGrammar {
    pub json: Value,
    pub name: String,
    pub scope: String,
    pub extensions: Vec<String>,
    pub first_line: Option<String>,
}

impl TmGrammar {
    pub fn from_value(json: Value, name_hint: Option<&str>) -> Result<TmGrammar, String> {
        let scope = json["scopeName"]
            .as_str()
            .ok_or("grammar has no scopeName")?
            .to_owned();
        let name = name_hint
            .map(str::to_owned)
            .or_else(|| json["name"].as_str().map(str::to_owned))
            .unwrap_or_else(|| scope.clone());
        let extensions = json["fileTypes"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str())
                    .map(|x| x.trim_start_matches('.').to_owned())
                    .collect()
            })
            .unwrap_or_default();
        let first_line = json["firstLineMatch"].as_str().map(str::to_owned);
        Ok(TmGrammar { json, name, scope, extensions, first_line })
    }
}

/// Sanitize a TextMate scope string for syntect: drop `$1`-style capture
/// substitutions (unsupported) and cap each scope at syntect's 8-atom limit.
fn clean_scope(name: &str) -> Option<String> {
    let parts: Vec<String> = name
        .split_whitespace()
        .filter_map(|scope| {
            let atoms: Vec<&str> = scope
                .split('.')
                .take_while(|a| !a.contains('$') && !a.is_empty())
                .take(8)
                .collect();
            (!atoms.is_empty()).then(|| atoms.join("."))
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn ctx_name(repo_key: &str) -> String {
    let clean: String = repo_key
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    format!("r_{clean}")
}

struct Conv<'a> {
    self_scope: &'a str,
    /// Known grammar scopes -> whether it was converted from TextMate (so `#rule`
    /// sub-contexts follow our `r_` naming).
    known: &'a HashMap<String, bool>,
    repo: HashMap<String, Value>,
    dropped: usize,
}

impl Conv<'_> {
    fn collect_repo(&mut self, rule: &Value) {
        if let Some(r) = rule["repository"].as_object() {
            for (k, v) in r {
                self.repo.entry(k.clone()).or_insert_with(|| v.clone());
                self.collect_repo(v);
            }
        }
        for key in ["patterns"] {
            if let Some(a) = rule[key].as_array() {
                for p in a {
                    self.collect_repo(p);
                }
            }
        }
    }

    fn regex_ok(&mut self, re: &str) -> bool {
        // `end` patterns may back-reference `begin` captures (\1..\9); syntect substitutes
        // them at match time, so stub them out for the standalone validity check.
        let mut probe = String::with_capacity(re.len());
        let mut chars = re.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.peek() {
                    Some(d) if d.is_ascii_digit() && *d != '0' => {
                        chars.next();
                        probe.push_str("(?:)");
                    }
                    Some(_) => {
                        probe.push(c);
                        probe.push(chars.next().unwrap());
                    }
                    None => probe.push(c),
                }
            } else {
                probe.push(c);
            }
        }
        let ok = syntect::parsing::Regex::try_compile(&probe).is_none();
        if !ok {
            self.dropped += 1;
        }
        ok
    }

    fn captures(&self, v: &Value) -> Option<(Option<String>, Y)> {
        let obj = v.as_object()?;
        let mut whole = None;
        let mut items = Vec::new();
        for (k, c) in obj {
            let Ok(n) = k.parse::<i64>() else { continue };
            let Some(name) = c["name"].as_str().and_then(clean_scope) else { continue };
            if n == 0 {
                whole = Some(name);
            } else {
                items.push((Y::I(n), s(name)));
            }
        }
        Some((whole, Y::M(items)))
    }

    fn include(&self, inc: &str) -> Option<Y> {
        let target = match inc {
            "$self" | "$base" => "main".to_owned(),
            _ if inc.starts_with('#') => {
                let key = &inc[1..];
                self.repo.contains_key(key).then(|| ctx_name(key))?
            }
            _ => {
                let (scope, sub) = match inc.split_once('#') {
                    Some((a, b)) => (a, Some(b)),
                    None => (inc, None),
                };
                if scope == self.self_scope {
                    match sub {
                        Some(k) if self.repo.contains_key(k) => ctx_name(k),
                        Some(_) => return None,
                        None => "main".to_owned(),
                    }
                } else {
                    let is_tm = *self.known.get(scope)?;
                    match sub {
                        Some(k) if is_tm => format!("scope:{scope}#{}", ctx_name(k)),
                        _ => format!("scope:{scope}"),
                    }
                }
            }
        };
        Some(Y::M(vec![(s("include"), s(target))]))
    }

    fn rule(&mut self, r: &Value) -> Option<Y> {
        if r["disabled"].as_i64() == Some(1) || r["disabled"].as_bool() == Some(true) {
            return None;
        }
        if let Some(inc) = r["include"].as_str() {
            let y = self.include(inc);
            if y.is_none() {
                self.dropped += 1;
            }
            return y;
        }
        let name = r["name"].as_str().and_then(clean_scope);
        if let Some(m) = r["match"].as_str() {
            if !self.regex_ok(m) {
                return None;
            }
            let mut map = vec![(s("match"), s(m))];
            let caps = self.captures(&r["captures"]);
            let scope = name.or_else(|| caps.as_ref().and_then(|c| c.0.clone()));
            if let Some(sc) = scope {
                map.push((s("scope"), s(sc)));
            }
            if let Some((_, c)) = caps {
                map.push((s("captures"), c));
            }
            return Some(Y::M(map));
        }
        if let Some(begin) = r["begin"].as_str() {
            let end = match (r["end"].as_str(), r["while"].as_str()) {
                (Some(e), _) => e.to_owned(),
                // `while` has no Sublime equivalent; approximate as "ends when a line stops matching".
                (None, Some(w)) => format!("^(?!(?:{}))", w.trim_start_matches('^')),
                _ => return None,
            };
            if !self.regex_ok(begin) || !self.regex_ok(&end) {
                return None;
            }
            let shared = self.captures(&r["captures"]);
            let bcaps = self.captures(&r["beginCaptures"]).or_else(|| self.captures(&r["captures"]));
            let ecaps = self.captures(&r["endCaptures"]).or(shared);

            let mut end_rule = vec![(s("match"), s(end))];
            if let Some((whole, c)) = ecaps {
                if let Some(w) = whole {
                    end_rule.push((s("scope"), s(w)));
                }
                end_rule.push((s("captures"), c));
            }
            end_rule.push((s("pop"), Y::B(true)));
            let end_rule = Y::M(end_rule);

            let mut ctx = Vec::new();
            if let Some(n) = &name {
                ctx.push(Y::M(vec![(s("meta_scope"), s(n.clone()))]));
            }
            if let Some(cn) = r["contentName"].as_str().and_then(clean_scope) {
                ctx.push(Y::M(vec![(s("meta_content_scope"), s(cn))]));
            }
            let end_last = r["applyEndPatternLast"].as_i64() == Some(1)
                || r["applyEndPatternLast"].as_bool() == Some(true);
            if !end_last {
                ctx.push(end_rule);
                ctx.extend(self.patterns(&r["patterns"]));
            } else {
                ctx.extend(self.patterns(&r["patterns"]));
                ctx.push(end_rule);
            }

            let mut push = vec![(s("match"), s(begin))];
            let (whole, bc) = match bcaps {
                Some((w, c)) => (w, Some(c)),
                None => (None, None),
            };
            let scope = match (name, whole) {
                (Some(n), Some(w)) => Some(format!("{n} {w}")),
                (n, w) => n.or(w),
            };
            if let Some(sc) = scope {
                push.push((s("scope"), s(sc)));
            }
            if let Some(c) = bc {
                push.push((s("captures"), c));
            }
            push.push((s("push"), Y::L(ctx)));
            return Some(Y::M(push));
        }
        if r["patterns"].is_array() {
            let items = self.patterns(&r["patterns"]);
            return Some(Y::M(vec![(s("include"), Y::L(items))]));
        }
        None
    }

    fn patterns(&mut self, v: &Value) -> Vec<Y> {
        v.as_array()
            .map(|a| a.iter().filter_map(|r| self.rule(r)).collect())
            .unwrap_or_default()
    }

    /// A repository entry becomes a named context.
    fn context_for(&mut self, r: &Value) -> Vec<Y> {
        if r.get("match").is_some() || r.get("begin").is_some() || r.get("include").is_some() {
            self.rule(r).into_iter().collect()
        } else {
            self.patterns(&r["patterns"])
        }
    }
}

/// Convert a TextMate grammar to `.sublime-syntax` text. Returns the text and the
/// number of rules dropped (unsupported regex or unresolvable include).
pub fn convert(g: &TmGrammar, known: &HashMap<String, bool>) -> (String, usize) {
    let mut c = Conv { self_scope: &g.scope, known, repo: HashMap::new(), dropped: 0 };
    c.collect_repo(&g.json);
    let mut contexts = vec![(s("main"), Y::L(c.patterns(&g.json["patterns"])))];
    let mut keys: Vec<String> = c.repo.keys().cloned().collect();
    keys.sort();
    let mut seen = HashSet::new();
    for k in keys {
        let name = ctx_name(&k);
        if !seen.insert(name.clone()) {
            continue;
        }
        let rule = c.repo[&k].clone();
        let items = c.context_for(&rule);
        contexts.push((s(name), Y::L(items)));
    }
    let mut top = vec![
        (s("name"), s(g.name.clone())),
        (s("scope"), s(g.scope.clone())),
        (s("file_extensions"), Y::L(g.extensions.iter().map(|e| s(e.clone())).collect())),
    ];
    if let Some(fl) = &g.first_line {
        top.push((s("first_line_match"), s(fl.clone())));
    }
    top.push((s("contexts"), Y::M(contexts)));
    let mut out = String::new();
    emit(&Y::M(top), &mut out);
    (out, c.dropped)
}

// ---------------------------------------------------------------- VS Code themes

fn parse_color(s: &str) -> Option<Color> {
    let h = s.trim().strip_prefix('#')?;
    let hex = |i: usize, n: usize| u8::from_str_radix(&h[i..i + n], 16).ok();
    let dup = |v: u8| v * 17;
    Some(match h.len() {
        3 => Color { r: dup(hex(0, 1)?), g: dup(hex(1, 1)?), b: dup(hex(2, 1)?), a: 255 },
        4 => Color { r: dup(hex(0, 1)?), g: dup(hex(1, 1)?), b: dup(hex(2, 1)?), a: dup(hex(3, 1)?) },
        6 => Color { r: hex(0, 2)?, g: hex(2, 2)?, b: hex(4, 2)?, a: 255 },
        8 => Color { r: hex(0, 2)?, g: hex(2, 2)?, b: hex(4, 2)?, a: hex(6, 2)? },
        _ => return None,
    })
}

fn font_style(s: &str) -> FontStyle {
    let mut f = FontStyle::empty();
    for w in s.split_whitespace() {
        match w {
            "bold" => f |= FontStyle::BOLD,
            "italic" => f |= FontStyle::ITALIC,
            "underline" => f |= FontStyle::UNDERLINE,
            _ => {}
        }
    }
    f
}

fn token_colors(path: &Path, depth: usize) -> Result<(Vec<Value>, Map<String, Value>), String> {
    if depth > 4 {
        return Err("theme include chain too deep".into());
    }
    let v = read_doc(path)?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let (mut tokens, mut colors) = match v["include"].as_str() {
        Some(base) => token_colors(&dir.join(base), depth + 1)?,
        None => (Vec::new(), Map::new()),
    };
    if let Some(c) = v["colors"].as_object() {
        colors.extend(c.clone());
    }
    match &v["tokenColors"] {
        Value::Array(a) => tokens.extend(a.iter().cloned()),
        Value::String(p) => {
            let sub = read_doc(&dir.join(p))?;
            // a referenced .tmTheme keeps its rules under "settings"
            let arr = sub["settings"].as_array().or(sub["tokenColors"].as_array());
            tokens.extend(arr.into_iter().flatten().cloned());
        }
        _ => {}
    }
    // plain .tmTheme converted from plist
    if let Some(a) = v["settings"].as_array() {
        tokens.extend(a.iter().cloned());
    }
    Ok((tokens, colors))
}

/// Load a VS Code JSON color theme (or a plist .tmTheme) into a syntect Theme.
pub fn load_vscode_theme(path: &Path, name: &str) -> Result<Theme, String> {
    let (tokens, colors) = token_colors(path, 0)?;
    let mut settings = ThemeSettings::default();
    let col = |k: &str| colors.get(k).and_then(|v| v.as_str()).and_then(parse_color);
    settings.foreground = col("editor.foreground");
    settings.background = col("editor.background");
    let mut scopes = Vec::new();
    for t in tokens {
        let set = &t["settings"];
        let style = StyleModifier {
            foreground: set["foreground"].as_str().and_then(parse_color),
            background: None,
            font_style: set["fontStyle"].as_str().map(font_style),
        };
        let scope = match &t["scope"] {
            Value::String(x) => x.clone(),
            Value::Array(a) => a
                .iter()
                .filter_map(|x| x.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            _ => {
                // global settings entry
                if settings.foreground.is_none() {
                    settings.foreground = style.foreground;
                }
                continue;
            }
        };
        if let Ok(sel) = ScopeSelectors::from_str(&scope) {
            scopes.push(ThemeItem { scope: sel, style });
        }
    }
    if settings.foreground.is_none() {
        return Err("theme defines no colors".into());
    }
    Ok(Theme {
        name: Some(name.to_owned()),
        author: None,
        settings,
        scopes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonc() {
        let v = parse_jsonc("{ // c\n \"a\": [1, 2, // x\n], /* x */ \"b\": \"//not\", }").unwrap();
        assert_eq!(v["a"][1], 2);
        assert_eq!(v["b"], "//not");
    }

    #[test]
    fn converts_and_loads_in_syntect() {
        let g = serde_json::json!({
            "scopeName": "source.demo",
            "name": "Demo",
            "fileTypes": ["demo"],
            "patterns": [
                {"include": "#comment"},
                {"match": "\\b(let|fn)\\b", "name": "keyword.control.demo"},
                {"include": "#nope"},
                {"begin": "\"", "end": "\"", "name": "string.quoted.demo",
                 "patterns": [{"match": "\\\\.", "name": "constant.character.escape.demo"}]},
                {"begin": "(<<)(\\w+)", "end": "^\\2$", "beginCaptures": {"1": {"name": "keyword.operator.$1"}}}
            ],
            "repository": {
                "comment": {"begin": "/\\*", "end": "\\*/", "name": "comment.block.demo"}
            }
        });
        let tm = TmGrammar::from_value(g, None).unwrap();
        let (yaml, dropped) = convert(&tm, &HashMap::new());
        assert_eq!(dropped, 1); // #nope
        let def = syntect::parsing::SyntaxDefinition::load_from_str(&yaml, true, None)
            .unwrap_or_else(|e| panic!("{e}\n{yaml}"));
        let mut b = syntect::parsing::SyntaxSetBuilder::new();
        b.add(def);
        let ss = b.build();
        let syn = ss.find_syntax_by_extension("demo").unwrap();
        let mut st = syntect::parsing::ParseState::new(syn);
        let ops = st.parse_line("let x = \"a\\n\" /* c */\n", &ss).unwrap();
        let scopes: Vec<String> = ops
            .iter()
            .filter_map(|(_, op)| match op {
                syntect::parsing::ScopeStackOp::Push(s) => Some(s.build_string()),
                _ => None,
            })
            .collect();
        assert!(scopes.iter().any(|s| s == "keyword.control.demo"), "{scopes:?}");
        assert!(scopes.iter().any(|s| s == "string.quoted.demo"), "{scopes:?}");
        assert!(scopes.iter().any(|s| s == "constant.character.escape.demo"), "{scopes:?}");
        assert!(scopes.iter().any(|s| s == "comment.block.demo"), "{scopes:?}");
    }

    #[test]
    fn colors() {
        let c = parse_color("#abc").unwrap();
        assert_eq!((c.r, c.g, c.b), (0xaa, 0xbb, 0xcc));
        assert!(parse_color("#11223344").is_some());
    }
}
