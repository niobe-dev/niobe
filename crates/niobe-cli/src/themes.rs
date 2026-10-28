// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The themes the user wrote, in `themes/*.toml` beside their config.
//!
//! One file is one theme:
//!
//! ```toml
//! base = "cyber"        # a built-in theme; `cyber` where left out
//! name = "midnight"     # what the menu calls it; the file's name where left out
//!
//! [colors]
//! hot = "#ff8800"       # 24-bit, drawn on a terminal that can
//! fg = "white"          # one of the sixteen names, drawn everywhere
//!
//! [borders]
//! border = "rounded"
//! ```
//!
//! Every colour the file leaves out is the base's. A colour spelled in hex
//! is drawn only by a terminal that draws 24-bit colour; where the terminal
//! has the sixteen, the theme is the base's sixteen-colour table with the
//! colours spelled as names laid over it. A file that is not a theme, or one
//! whose colours put a pairing on top of itself, is refused at its key and
//! line, and the shell does not open on it.
//!
//! A theme file is the user's own, not something a repository carries: only
//! the user's config directory is looked in, so a clone cannot change how the
//! shell is drawn.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use niobe_tui::Theme;
use niobe_tui::theme::{self, UserTheme};
use toml::de::{DeTable, DeValue};

/// The problems found reading the themes, which the shell is refused on.
static PROBLEMS: OnceLock<Vec<String>> = OnceLock::new();

/// Reads the user's themes and makes them offered. Done before the arguments
/// are read, so that `--theme` can name one. What was wrong with a file is
/// kept for [`problem`] to say where a session is about to open.
pub fn register(directory: Option<PathBuf>) {
    let (themes, problems) = match directory {
        Some(directory) => read(&directory),
        None => (Vec::new(), Vec::new()),
    };
    let _ = theme::register_user(themes);
    let _ = PROBLEMS.set(problems);
}

/// The first thing wrong with a theme file, where something is.
pub fn problem() -> Option<&'static str> {
    PROBLEMS
        .get()
        .and_then(|problems| problems.first())
        .map(String::as_str)
}

/// Every theme in `directory`, in name order, and what was wrong with the
/// files that are not.
fn read(directory: &Path) -> (Vec<UserTheme>, Vec<String>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return (Vec::new(), Vec::new());
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    files.sort();
    let mut themes: Vec<UserTheme> = Vec::new();
    let mut problems = Vec::new();
    for path in files {
        let said = niobe_config::read::text(&path)
            .map_err(|error| format!("{}: {error}", path.display()))
            .and_then(|text| theme_of(&text, &path));
        match said {
            Ok(user) => {
                let name = user.sixteen.name;
                if themes
                    .iter()
                    .any(|had| had.sixteen.name.eq_ignore_ascii_case(name))
                {
                    problems.push(format!(
                        "{}: another theme file is already called `{name}`",
                        path.display()
                    ));
                } else {
                    themes.push(user);
                }
            }
            Err(problem) => problems.push(problem),
        }
    }
    (themes, problems)
}

/// The theme `text`, the file at `path`, spells.
fn theme_of(text: &str, path: &Path) -> Result<UserTheme, String> {
    let line = |at: usize| {
        text.get(..at)
            .map_or(1, |head| head.matches('\n').count() + 1)
    };
    let said = |at: usize, key: &str, message: &str| {
        format!("{}:{}: {key}: {message}", path.display(), line(at))
    };
    let root = DeTable::parse(text).map_err(|error| {
        let at = error.span().map_or(0, |span| span.start);
        format!("{}:{}: {}", path.display(), line(at), error.message())
    })?;
    let mut base = Theme::default();
    let mut name: Option<String> = None;
    let mut colours: Vec<(String, (theme::Colour, bool), usize)> = Vec::new();
    let mut borders: Vec<(String, theme::Line, usize)> = Vec::new();
    for (key, value) in root.get_ref() {
        let at = key.span().start;
        match key.get_ref().as_ref() {
            "base" => {
                let wanted = string(value, at, "base", &said)?;
                base = builtin(wanted).ok_or_else(|| {
                    said(
                        value.span().start,
                        "base",
                        &format!(
                            "`{wanted}` is not a built-in theme; expected {}",
                            builtin_names()
                        ),
                    )
                })?;
            }
            "name" => {
                let wanted = string(value, at, "name", &said)?;
                if wanted.is_empty() || wanted.chars().any(char::is_control) {
                    return Err(said(
                        value.span().start,
                        "name",
                        "a name has to be one line of text",
                    ));
                }
                name = Some(wanted.to_owned());
            }
            "colors" | "colours" => {
                let DeValue::Table(table) = value.get_ref() else {
                    return Err(said(at, "colors", "expected a table of colours"));
                };
                for (key, value) in table {
                    let at = key.span().start;
                    let shown = format!("colors.{}", key.get_ref());
                    if !Theme::COLOUR_KEYS.contains(&key.get_ref().as_ref()) {
                        return Err(said(
                            at,
                            &shown,
                            &format!(
                                "unknown colour; expected one of {}",
                                Theme::COLOUR_KEYS.join(", ")
                            ),
                        ));
                    }
                    let spelled = string(value, at, &shown, &said)?;
                    let parsed = theme::parse_colour(spelled).ok_or_else(|| {
                        said(
                            value.span().start,
                            &shown,
                            &format!(
                                "`{spelled}` is not a colour; expected #rrggbb or one of black, \
                                 red, green, yellow, blue, magenta, cyan, white, gray, dark-gray, \
                                 light-red, light-green, light-yellow, light-blue, light-magenta, \
                                 light-cyan"
                            ),
                        )
                    })?;
                    colours.push((key.get_ref().to_string(), parsed, value.span().start));
                }
            }
            "borders" => {
                let DeValue::Table(table) = value.get_ref() else {
                    return Err(said(at, "borders", "expected a table of lines"));
                };
                for (key, value) in table {
                    let at = key.span().start;
                    let shown = format!("borders.{}", key.get_ref());
                    if !Theme::BORDER_KEYS.contains(&key.get_ref().as_ref()) {
                        return Err(said(
                            at,
                            &shown,
                            &format!(
                                "unknown line; expected one of {}",
                                Theme::BORDER_KEYS.join(", ")
                            ),
                        ));
                    }
                    let spelled = string(value, at, &shown, &said)?;
                    let line = theme::parse_border(spelled).ok_or_else(|| {
                        said(
                            value.span().start,
                            &shown,
                            &format!(
                                "`{spelled}` is not a line; expected plain, rounded, double or thick"
                            ),
                        )
                    })?;
                    borders.push((key.get_ref().to_string(), line, value.span().start));
                }
            }
            other => {
                return Err(said(
                    at,
                    other,
                    "unknown key; expected `base`, `name`, `colors` or `borders`",
                ));
            }
        }
    }

    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let called = name.unwrap_or(stem);
    if called.is_empty() || called.chars().any(char::is_control) {
        return Err(format!(
            "{}: the theme has no name; set `name` or name the file",
            path.display()
        ));
    }
    if builtin(&called).is_some() {
        return Err(format!(
            "{}: `{called}` is the name of a built-in theme",
            path.display()
        ));
    }
    // A user theme's name lives as long as the process does, as the built-in
    // ones do: it is read once, and a handful of files is a handful of names.
    let leaked: &'static str = Box::leak(called.into_boxed_str());

    let mut sixteen = base.at(theme::Depth::Sixteen).named(leaked);
    let mut deep = base.at(theme::Depth::TrueColour).named(leaked);
    for (key, (colour, is_hex), at) in &colours {
        deep = deep
            .with_colour(key, *colour)
            .ok_or_else(|| said(*at, key, "unknown colour"))?;
        if !is_hex {
            sixteen = sixteen
                .with_colour(key, *colour)
                .ok_or_else(|| said(*at, key, "unknown colour"))?;
        }
    }
    for (key, line, at) in &borders {
        sixteen = sixteen
            .with_border(key, *line)
            .ok_or_else(|| said(*at, key, "unknown line"))?;
        deep = deep
            .with_border(key, *line)
            .ok_or_else(|| said(*at, key, "unknown line"))?;
    }

    for (palette, depth) in [(&sixteen, "sixteen-colour"), (&deep, "24-bit")] {
        if let Some(what) = palette.invisible().first() {
            return Err(format!(
                "{}: the {depth} table draws {what} on top of its own colour",
                path.display()
            ));
        }
    }
    if let Some((what, ratio)) = deep.illegible().first() {
        return Err(format!(
            "{}: the 24-bit table draws {what} at {ratio:.1} to 1 against its background; \
             3 to 1 is the least that can be read",
            path.display()
        ));
    }
    Ok(UserTheme { sixteen, deep })
}

/// What `value` is as text, or the error that says it is not.
fn string<'a>(
    value: &'a toml::Spanned<DeValue<'_>>,
    at: usize,
    key: &str,
    said: &dyn Fn(usize, &str, &str) -> String,
) -> Result<&'a str, String> {
    match value.get_ref() {
        DeValue::String(text) => Ok(text),
        _ => Err(said(at, key, "expected a string")),
    }
}

/// The built-in theme called `name`.
fn builtin(name: &str) -> Option<Theme> {
    theme::THEMES
        .into_iter()
        .find(|theme| theme.name.eq_ignore_ascii_case(name))
}

fn builtin_names() -> String {
    theme::THEMES
        .iter()
        .map(|theme| format!("`{}`", theme.name.to_lowercase()))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn themed(text: &str) -> Result<UserTheme, String> {
        theme_of(text, Path::new("/u/niobe/themes/midnight.toml"))
    }

    #[test]
    fn a_theme_is_its_base_with_the_colours_it_sets_laid_over_it() {
        let user = themed(
            "base = \"cyber\"\n[colors]\nhot = \"#ff8800\"\nfg = \"light-cyan\"\n\
             [borders]\nborder = \"rounded\"\n",
        )
        .expect("a valid theme");

        let cyber = theme::THEMES[0];
        assert_eq!(user.sixteen.name, "midnight", "named by its file");
        // A name spells the same colour at either depth.
        assert_eq!(
            user.sixteen.fg,
            niobe_tui::theme::parse_colour("light-cyan")
                .expect("a name")
                .0
        );
        assert_eq!(user.deep.fg, user.sixteen.fg);
        // A hex colour is the 24-bit table's alone: the sixteen keep the base's.
        assert_eq!(user.sixteen.hot, cyber.hot);
        assert_eq!(
            user.deep.hot,
            theme::parse_colour("#ff8800").expect("hex").0
        );
        // What the file left out is the base's.
        assert_eq!(user.sixteen.dim, cyber.dim);
        assert_eq!(user.deep.dim, cyber.deep().dim);
        assert_eq!(
            user.sixteen.border,
            theme::parse_border("rounded").expect("a line")
        );
    }

    #[test]
    fn a_colour_that_is_not_one_is_reported_at_its_key_and_its_line() {
        let said = themed("[colors]\nfg = \"white\"\nhot = \"orange\"\n")
            .expect_err("orange is not one of the sixteen");

        assert!(
            said.starts_with("/u/niobe/themes/midnight.toml:3: colors.hot: "),
            "{said}"
        );
        assert!(said.contains("#rrggbb"), "{said}");
    }

    #[test]
    fn a_key_that_is_not_a_colour_or_a_theme_key_is_reported_at_its_line() {
        let colour = themed("[colors]\nforeground = \"white\"\n").expect_err("no such colour");
        assert!(
            colour.contains(":2: colors.foreground: unknown colour"),
            "{colour}"
        );

        let key = themed("\nbasse = \"cyber\"\n").expect_err("no such key");
        assert!(key.contains(":2: basse: unknown key"), "{key}");

        let base = themed("base = \"matrix\"\n").expect_err("no such base");
        assert!(
            base.contains(":1: base: `matrix` is not a built-in theme"),
            "{base}"
        );
    }

    #[test]
    fn a_theme_that_draws_something_on_top_of_its_own_colour_is_refused() {
        let said = themed("[colors]\nfg = \"black\"\npane_bg = \"black\"\n")
            .expect_err("body text on its own colour");

        assert!(said.contains("body text"), "{said}");
        assert!(said.contains("on top of its own colour"), "{said}");
    }

    #[test]
    fn a_dialog_whose_secondary_text_is_its_own_face_is_refused() {
        let said = themed("[colors]\ndialog_bg = \"green\"\ndialog_dim = \"green\"\n")
            .expect_err("a dialog's notes on its own colour");

        assert!(said.contains("secondary text in a dialog"), "{said}");
    }

    #[test]
    fn a_24_bit_palette_that_cannot_be_read_is_refused_with_its_ratio() {
        let said = themed("[colors]\nfg = \"#202020\"\npane_bg = \"#222222\"\n")
            .expect_err("near-black on near-black");

        assert!(said.contains("to 1 against its background"), "{said}");
    }

    #[test]
    fn a_theme_may_not_take_a_built_in_ones_name() {
        let said = themed("name = \"Neo\"\n").expect_err("neo is built in");

        assert!(said.contains("built-in theme"), "{said}");
    }

    #[test]
    fn a_directory_of_files_is_read_in_name_order_and_a_bad_one_is_reported_not_skipped() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        std::fs::write(dir.path().join("b.toml"), "[colors]\nhot = \"red\"\n").expect("written");
        std::fs::write(dir.path().join("a.toml"), "[colors]\nhot = \"green\"\n").expect("written");
        std::fs::write(dir.path().join("c.toml"), "[colors]\nhot = \"nope\"\n").expect("written");
        std::fs::write(dir.path().join("notes.txt"), "not a theme").expect("written");

        let (themes, problems) = read(dir.path());

        let names: Vec<&str> = themes.iter().map(|t| t.sixteen.name).collect();
        assert_eq!(names, ["a", "b"]);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("c.toml:2: colors.hot"), "{problems:?}");
    }

    #[test]
    fn a_directory_that_is_not_there_has_no_themes_and_no_problem() {
        let (themes, problems) = read(Path::new("/nonexistent/niobe/themes"));

        assert!(themes.is_empty());
        assert!(problems.is_empty());
    }
}
