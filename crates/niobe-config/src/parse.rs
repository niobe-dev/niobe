// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! A config file's text, walked into a [`Config`].
//!
//! The document is parsed into TOML's spanned tables and walked by hand, not
//! deserialized: every key and value keeps the byte range it came from, so an
//! error can name the dotted key and the line, and the messages are written
//! for someone editing a config rather than for someone debugging a type.

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::Path;

use niobe_core::Backend;
use niobe_core::permission::{Allowlist, Rule};
use toml::Spanned;
use toml::de::{DeString, DeTable, DeValue};

use crate::{Config, ConfigError, DefaultProfile, Profile};

/// What `backend` may be, in the order an error lists them.
const BACKENDS: [Backend; 3] = [Backend::Claude, Backend::Codex, Backend::Native];

/// Parses the config file at `path`.
pub(crate) fn config(text: &str, path: &Path) -> Result<Config, ConfigError> {
    let file = File { text, path };
    let root = DeTable::parse(text).map_err(|error| file.syntax(&error))?;
    file.root(root.get_ref())
}

type Entry<'t, 'i> = (&'t Spanned<DeString<'i>>, &'t Spanned<DeValue<'i>>);

/// The file being walked, which every error is reported against.
struct File<'a> {
    text: &'a str,
    path: &'a Path,
}

impl File<'_> {
    fn root(&self, root: &DeTable<'_>) -> Result<Config, ConfigError> {
        let mut config = Config::default();
        for (key, value) in in_file_order(root) {
            let at = Key::root(key.get_ref());
            match key.get_ref().as_ref() {
                "default_profile" => {
                    let name = self.non_empty_string(value, &at)?;
                    config.default_profile = Some(DefaultProfile {
                        name: name.to_owned(),
                        path: self.path.to_path_buf(),
                        line: self.line(&value.span()),
                    });
                }
                "permissions" => config.allowed = self.permissions(value, &at)?,
                "profiles" => {
                    for (name, profile) in in_file_order(self.table(value, &at)?) {
                        let at = at.child(name.get_ref());
                        if name.get_ref().is_empty() {
                            return Err(self.invalid(
                                &name.span(),
                                &at,
                                "a profile name cannot be empty",
                            ));
                        }
                        let profile = self.profile(name, profile, &at)?;
                        config.profiles.insert(name.get_ref().to_string(), profile);
                    }
                }
                _ => {
                    return Err(self.invalid(
                        &key.span(),
                        &at,
                        "unknown key; expected `default_profile`, `profiles` or `permissions`",
                    ));
                }
            }
        }
        Ok(config)
    }

    /// The standing answers to permission prompts, as the shell will match
    /// them.
    ///
    /// A rule that is not one is reported at its line: a config that silently
    /// dropped one would leave the operator expecting a prompt not to come
    /// back when it will.
    fn permissions(
        &self,
        value: &Spanned<DeValue<'_>>,
        at: &Key,
    ) -> Result<Allowlist, ConfigError> {
        let mut allowed = Allowlist::new();
        for (key, value) in in_file_order(self.table(value, at)?) {
            let at = at.child(key.get_ref());
            match key.get_ref().as_ref() {
                "allow" => {
                    let DeValue::Array(items) = value.get_ref() else {
                        return Err(self.wrong_type(value, &at, "an array of rules"));
                    };
                    for (index, item) in items.iter().enumerate() {
                        let at = at.index(index);
                        let text = self.string(item, &at)?;
                        let rule = Rule::parse(text)
                            .map_err(|error| self.invalid(&item.span(), &at, &error.to_string()))?;
                        allowed.insert(rule);
                    }
                }
                _ => {
                    return Err(self.invalid(&key.span(), &at, "unknown key; expected `allow`"));
                }
            }
        }
        Ok(allowed)
    }

    fn profile(
        &self,
        name: &Spanned<DeString<'_>>,
        value: &Spanned<DeValue<'_>>,
        at: &Key,
    ) -> Result<Profile, ConfigError> {
        let mut backend = None;
        let mut env = BTreeMap::new();
        let mut args = Vec::new();
        let mut models = Vec::new();
        let mut auth_refresh = None;

        for (key, value) in in_file_order(self.table(value, at)?) {
            let at = at.child(key.get_ref());
            match key.get_ref().as_ref() {
                "backend" => backend = Some(self.backend(value, &at)?),
                "env" => env = self.env(value, &at)?,
                "args" => args = self.strings(value, &at)?,
                "models" => models = self.non_empty_strings(value, &at)?,
                "auth_refresh" => {
                    auth_refresh = Some(self.non_empty_string(value, &at)?.to_owned());
                }
                _ => {
                    return Err(self.invalid(
                        &key.span(),
                        &at,
                        "unknown key; expected `backend`, `env`, `args`, `models` or \
                         `auth_refresh`",
                    ));
                }
            }
        }

        let backend = backend.ok_or_else(|| {
            self.invalid(
                &name.span(),
                at,
                &format!("no `backend`; expected {}", backend_list()),
            )
        })?;
        Ok(Profile {
            backend,
            env,
            args,
            models,
            auth_refresh,
            source: self.path.to_path_buf(),
            withheld: None,
        })
    }

    fn backend(&self, value: &Spanned<DeValue<'_>>, at: &Key) -> Result<Backend, ConfigError> {
        let name = self.string(value, at)?;
        BACKENDS
            .into_iter()
            .find(|backend| backend.as_str() == name)
            .ok_or_else(|| {
                self.invalid(
                    &value.span(),
                    at,
                    &format!("`{name}` is not a backend; expected {}", backend_list()),
                )
            })
    }

    /// The variables of a profile. Each must be settable in a child process's
    /// environment, which is checked here so that a bad name fails at its line
    /// in the config rather than when the backend is spawned.
    fn env(
        &self,
        value: &Spanned<DeValue<'_>>,
        at: &Key,
    ) -> Result<BTreeMap<String, String>, ConfigError> {
        let mut env = BTreeMap::new();
        for (name, value) in in_file_order(self.table(value, at)?) {
            let at = at.child(name.get_ref());
            let problem = if name.get_ref().is_empty() {
                Some("a variable name cannot be empty")
            } else if name.get_ref().contains('=') {
                Some("a variable name cannot contain `=`")
            } else if name.get_ref().contains('\0') {
                Some("a variable name cannot contain a NUL character")
            } else {
                None
            };
            if let Some(problem) = problem {
                return Err(self.invalid(&name.span(), &at, problem));
            }

            let text = self.string(value, &at)?;
            if text.contains('\0') {
                return Err(self.invalid(
                    &value.span(),
                    &at,
                    "a value cannot contain a NUL character",
                ));
            }
            env.insert(name.get_ref().to_string(), text.to_owned());
        }
        Ok(env)
    }

    fn strings(&self, value: &Spanned<DeValue<'_>>, at: &Key) -> Result<Vec<String>, ConfigError> {
        let DeValue::Array(items) = value.get_ref() else {
            return Err(self.wrong_type(value, at, "an array of strings"));
        };
        items
            .iter()
            .enumerate()
            .map(|(index, item)| self.string(item, &at.index(index)).map(str::to_owned))
            .collect()
    }

    /// An array of strings, each of which has to say something: a model named
    /// as blank space is a mistake in the file, not a model.
    fn non_empty_strings(
        &self,
        value: &Spanned<DeValue<'_>>,
        at: &Key,
    ) -> Result<Vec<String>, ConfigError> {
        let DeValue::Array(items) = value.get_ref() else {
            return Err(self.wrong_type(value, at, "an array of strings"));
        };
        items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                self.non_empty_string(item, &at.index(index))
                    .map(str::to_owned)
            })
            .collect()
    }

    fn table<'t, 'i>(
        &self,
        value: &'t Spanned<DeValue<'i>>,
        at: &Key,
    ) -> Result<&'t DeTable<'i>, ConfigError> {
        match value.get_ref() {
            DeValue::Table(table) => Ok(table),
            _ => Err(self.wrong_type(value, at, "a table")),
        }
    }

    fn string<'t>(
        &self,
        value: &'t Spanned<DeValue<'_>>,
        at: &Key,
    ) -> Result<&'t str, ConfigError> {
        match value.get_ref() {
            DeValue::String(text) => Ok(text),
            _ => Err(self.wrong_type(value, at, "a string")),
        }
    }

    /// A string that says something. Whitespace alone is empty: a command or a
    /// name of only spaces is a mistake, not a choice.
    fn non_empty_string<'t>(
        &self,
        value: &'t Spanned<DeValue<'_>>,
        at: &Key,
    ) -> Result<&'t str, ConfigError> {
        let text = self.string(value, at)?;
        if text.trim().is_empty() {
            return Err(self.invalid(&value.span(), at, "is empty"));
        }
        Ok(text)
    }

    fn wrong_type(&self, value: &Spanned<DeValue<'_>>, at: &Key, expected: &str) -> ConfigError {
        self.invalid(
            &value.span(),
            at,
            &format!("expected {expected}, found {}", kind(value.get_ref())),
        )
    }

    fn invalid(&self, span: &Range<usize>, at: &Key, message: &str) -> ConfigError {
        ConfigError::Invalid {
            path: self.path.to_path_buf(),
            line: self.line(span),
            key: Some(at.0.clone()),
            message: message.to_owned(),
        }
    }

    /// A document that is not TOML. The parser's message is kept, since it
    /// says what it expected at that point, and so is the text it stopped at
    /// when that is short enough to quote — for a duplicate key it is the key.
    /// The parser's rendering of the source around the error is not kept: the
    /// line number already points there.
    fn syntax(&self, error: &toml::de::Error) -> ConfigError {
        let span = error.span();
        let mut message = error.message().trim().to_owned();
        if let Some(quoted) = span.as_ref().and_then(|span| self.quotable(span)) {
            message = format!("{message}: `{quoted}`");
        }
        ConfigError::Invalid {
            path: self.path.to_path_buf(),
            line: span.map_or(1, |span| self.line(&span)),
            key: None,
            message,
        }
    }

    /// The text of a span, if it is on one line and short enough to put in a
    /// message.
    fn quotable(&self, span: &Range<usize>) -> Option<&str> {
        const LONGEST: usize = 40;
        let text = self.text.get(span.clone())?.trim();
        let fits = !text.is_empty() && text.len() <= LONGEST && !text.contains('\n');
        fits.then_some(text)
    }

    /// The line, counted from 1, that a span starts on.
    fn line(&self, span: &Range<usize>) -> usize {
        let start = span.start.min(self.text.len());
        self.text.as_bytes()[..start]
            .iter()
            .filter(|&&byte| byte == b'\n')
            .count()
            + 1
    }
}

/// The dotted path of a key, spelt the way it would be written in TOML.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Key(String);

impl Key {
    fn root(name: &str) -> Self {
        Self(quoted(name))
    }

    fn child(&self, name: &str) -> Self {
        Self(format!("{}.{}", self.0, quoted(name)))
    }

    fn index(&self, index: usize) -> Self {
        Self(format!("{}[{index}]", self.0))
    }
}

/// A key as TOML would need it written: bare when it can be, quoted when not.
fn quoted(name: &str) -> String {
    let bare = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if bare {
        return name.to_owned();
    }
    let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// A table's entries in the order they appear in the file. The parsed table is
/// sorted by key, and the first error reported should be the first one an
/// operator reading the file from the top would reach.
fn in_file_order<'t, 'i>(table: &'t DeTable<'i>) -> Vec<Entry<'t, 'i>> {
    let mut entries: Vec<_> = table.iter().collect();
    entries.sort_by_key(|(key, _)| key.span().start);
    entries
}

fn backend_list() -> String {
    let names: Vec<String> = BACKENDS
        .iter()
        .map(|backend| format!("`{backend}`"))
        .collect();
    match names.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
        None => String::new(),
    }
}

/// What a value is, as an error names it.
fn kind(value: &DeValue<'_>) -> &'static str {
    match value {
        DeValue::String(_) => "a string",
        DeValue::Integer(_) => "an integer",
        DeValue::Float(_) => "a float",
        DeValue::Boolean(_) => "a boolean",
        DeValue::Datetime(_) => "a date-time",
        DeValue::Array(_) => "an array",
        DeValue::Table(_) => "a table",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_quoted_only_when_it_could_not_be_written_bare() {
        assert_eq!(quoted("AWS_PROFILE"), "AWS_PROFILE");
        assert_eq!(quoted("my-work"), "my-work");
        assert_eq!(quoted("a.b"), "\"a.b\"");
        assert_eq!(quoted(""), "\"\"");
        assert_eq!(quoted("say \"hi\""), "\"say \\\"hi\\\"\"");
    }

    #[test]
    fn a_syntax_error_quotes_the_text_it_stopped_at_when_that_is_short() {
        let error = config(
            "[profiles.work]\nbackend = \"claude\"\nbackend = \"codex\"\n",
            Path::new("c.toml"),
        )
        .expect_err("a duplicate key");
        assert_eq!(error.to_string(), "c.toml:3: duplicate key: `backend`");
    }

    #[test]
    fn lines_count_from_one() {
        let file = File {
            text: "a\nb\n\nc",
            path: Path::new("x"),
        };
        assert_eq!(file.line(&(0..1)), 1);
        assert_eq!(file.line(&(2..3)), 2);
        assert_eq!(file.line(&(5..6)), 4);
        assert_eq!(file.line(&(99..100)), 4);
    }

    #[test]
    fn the_backends_are_listed_the_way_a_sentence_lists_them() {
        assert_eq!(backend_list(), "`claude`, `codex` or `native`");
    }
}
