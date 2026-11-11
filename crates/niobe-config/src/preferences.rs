// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The choices the operator made in the shell — the theme, and each
//! profile's model and effort — kept for the next session to open with.
//!
//! They are kept in a file of Niobe's own beside the user's config,
//! [`FILE_NAME`], rather than spliced into the config: the config is the
//! operator's, written by hand and possibly a repository's, and a profile the
//! choice belongs to may be defined in a file Niobe must not write. This file
//! is rewritten whole on every choice, so nothing the operator writes into it
//! is kept but the values it reads.
//!
//! What it holds is a convenience, never a reason a session cannot open: a
//! file that cannot be read, or holds something other than what this module
//! writes, reads as no choices made, and the next choice writes it afresh.

use std::collections::BTreeMap;
use std::path::Path;

use toml::de::{DeTable, DeValue};

use crate::ConfigError;

/// The file the choices are kept in, in the user's config directory.
pub const FILE_NAME: &str = "choices.toml";

/// What the file says first, so that an operator who opens it knows who
/// writes it and that an edit lasts only until the next choice.
const HEADER: &str = "# Written by niobe: the theme, and each profile's model and effort, last chosen\n\
                      # in the shell, which the next session opens with. Rewritten whole on each\n\
                      # choice; `--theme`, and a choice made in the shell, override it.\n";

/// The choices the last sessions made, as the file holds them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Saved {
    theme: Option<String>,
    profiles: BTreeMap<String, ProfileChoices>,
}

/// What was chosen for one profile.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ProfileChoices {
    model: Option<String>,
    effort: Option<String>,
}

impl ProfileChoices {
    fn is_empty(&self) -> bool {
        self.model.is_none() && self.effort.is_none()
    }
}

/// One choice the operator made, to be kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change<'a> {
    /// The palette the shell draws in, for every profile.
    Theme(&'a str),
    /// The model `profile` answers with; `None` forgets the one kept, as
    /// where the backend refused it.
    Model {
        /// The profile it was chosen under.
        profile: &'a str,
        /// The model, as the operator named it.
        model: Option<&'a str>,
    },
    /// How hard `profile`'s model works on a turn.
    Effort {
        /// The profile it was chosen under.
        profile: &'a str,
        /// The level, as the backend takes it.
        effort: &'a str,
    },
}

impl Saved {
    /// What the file at `path` holds; nothing where there is no file, or one
    /// that cannot be read or is not this module's.
    pub fn read(path: &Path) -> Self {
        crate::read::text(path)
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    /// What `text` holds, each value that is not what this module writes
    /// passed over rather than refused.
    pub fn parse(text: &str) -> Self {
        let Ok(root) = DeTable::parse(text) else {
            return Self::default();
        };
        let root = root.get_ref();
        let mut saved = Self {
            theme: root.get("theme").and_then(|value| said(value.get_ref())),
            profiles: BTreeMap::new(),
        };
        let Some(DeValue::Table(profiles)) = root.get("profiles").map(|value| value.get_ref())
        else {
            return saved;
        };
        for (name, value) in profiles {
            let DeValue::Table(table) = value.get_ref() else {
                continue;
            };
            let choices = ProfileChoices {
                model: table.get("model").and_then(|value| said(value.get_ref())),
                effort: table.get("effort").and_then(|value| said(value.get_ref())),
            };
            if !choices.is_empty() {
                saved.profiles.insert(name.get_ref().to_string(), choices);
            }
        }
        saved
    }

    /// The theme last chosen.
    pub fn theme(&self) -> Option<&str> {
        self.theme.as_deref()
    }

    /// The model last chosen under `profile`.
    pub fn model(&self, profile: &str) -> Option<&str> {
        self.profiles.get(profile)?.model.as_deref()
    }

    /// The effort last chosen under `profile`.
    pub fn effort(&self, profile: &str) -> Option<&str> {
        self.profiles.get(profile)?.effort.as_deref()
    }

    /// These choices with `change` made.
    fn changed(mut self, change: Change<'_>) -> Self {
        match change {
            Change::Theme(theme) => self.theme = Some(theme.to_owned()),
            Change::Model { profile, model } => {
                let choices = self.profiles.entry(profile.to_owned()).or_default();
                choices.model = model.map(str::to_owned);
            }
            Change::Effort { profile, effort } => {
                let choices = self.profiles.entry(profile.to_owned()).or_default();
                choices.effort = Some(effort.to_owned());
            }
        }
        self.profiles.retain(|_, choices| !choices.is_empty());
        self
    }

    /// The file's text.
    fn text(&self) -> String {
        let mut text = HEADER.to_owned();
        if let Some(theme) = &self.theme {
            text.push_str(&format!("\ntheme = {}\n", crate::write::quoted(theme)));
        }
        for (profile, choices) in &self.profiles {
            text.push_str(&format!("\n[profiles.{}]\n", crate::parse::quoted(profile)));
            for (key, value) in [("model", &choices.model), ("effort", &choices.effort)] {
                if let Some(value) = value {
                    text.push_str(&format!("{key} = {}\n", crate::write::quoted(value)));
                }
            }
        }
        text
    }
}

/// A string that says something; a blank one is no choice.
fn said(value: &DeValue<'_>) -> Option<String> {
    match value {
        DeValue::String(text) if !text.trim().is_empty() => Some(text.to_string()),
        _ => None,
    }
}

/// Keeps `change` in the file at `path`, every other choice there as it was.
///
/// Read, changed and replaced whole under the lock a config's rule is written
/// under, so two sessions choosing at once each keep what the other chose
/// rather than what both read; a write that fails leaves the file as it was.
/// A file a link names is written where it lives.
pub fn keep(path: &Path, change: Change<'_>) -> Result<(), ConfigError> {
    let failed = |error| ConfigError::Write {
        path: path.to_path_buf(),
        error,
    };
    let target = crate::replace::resolved(path).map_err(failed)?;
    let dir = crate::replace::parent(&target);
    std::fs::create_dir_all(dir).map_err(failed)?;
    let _lock = crate::replace::Lock::directory(dir).map_err(failed)?;
    let text = Saved::read(&target).changed(change).text();
    crate::replace::replace(&target, text.as_bytes()).map_err(failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(dir: &Path) -> std::path::PathBuf {
        dir.join("niobe").join(FILE_NAME)
    }

    #[test]
    fn a_theme_kept_where_there_was_no_file_is_read_back() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let path = kept(dir.path());

        keep(&path, Change::Theme("neo")).expect("the choice is kept");

        assert_eq!(Saved::read(&path).theme(), Some("neo"));
    }

    #[test]
    fn a_model_and_an_effort_are_kept_for_their_profile_and_nothing_else_moves() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let path = kept(dir.path());
        keep(&path, Change::Theme("classic")).expect("kept");
        keep(
            &path,
            Change::Model {
                profile: "work",
                model: Some("sonnet"),
            },
        )
        .expect("kept");

        keep(
            &path,
            Change::Model {
                profile: "max",
                model: Some("opus"),
            },
        )
        .expect("kept");
        keep(
            &path,
            Change::Effort {
                profile: "max",
                effort: "high",
            },
        )
        .expect("kept");

        let saved = Saved::read(&path);
        assert_eq!(saved.theme(), Some("classic"));
        assert_eq!(saved.model("max"), Some("opus"));
        assert_eq!(saved.effort("max"), Some("high"));
        assert_eq!(saved.model("work"), Some("sonnet"));
        assert_eq!(saved.effort("work"), None);
        assert_eq!(saved.model("other"), None);
    }

    #[test]
    fn the_last_choice_of_a_kind_is_the_one_kept_and_keeping_it_again_changes_nothing() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let path = kept(dir.path());
        keep(&path, Change::Theme("neo")).expect("kept");
        keep(&path, Change::Theme("classic")).expect("kept");
        let once = std::fs::read_to_string(&path).expect("the file was written");

        keep(&path, Change::Theme("classic")).expect("kept");

        assert_eq!(Saved::read(&path).theme(), Some("classic"));
        assert_eq!(
            std::fs::read_to_string(&path).expect("the file was written"),
            once
        );
    }

    #[test]
    fn a_model_forgotten_leaves_the_profile_s_effort_and_a_profile_with_nothing_goes() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let path = kept(dir.path());
        for change in [
            Change::Model {
                profile: "max",
                model: Some("opus"),
            },
            Change::Effort {
                profile: "max",
                effort: "low",
            },
            Change::Model {
                profile: "work",
                model: Some("haiku"),
            },
        ] {
            keep(&path, change).expect("kept");
        }

        for profile in ["max", "work"] {
            keep(
                &path,
                Change::Model {
                    profile,
                    model: None,
                },
            )
            .expect("kept");
        }

        let saved = Saved::read(&path);
        assert_eq!(saved.model("max"), None);
        assert_eq!(saved.effort("max"), Some("low"));
        let text = std::fs::read_to_string(&path).expect("the file was written");
        assert!(!text.contains("work"), "{text}");
    }

    #[test]
    fn names_and_values_toml_would_misread_are_written_so_they_read_back_as_chosen() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let path = kept(dir.path());
        let profile = "my \"work\" profile.x";
        let model = "odd\\model\"\nwith a break\u{7}";

        keep(
            &path,
            Change::Model {
                profile,
                model: Some(model),
            },
        )
        .expect("kept");

        assert_eq!(Saved::read(&path).model(profile), Some(model));
    }

    #[test]
    fn a_file_that_is_not_this_module_s_reads_as_nothing_chosen_and_the_next_choice_replaces_it() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let path = kept(dir.path());
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("made");
        std::fs::write(&path, "theme = [\nnot toml").expect("written");
        assert_eq!(Saved::read(&path), Saved::default());

        keep(&path, Change::Theme("neo")).expect("kept");

        assert_eq!(Saved::read(&path).theme(), Some("neo"));
    }

    #[test]
    fn values_of_the_wrong_kind_and_keys_it_does_not_write_are_passed_over() {
        let saved = Saved::parse(
            "theme = 3\nother = \"x\"\n[profiles]\nbare = \"x\"\n[profiles.max]\n\
             model = \"  \"\neffort = \"high\"\n[profiles.work]\nmodel = [\"opus\"]\n",
        );

        assert_eq!(saved.theme(), None);
        assert_eq!(saved.model("max"), None);
        assert_eq!(saved.effort("max"), Some("high"));
        assert_eq!(saved.model("work"), None);
        assert_eq!(saved.model("bare"), None);
    }

    #[test]
    fn a_file_that_is_not_a_regular_file_reads_as_nothing_chosen() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let path = kept(dir.path());
        std::fs::create_dir_all(&path).expect("a directory where the file would be");

        assert_eq!(Saved::read(&path), Saved::default());
        assert!(keep(&path, Change::Theme("neo")).is_err());
    }
}
