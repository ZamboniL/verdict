// SPDX-License-Identifier: GPL-3.0-or-later
//! What giallo doesn't know: which file gets which grammar, and its theme names.
//! giallo looks grammars up by name or alias only, so extensions that aren't one of
//! those, exact file names and `#!` lines are mapped here.

/// giallo's builtin themes (0.6). It has no way to list them yet.
pub const BUILTIN_THEMES: &[&str] = &[
    "andromeeda",
    "aurora-x",
    "ayu-dark",
    "ayu-light",
    "ayu-mirage",
    "catppuccin-frappe",
    "catppuccin-latte",
    "catppuccin-macchiato",
    "catppuccin-mocha",
    "dark-plus",
    "dracula",
    "dracula-soft",
    "everforest-dark",
    "everforest-light",
    "github-dark",
    "github-dark-default",
    "github-dark-dimmed",
    "github-dark-high-contrast",
    "github-light",
    "github-light-default",
    "github-light-high-contrast",
    "gruvbox-dark-hard",
    "gruvbox-dark-medium",
    "gruvbox-dark-soft",
    "gruvbox-light-hard",
    "gruvbox-light-medium",
    "gruvbox-light-soft",
    "horizon",
    "horizon-bright",
    "houston",
    "kanagawa-dragon",
    "kanagawa-lotus",
    "kanagawa-wave",
    "laserwave",
    "light-plus",
    "material-theme",
    "material-theme-darker",
    "material-theme-lighter",
    "material-theme-ocean",
    "material-theme-palenight",
    "min-dark",
    "min-light",
    "monokai",
    "night-owl",
    "night-owl-light",
    "nord",
    "one-dark-pro",
    "one-light",
    "plastic",
    "poimandres",
    "red",
    "rose-pine",
    "rose-pine-dawn",
    "rose-pine-moon",
    "slack-dark",
    "slack-ochin",
    "snazzy-light",
    "solarized-dark",
    "solarized-light",
    "synthwave-84",
    "tokyo-night",
    "vesper",
    "vitesse-black",
    "vitesse-dark",
    "vitesse-light",
];

/// Extensions whose language isn't a builtin grammar name or alias.
pub const EXTENSIONS: &[(&str, &str)] = &[
    ("h", "c"),
    ("hpp", "cpp"),
    ("hh", "cpp"),
    ("hxx", "cpp"),
    ("cc", "cpp"),
    ("cxx", "cpp"),
    ("ino", "cpp"),
    ("htm", "html"),
    ("xhtml", "html"),
    ("markdown", "markdown"),
    ("mdown", "markdown"),
    ("mkd", "markdown"),
    ("pyi", "python"),
    ("pyw", "python"),
    ("rake", "ruby"),
    ("gemspec", "ruby"),
    ("ru", "ruby"),
    ("ksh", "shellscript"),
    ("bash", "shellscript"),
    ("zsh", "shellscript"),
    ("el", "emacs-lisp"),
    ("ex", "elixir"),
    ("exs", "elixir"),
    ("hrl", "erlang"),
    ("ml", "ocaml"),
    ("mli", "ocaml"),
    ("gradle", "groovy"),
    ("sc", "scala"),
    ("pl", "perl"),
    ("pm", "perl"),
    ("t", "perl"),
    ("svg", "xml"),
    ("plist", "xml"),
    ("csproj", "xml"),
    ("fsproj", "xml"),
    ("xaml", "xml"),
    ("psm1", "powershell"),
    ("psd1", "powershell"),
    ("cfg", "ini"),
    ("conf", "ini"),
    ("containerfile", "docker"),
    ("mk", "make"),
    ("m", "objective-c"),
    ("mm", "objective-cpp"),
    ("fsx", "fsharp"),
    ("fsi", "fsharp"),
    ("env", "dotenv"),
    ("patch", "diff"),
    ("vert", "glsl"),
    ("frag", "glsl"),
    ("sty", "latex"),
    ("cls", "latex"),
    ("lock", "toml"),
    ("code-workspace", "jsonc"),
];

/// Exact file names.
pub const FILENAMES: &[(&str, &str)] = &[
    ("Dockerfile", "docker"),
    ("Containerfile", "docker"),
    ("Makefile", "make"),
    ("makefile", "make"),
    ("GNUmakefile", "make"),
    ("CMakeLists.txt", "cmake"),
    ("Gemfile", "ruby"),
    ("Rakefile", "ruby"),
    ("Guardfile", "ruby"),
    ("Podfile", "ruby"),
    ("Vagrantfile", "ruby"),
    ("Jenkinsfile", "groovy"),
    ("justfile", "just"),
    ("Justfile", "just"),
    (".bashrc", "shellscript"),
    (".bash_profile", "shellscript"),
    (".zshrc", "shellscript"),
    (".zprofile", "shellscript"),
    (".profile", "shellscript"),
    (".gitconfig", "ini"),
    (".editorconfig", "ini"),
    (".env", "dotenv"),
    ("CODEOWNERS", "codeowners"),
    ("COMMIT_EDITMSG", "git-commit"),
    ("git-rebase-todo", "git-rebase"),
    ("Cargo.lock", "toml"),
    ("go.mod", "go"),
];

/// `#!` interpreters (version digits stripped).
const INTERPRETERS: &[(&str, &str)] = &[
    ("sh", "shellscript"),
    ("bash", "shellscript"),
    ("zsh", "shellscript"),
    ("ksh", "shellscript"),
    ("dash", "shellscript"),
    ("node", "javascript"),
    ("nodejs", "javascript"),
    ("deno", "typescript"),
    ("bun", "typescript"),
    ("tsx", "typescript"),
    ("ts-node", "typescript"),
    ("python", "python"),
    ("ruby", "ruby"),
    ("perl", "perl"),
    ("php", "php"),
    ("lua", "lua"),
    ("fish", "fish"),
    ("pwsh", "powershell"),
    ("Rscript", "r"),
    ("elixir", "elixir"),
    ("awk", "awk"),
    ("gawk", "awk"),
    ("make", "make"),
    ("nu", "nushell"),
];

/// The language a first line names: a `#!` interpreter, or `<?php` / `<?xml`.
/// "#!/usr/bin/env python3" -> "python".
pub fn from_first_line(line: &str) -> Option<&'static str> {
    if line.starts_with("<?php") {
        return Some("php");
    }
    if line.starts_with("<?xml") {
        return Some("xml");
    }
    let rest = line.strip_prefix("#!")?.trim();
    let mut words = rest.split_whitespace();
    let mut prog = words.next()?.rsplit('/').next()?;
    if prog == "env" {
        prog = words.find(|w| !w.starts_with('-') && !w.contains('='))?;
    }
    let base = prog.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    INTERPRETERS
        .iter()
        .find(|(i, _)| *i == base)
        .map(|(_, l)| *l)
}

#[cfg(test)]
mod tests {
    use super::from_first_line;

    #[test]
    fn first_lines() {
        assert_eq!(from_first_line("#!/usr/bin/env python3\n"), Some("python"));
        assert_eq!(from_first_line("#!/bin/bash -e"), Some("shellscript"));
        assert_eq!(
            from_first_line("#!/usr/bin/env -S deno run"),
            Some("typescript")
        );
        assert_eq!(from_first_line("<?php"), Some("php"));
        assert_eq!(from_first_line("# just a comment"), None);
    }
}
