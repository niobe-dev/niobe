// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The languages the highlighter reads, and how a fence's label or a file's
//! name says which one a piece of code is in.

use super::spec::{self, Grammar};

/// A language the highlighter reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    /// Bash, sh, zsh and the other POSIX shells.
    Shell,
    /// C, and its headers.
    C,
    /// C++.
    Cpp,
    /// C#.
    CSharp,
    /// CSS, and SCSS and Less, which write it the same way at a lexer's
    /// distance.
    Css,
    /// Dart.
    Dart,
    /// A unified diff.
    Diff,
    /// A Dockerfile.
    Dockerfile,
    /// Elixir.
    Elixir,
    /// Go.
    Go,
    /// Haskell.
    Haskell,
    /// HTML, and XML and SVG, which are written in the same tags.
    Html,
    /// Java.
    Java,
    /// JavaScript, and JSX.
    JavaScript,
    /// JSON, with the comments JSONC allows.
    Json,
    /// Kotlin.
    Kotlin,
    /// Lua.
    Lua,
    /// A makefile.
    Make,
    /// Miri.
    Miri,
    /// Objective-C.
    ObjectiveC,
    /// Perl.
    Perl,
    /// PHP.
    Php,
    /// PowerShell.
    PowerShell,
    /// Python.
    Python,
    /// R.
    R,
    /// Ruby.
    Ruby,
    /// Rust.
    Rust,
    /// Scala.
    Scala,
    /// SQL, in any dialect's common words.
    Sql,
    /// Swift.
    Swift,
    /// TOML, and INI files, which write their sections and keys alike.
    Toml,
    /// TypeScript, and TSX.
    TypeScript,
    /// YAML.
    Yaml,
    /// Zig.
    Zig,
}

impl Language {
    /// Every language there is.
    pub const ALL: [Language; 34] = [
        Language::Shell,
        Language::C,
        Language::Cpp,
        Language::CSharp,
        Language::Css,
        Language::Dart,
        Language::Diff,
        Language::Dockerfile,
        Language::Elixir,
        Language::Go,
        Language::Haskell,
        Language::Html,
        Language::Java,
        Language::JavaScript,
        Language::Json,
        Language::Kotlin,
        Language::Lua,
        Language::Make,
        Language::Miri,
        Language::ObjectiveC,
        Language::Perl,
        Language::Php,
        Language::PowerShell,
        Language::Python,
        Language::R,
        Language::Ruby,
        Language::Rust,
        Language::Scala,
        Language::Sql,
        Language::Swift,
        Language::Toml,
        Language::TypeScript,
        Language::Yaml,
        Language::Zig,
    ];

    /// The language a code fence's label names — ```` ```rust ````,
    /// ```` ```py title="x" ````, Pandoc's ```` ```{.ts} ```` — by its first
    /// word, in any case, or `None`
    /// where the label names none the highlighter reads.
    pub fn from_label(label: &str) -> Option<Self> {
        let word = label
            .split(|c: char| c.is_whitespace() || matches!(c, ',' | '{' | '}' | ':'))
            .map(|word| word.trim_start_matches('.'))
            .find(|word| !word.is_empty())?;
        Self::named(&word.to_ascii_lowercase())
    }

    /// The language a file at `path` is written in, by its extension or, for
    /// the files named for what they are, by its name: `Dockerfile`,
    /// `Makefile`. `None` where neither says.
    pub fn from_path(path: &str) -> Option<Self> {
        let name = path.rsplit(is_path_separator).next().unwrap_or(path);
        let lower = name.to_ascii_lowercase();
        if let Some(language) = Self::by_file_name(&lower) {
            return Some(language);
        }
        let (stem, extension) = lower.rsplit_once('.')?;
        if stem.is_empty() {
            // A dot file, as `.bashrc`: its name is all it has to go by.
            return Self::named(extension.trim_end_matches("rc"));
        }
        Self::named(extension)
    }

    /// The language of a file called `name`, in lower case, where the whole
    /// name says it.
    fn by_file_name(name: &str) -> Option<Self> {
        Some(match name {
            "dockerfile" | "containerfile" => Language::Dockerfile,
            "makefile" | "gnumakefile" => Language::Make,
            "cargo.lock" | "pipfile" | ".editorconfig" => Language::Toml,
            "gemfile" | "rakefile" => Language::Ruby,
            ".zshrc" | ".bashrc" | ".profile" | ".bash_profile" | ".zprofile" => Language::Shell,
            _ if name.starts_with("dockerfile.") => Language::Dockerfile,
            _ => return None,
        })
    }

    /// The language `name` — a fence's word or an extension, in lower case —
    /// stands for.
    fn named(name: &str) -> Option<Self> {
        Some(match name {
            "sh" | "bash" | "zsh" | "ksh" | "shell" | "console" | "shellsession" | "fish" => {
                Language::Shell
            }
            "c" | "h" => Language::C,
            "cpp" | "c++" | "cc" | "cxx" | "hpp" | "hh" | "hxx" | "ino" => Language::Cpp,
            "cs" | "csharp" | "c#" => Language::CSharp,
            "css" | "scss" | "sass" | "less" => Language::Css,
            "dart" => Language::Dart,
            "diff" | "patch" => Language::Diff,
            "dockerfile" | "docker" | "containerfile" => Language::Dockerfile,
            "ex" | "exs" | "elixir" | "heex" => Language::Elixir,
            "go" | "golang" => Language::Go,
            "hs" | "lhs" | "haskell" => Language::Haskell,
            "html" | "htm" | "xhtml" | "xml" | "svg" | "plist" | "xsl" | "xsd" => Language::Html,
            "java" => Language::Java,
            "js" | "javascript" | "jsx" | "mjs" | "cjs" | "node" => Language::JavaScript,
            "json" | "jsonc" | "json5" | "jsonl" | "ndjson" | "geojson" => Language::Json,
            "kt" | "kts" | "kotlin" => Language::Kotlin,
            "lua" => Language::Lua,
            "make" | "makefile" | "mk" | "mak" => Language::Make,
            "miri" | "mi" => Language::Miri,
            "objc" | "objective-c" | "objectivec" | "m" | "mm" => Language::ObjectiveC,
            "perl" | "pl" | "pm" => Language::Perl,
            "php" => Language::Php,
            "ps1" | "psm1" | "psd1" | "powershell" | "pwsh" | "ps" => Language::PowerShell,
            "py" | "python" | "python3" | "py3" | "pyi" | "pyw" | "gyp" => Language::Python,
            "r" | "rscript" => Language::R,
            "rb" | "ruby" | "rake" | "gemspec" | "erb" => Language::Ruby,
            "rs" | "rust" => Language::Rust,
            "scala" | "sc" | "sbt" => Language::Scala,
            "sql" | "psql" | "mysql" | "sqlite" | "pgsql" | "plsql" => Language::Sql,
            "swift" => Language::Swift,
            "toml" | "ini" | "cfg" | "conf" | "properties" | "desktop" => Language::Toml,
            "ts" | "typescript" | "tsx" | "mts" | "cts" => Language::TypeScript,
            "yaml" | "yml" => Language::Yaml,
            "zig" | "zon" => Language::Zig,
            _ => return None,
        })
    }

    /// How the language is read.
    pub(super) fn spec(self) -> Grammar {
        match self {
            Language::Shell => Grammar::Code(&spec::SHELL),
            Language::C => Grammar::Code(&spec::C),
            Language::Cpp => Grammar::Code(&spec::CPP),
            Language::CSharp => Grammar::Code(&spec::CSHARP),
            Language::Css => Grammar::Code(&spec::CSS),
            Language::Dart => Grammar::Code(&spec::DART),
            Language::Diff => Grammar::Diff,
            Language::Dockerfile => Grammar::Code(&spec::DOCKERFILE),
            Language::Elixir => Grammar::Code(&spec::ELIXIR),
            Language::Go => Grammar::Code(&spec::GO),
            Language::Haskell => Grammar::Code(&spec::HASKELL),
            Language::Html => Grammar::Markup,
            Language::Java => Grammar::Code(&spec::JAVA),
            Language::JavaScript => Grammar::Code(&spec::JAVASCRIPT),
            Language::Json => Grammar::Code(&spec::JSON),
            Language::Kotlin => Grammar::Code(&spec::KOTLIN),
            Language::Lua => Grammar::Code(&spec::LUA),
            Language::Make => Grammar::Code(&spec::MAKE),
            Language::Miri => Grammar::Code(&spec::MIRI),
            Language::ObjectiveC => Grammar::Code(&spec::OBJECTIVE_C),
            Language::Perl => Grammar::Code(&spec::PERL),
            Language::Php => Grammar::Code(&spec::PHP),
            Language::PowerShell => Grammar::Code(&spec::POWERSHELL),
            Language::Python => Grammar::Code(&spec::PYTHON),
            Language::R => Grammar::Code(&spec::R),
            Language::Ruby => Grammar::Code(&spec::RUBY),
            Language::Rust => Grammar::Code(&spec::RUST),
            Language::Scala => Grammar::Code(&spec::SCALA),
            Language::Sql => Grammar::Code(&spec::SQL),
            Language::Swift => Grammar::Code(&spec::SWIFT),
            Language::Toml => Grammar::Code(&spec::TOML),
            Language::TypeScript => Grammar::Code(&spec::TYPESCRIPT),
            Language::Yaml => Grammar::Code(&spec::YAML),
            Language::Zig => Grammar::Code(&spec::ZIG),
        }
    }
}

// Searched with a named function rather than a char array: a `[char; N]`
// pattern shares its searcher with every other one of that length in the
// crate, and under the release profile's `opt-level = "z"` one more caller
// stopped it being inlined into `text::one_line`, which a megabyte prompt
// runs through on every frame.
/// Whether `c` separates a path's directories, on any system.
fn is_path_separator(c: char) -> bool {
    matches!(c, '/' | '\\')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fence_label_names_its_language_by_its_first_word_in_any_case() {
        assert_eq!(Language::from_label("rust"), Some(Language::Rust));
        assert_eq!(
            Language::from_label("Python title=\"x\""),
            Some(Language::Python)
        );
        assert_eq!(Language::from_label("rust,ignore"), Some(Language::Rust));
        assert_eq!(Language::from_label("{.ts}"), Some(Language::TypeScript));
        assert_eq!(Language::from_label("miri"), Some(Language::Miri));
        assert_eq!(Language::from_label("text"), None);
        assert_eq!(Language::from_label(""), None);
    }

    #[test]
    fn a_path_names_its_language_by_extension_or_by_the_whole_name() {
        assert_eq!(
            Language::from_path("crates/tui/src/ui.rs"),
            Some(Language::Rust)
        );
        assert_eq!(
            Language::from_path("/repo/App.TSX"),
            Some(Language::TypeScript)
        );
        assert_eq!(
            Language::from_path("bench/x/spectral-norm.mi"),
            Some(Language::Miri)
        );
        assert_eq!(
            Language::from_path("docker/Dockerfile"),
            Some(Language::Dockerfile)
        );
        assert_eq!(Language::from_path("Makefile"), Some(Language::Make));
        assert_eq!(
            Language::from_path("/home/me/.zshrc"),
            Some(Language::Shell)
        );
        assert_eq!(Language::from_path("C:\\src\\main.go"), Some(Language::Go));
        assert_eq!(Language::from_path("README"), None);
        assert_eq!(Language::from_path("notes.txt"), None);
        assert_eq!(Language::from_path(""), None);
    }

    #[test]
    fn every_language_is_listed_once() {
        for (at, language) in Language::ALL.iter().enumerate() {
            assert!(
                !Language::ALL[..at].contains(language),
                "{language:?} twice"
            );
        }
    }
}
