// SPDX-License-Identifier: GPL-3.0-or-later
//! Reading the VS Code / TextMate ecosystem's files into the strict JSON giallo
//! takes: jsonc and plist documents, and color themes with `include`s or in the
//! older `.tmTheme` form.

use serde_json::{Map, Value, json};
use std::path::Path;

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

pub(crate) fn token_colors(
    path: &Path,
    depth: usize,
) -> Result<(Vec<Value>, Map<String, Value>), String> {
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

/// A color theme (VS Code JSON, with `include`s, or a plist .tmTheme) as the JSON
/// giallo reads, named `name`.
pub fn theme_json(path: &Path, name: &str) -> Result<Value, String> {
    let (mut tokens, mut colors) = token_colors(path, 0)?;
    let light = read_doc(path)?["type"].as_str() == Some("light");
    // .tmTheme: the scope-less entry holds the editor colors.
    if let Some(i) = tokens.iter().position(|v| v.get("scope").is_none()) {
        let global = tokens.remove(i);
        for (from, to) in [
            ("foreground", "editor.foreground"),
            ("background", "editor.background"),
        ] {
            if let Some(c) = global["settings"][from].as_str() {
                colors.entry(to).or_insert_with(|| c.into());
            }
        }
    }
    // giallo requires both; fall back to VS Code's defaults.
    let (fg, bg) = if light {
        ("#000000", "#ffffff")
    } else {
        ("#d4d4d4", "#1e1e1e")
    };
    colors
        .entry("editor.foreground")
        .or_insert_with(|| fg.into());
    colors
        .entry("editor.background")
        .or_insert_with(|| bg.into());
    Ok(json!({
        "name": name,
        "type": if light { "light" } else { "dark" },
        "colors": colors,
        "tokenColors": tokens,
    }))
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
    fn tm_theme() {
        let dir = crate::testutil::temp_dir("tm-theme");
        let p = dir.join("t.tmTheme");
        std::fs::write(
            &p,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>settings</key><array>
  <dict><key>settings</key><dict><key>foreground</key><string>#112233</string></dict></dict>
  <dict><key>scope</key><string>keyword</string>
        <key>settings</key><dict><key>foreground</key><string>#ff0000</string></dict></dict>
</array></dict></plist>"#,
        )
        .unwrap();
        let t = theme_json(&p, "T").unwrap();
        assert_eq!(t["colors"]["editor.foreground"], "#112233");
        assert_eq!(t["colors"]["editor.background"], "#1e1e1e");
        assert_eq!(t["tokenColors"].as_array().unwrap().len(), 1);
    }
}
