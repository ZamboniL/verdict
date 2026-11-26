# verdict

Hunk-by-hunk review of AI-written changes. A small native app (Rust + egui) that shows
what the agent changed, lets you **approve**, **reject** (with a reason), **edit**
inline, or **comment** on each hunk, and sends the result back to the agent.

The git index is the "approved" state: approving stages a hunk, rejecting reverts it
in the working tree, and nothing is written until you submit.

## Usage

```
verdict [PATH]     review unstaged changes in the repo containing PATH
verdict hook       Claude Code Stop hook entry point
verdict langs      list languages (builtin + yours) and grammar warnings
verdict themes     list syntax themes (* = active)
verdict lang FILE  show which language FILE is highlighted as
verdict init       create ~/.config/verdict with an example config.toml
```

Keys: `j`/`k` move, `a` approve, `r` reject, `e` edit, `c` comment, `n` next
pending, `Esc` leave a text box, `Ctrl+Enter` submit.

### Claude Code hook

```json
"hooks": {
  "Stop": [{ "hooks": [{ "type": "command", "command": "verdict hook", "timeout": 86400 }] }]
}
```

When a turn ends with unreviewed changes the window opens. Approving everything lets
the agent stop; anything else becomes the agent's next instruction. Set
`VERDICT_DISABLE=1` to bypass it.

## Syntax highlighting

About 200 languages and 30 themes are built in (bat's collection, via two-face).
Whole files are highlighted from line 1 in the background, so hunks inside
multi-line comments and strings render correctly, and the inline editor is
highlighted as you type.

### Extending it

Everything lives in `$XDG_CONFIG_HOME/verdict` (default `~/.config/verdict`;
`verdict init` creates it):

| Path            | What goes there |
|-----------------|-----------------|
| `config.toml`   | theme choice and file associations |
| `grammars/`     | `.sublime-syntax`, `.tmLanguage` (plist), `.tmLanguage.json` |
| `themes/`       | `.tmTheme`, VS Code color themes (`.json`) |
| `extensions/`   | unpacked VS Code extensions |

**VS Code extensions.** A `.vsix` is a zip archive. Unzip it into
`extensions/<name>/`. verdict reads every `package.json` it finds there and uses
the `contributes` section:

- `grammars`: the TextMate grammars
- `languages`: `extensions`, `filenames` and `firstLine`, which decide what files a grammar applies to
- `themes`: color themes

```
unzip some-language.vsix -d ~/.config/verdict/extensions/some-language
verdict langs | grep user
```

**config.toml**

```toml
[highlight]
theme = "OneHalfDark"          # builtin or any theme you added

[languages]                    # like VS Code's files.associations
"*.mdx" = "Markdown"           # no "/" → matches the file name
"Justfile" = "Makefile"
"docker/**/*.env" = "source.shell.bash"   # with "/" → repo-relative path
```

Association values can be a language name (`verdict langs`), a scope
(`source.ts`) or an extension (`ts`).

**How a language is chosen:**
1. Your `[languages]` associations.
2. File names declared by extensions.
3. The file extension. The longest match wins, so `d.ts` beats `ts`.
4. The first line, for shebangs and similar.

User grammars override builtin ones for the same extension.

### How TextMate grammars are supported

syntect (the highlighting engine) only reads Sublime's `.sublime-syntax` format.
verdict converts TextMate grammars into it when it loads them:

- `begin`/`end` become push/pop contexts.
- `#repository` entries become named contexts.
- `include: source.x` resolves against every grammar verdict knows about.

Regexes run on Oniguruma, the same engine VS Code uses.

Known limits:
- `while` rules are approximated.
- Injection grammars (`injectTo`) are skipped.
- Capture-name substitutions such as `keyword.$1` are dropped.

Rules that can't be converted are skipped and reported by `verdict langs` and in
the window's toolbar.

The compiled set of user grammars is cached in `~/.cache/verdict`. It rebuilds
automatically when any grammar file changes.
