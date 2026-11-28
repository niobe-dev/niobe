// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The choices the operator made in the shell — the theme, in each project
//! and the last anywhere, the mode each project's tool calls are gated in,
//! and each profile's model and effort — kept for the next session to open
//! with.
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

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use niobe_core::event::Mode;
use toml::de::{DeTable, DeValue};

use crate::ConfigError;

/// The file the choices are kept in, in the user's config directory.
pub const FILE_NAME: &str = "choices.toml";

/// What the file says first, so that an operator who opens it knows who
/// writes it and that an edit lasts only until the next choice.
const HEADER: &str = "# Written by niobe: the theme last chosen in the shell, anywhere and in each\n\
                      # project, the mode each project's tool calls were last gated in, and each\n\
                      # profile's model and effort, which the next session opens with. Rewritten\n\
                      # whole on each choice; `--theme`, and a choice made in the shell, override it.\n";

/// The choices the last sessions made, as the file holds them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Saved {
    /// The theme last chosen, in whichever project it was.
    theme: Option<String>,
    /// What was last chosen in each project, the project chosen in last
    /// first, so that what the file has no room for is the oldest.
    projects: Vec<ProjectChoices>,
    profiles: BTreeMap<String, ProfileChoices>,
}

/// What was last chosen in one project: at least one of a theme and a mode.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectChoices {
    /// The project's root, as the session that chose it was opened at.
    root: String,
    theme: Option<String>,
    mode: Option<Mode>,
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
    /// The palette the shell draws in, for every profile: the one the
    /// project rooted at `project` opens in from now on, and the one a
    /// project with no choice of its own does.
    Theme {
        /// The root of the project it was chosen in; `None` where the session
        /// ran in none, and then it is only the last chosen. A root that is
        /// not UTF-8 is kept as none, since the file is text.
        project: Option<&'a Path>,
        /// The theme, by its name.
        theme: &'a str,
    },
    /// The mode the project rooted at `project` gates tool calls in from
    /// now on. Kept for that project alone, and not at all where its root is
    /// not UTF-8, since the file is text.
    Mode {
        /// The root of the project it was chosen in.
        project: &'a Path,
        /// The mode.
        mode: Mode,
    },
    /// The model `profile` answers with; `None` forgets the one kept, as
    /// where the backend refused it.
    Model {
        /// The profile it was chosen under.
        profile: &'a str,
        /// The model, as the operator named it.
        model: Option<&'a str>,
    },
    /// How hard `profile`'s model works on a turn; `None` forgets the one
    /// kept, as where the operator handed the level to the model's default.
    Effort {
        /// The profile it was chosen under.
        profile: &'a str,
        /// The level, as the backend takes it.
        effort: Option<&'a str>,
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
            projects: root
                .get("projects")
                .map(|value| projects(value.get_ref()))
                .unwrap_or_default(),
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

    /// The theme last chosen, in whichever project it was.
    pub fn last_theme(&self) -> Option<&str> {
        self.theme.as_deref()
    }

    /// The theme last chosen in the project rooted at `root`; none where
    /// none was chosen there, or none the file still has room for.
    pub fn project_theme(&self, root: &Path) -> Option<&str> {
        self.project(root)?.theme.as_deref()
    }

    /// The mode last chosen in the project rooted at `root`; none where
    /// none was chosen there, or none the file still has room for.
    pub fn project_mode(&self, root: &Path) -> Option<Mode> {
        self.project(root)?.mode
    }

    /// What was last chosen in the project rooted at `root`.
    fn project(&self, root: &Path) -> Option<&ProjectChoices> {
        let root = root.to_str()?;
        self.projects.iter().find(|project| project.root == root)
    }

    /// The choices of the project rooted at `root`, moved to the front as
    /// the one chosen in last, and made where it had none.
    fn chosen_in(&mut self, root: &str) -> &mut ProjectChoices {
        let earlier = self
            .projects
            .iter()
            .position(|project| project.root == root)
            .map(|at| self.projects.remove(at));
        self.projects.insert(
            0,
            earlier.unwrap_or_else(|| ProjectChoices {
                root: root.to_owned(),
                theme: None,
                mode: None,
            }),
        );
        &mut self.projects[0]
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
            Change::Theme { project, theme } => {
                if let Some(root) = project.and_then(Path::to_str) {
                    self.chosen_in(root).theme = Some(theme.to_owned());
                }
                self.theme = Some(theme.to_owned());
            }
            Change::Mode { project, mode } => {
                if let Some(root) = project.to_str() {
                    self.chosen_in(root).mode = Some(mode);
                }
            }
            Change::Model { profile, model } => {
                let choices = self.profiles.entry(profile.to_owned()).or_default();
                choices.model = model.map(str::to_owned);
            }
            Change::Effort { profile, effort } => {
                let choices = self.profiles.entry(profile.to_owned()).or_default();
                choices.effort = effort.map(str::to_owned);
            }
        }
        self.profiles.retain(|_, choices| !choices.is_empty());
        self
    }

    /// The file's text: everything chosen, and as many projects, newest
    /// first, as keep it within what [`Saved::read`] reads back — past that
    /// the whole file would read as nothing chosen.
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
        let room = usize::try_from(crate::read::LIMIT).unwrap_or(usize::MAX);
        for project in &self.projects {
            let mut entry = format!(
                "\n[[projects]]\nroot = {}\n",
                crate::write::quoted(&project.root)
            );
            if let Some(theme) = &project.theme {
                entry.push_str(&format!("theme = {}\n", crate::write::quoted(theme)));
            }
            if let Some(mode) = project.mode {
                entry.push_str(&format!("mode = {}\n", crate::write::quoted(mode.as_str())));
            }
            if text.len().saturating_add(entry.len()) > room {
                break;
            }
            text.push_str(&entry);
        }
        text
    }
}

/// The projects a `projects` array holds, in its order, each table that is
/// not a root and a theme or a mode passed over, and so is a root listed
/// again: the first listing is the newer.
fn projects(value: &DeValue<'_>) -> Vec<ProjectChoices> {
    let DeValue::Array(entries) = value else {
        return Vec::new();
    };
    // A set rather than a search of what is kept: the file may hold tens of
    // thousands of entries, and it is read before the shell opens.
    let mut seen = BTreeSet::new();
    let mut projects = Vec::new();
    for entry in entries {
        let DeValue::Table(table) = entry.get_ref() else {
            continue;
        };
        let read = |key: &str| table.get(key).and_then(|value| said(value.get_ref()));
        let Some(root) = read("root") else {
            continue;
        };
        let theme = read("theme");
        let mode = read("mode").and_then(|mode| named(&mode));
        if (theme.is_some() || mode.is_some()) && seen.insert(root.clone()) {
            projects.push(ProjectChoices { root, theme, mode });
        }
    }
    projects
}

/// The mode the shell calls `name`; none for a name it has no mode for,
/// such as one of the backend's own that the shell does not model.
fn named(name: &str) -> Option<Mode> {
    [Mode::Plan, Mode::Ask, Mode::Auto]
        .into_iter()
        .find(|mode| mode.as_str() == name)
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
    use niobe_core::event::Mode;

    fn kept(dir: &Path) -> std::path::PathBuf {
        dir.join("niobe").join(FILE_NAME)
    }

    /// A theme chosen in no project in particular.
    fn theme(theme: &str) -> Change<'_> {
        Change::Theme {
            project: None,
            theme,
        }
    }

    /// A theme chosen in the project rooted at `root`.
    fn theme_in<'a>(root: &'a Path, theme: &'a str) -> Change<'a> {
        Change::Theme {
            project: Some(root),
            theme,
        }
    }

    #[test]
    fn a_theme_kept_where_there_was_no_file_is_read_back() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let path = kept(dir.path());

        keep(&path, theme("neo")).expect("the choice is kept");

        assert_eq!(Saved::read(&path).last_theme(), Some("neo"));
    }

    #[test]
    fn a_theme_chosen_in_a_project_is_that_project_s_and_the_last_chosen_anywhere() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let path = kept(dir.path());
        let (a, b, c) = (
            Path::new("/work/a"),
            Path::new("/work/b"),
            Path::new("/work/c"),
        );

        keep(&path, theme_in(a, "neo")).expect("kept");
        keep(&path, theme_in(b, "classic")).expect("kept");

        let saved = Saved::read(&path);
        assert_eq!(saved.project_theme(a), Some("neo"));
        assert_eq!(saved.project_theme(b), Some("classic"));
        assert_eq!(saved.project_theme(c), None);
        assert_eq!(saved.last_theme(), Some("classic"));
    }

    #[test]
    fn a_project_chosen_in_again_keeps_one_entry_with_the_newer_theme() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let path = kept(dir.path());
        let (a, b) = (Path::new("/work/a"), Path::new("/work/b"));
        keep(&path, theme_in(a, "neo")).expect("kept");
        keep(&path, theme_in(b, "classic")).expect("kept");

        keep(&path, theme_in(a, "modern")).expect("kept");
        let once = std::fs::read_to_string(&path).expect("the file was written");
        keep(&path, theme_in(a, "modern")).expect("kept");

        let saved = Saved::read(&path);
        assert_eq!(saved.project_theme(a), Some("modern"));
        assert_eq!(saved.project_theme(b), Some("classic"));
        assert_eq!(saved.last_theme(), Some("modern"));
        let text = std::fs::read_to_string(&path).expect("the file was written");
        assert_eq!(text.matches("/work/a").count(), 1, "{text}");
        assert_eq!(text, once);
    }

    #[test]
    fn a_theme_chosen_in_no_project_moves_the_last_chosen_and_leaves_each_project_s() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let path = kept(dir.path());
        let a = Path::new("/work/a");
        keep(&path, theme_in(a, "neo")).expect("kept");

        keep(&path, theme("classic")).expect("kept");

        let saved = Saved::read(&path);
        assert_eq!(saved.project_theme(a), Some("neo"));
        assert_eq!(saved.last_theme(), Some("classic"));
    }

    #[test]
    fn projects_past_what_the_file_is_read_back_at_are_forgotten_oldest_first() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let path = kept(dir.path());
        let deep = "d".repeat(100_000);
        let roots: Vec<std::path::PathBuf> = (0..15)
            .map(|n| Path::new("/").join(&deep).join(n.to_string()))
            .collect();

        for root in &roots {
            keep(&path, theme_in(root, "neo")).expect("kept");
        }

        let length = std::fs::metadata(&path).expect("written").len();
        assert!(length <= crate::read::LIMIT, "{length} bytes");
        let saved = Saved::read(&path);
        assert_eq!(saved.project_theme(&roots[14]), Some("neo"));
        assert_eq!(saved.project_theme(&roots[0]), None);
        assert_eq!(saved.last_theme(), Some("neo"));
    }

    #[test]
    fn a_file_written_before_themes_were_kept_per_project_reads_its_theme_as_the_last_chosen() {
        let saved = Saved::parse("theme = \"neo\"\n");

        assert_eq!(saved.last_theme(), Some("neo"));
        assert_eq!(saved.project_theme(Path::new("/work/a")), None);
    }

    #[test]
    fn project_entries_toml_would_misread_or_this_module_does_not_write_are_passed_over() {
        let saved = Saved::parse("projects = 3\n");
        assert_eq!(saved.project_theme(Path::new("/a")), None);

        let saved = Saved::parse(
            "[[projects]]\nroot = \"/a\"\ntheme = \"neo\"\n\
             [[projects]]\nroot = \"/a\"\ntheme = \"classic\"\n\
             [[projects]]\nroot = \"/b\"\ntheme = 3\n\
             [[projects]]\ntheme = \"modern\"\n\
             [[projects]]\nroot = \"  \"\ntheme = \"modern\"\n",
        );
        assert_eq!(saved.project_theme(Path::new("/a")), Some("neo"));
        assert_eq!(saved.project_theme(Path::new("/b")), None);
        assert_eq!(saved.last_theme(), None);
    }

    #[test]
    fn a_root_with_quotes_and_breaks_in_it_reads_back_as_the_root_it_was() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let path = kept(dir.path());
        let root = Path::new("/work/my \"odd\"\nproject\\x");

        keep(&path, theme_in(root, "neo")).expect("kept");

        assert_eq!(Saved::read(&path).project_theme(root), Some("neo"));
    }

    #[cfg(unix)]
    #[test]
    fn a_root_that_is_not_utf_8_keeps_only_the_last_chosen() {
        use std::os::unix::ffi::OsStrExt as _;
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let path = kept(dir.path());
        let root = Path::new(std::ffi::OsStr::from_bytes(b"/work/\xff"));

        keep(&path, theme_in(root, "neo")).expect("kept");

        let saved = Saved::read(&path);
        assert_eq!(saved.project_theme(root), None);
        assert_eq!(saved.last_theme(), Some("neo"));
    }

    /// A mode chosen in the project rooted at `root`.
    fn mode_in(root: &Path, mode: Mode) -> Change<'_> {
        Change::Mode {
            project: root,
            mode,
        }
    }

    #[test]
    fn a_mode_chosen_in_a_project_is_that_project_s_alone() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let path = kept(dir.path());
        let (a, b, c) = (
            Path::new("/work/a"),
            Path::new("/work/b"),
            Path::new("/work/c"),
        );

        keep(&path, mode_in(a, Mode::Ask)).expect("kept");
        keep(&path, mode_in(b, Mode::Auto)).expect("kept");

        let saved = Saved::read(&path);
        assert_eq!(saved.project_mode(a), Some(Mode::Ask));
        assert_eq!(saved.project_mode(b), Some(Mode::Auto));
        assert_eq!(saved.project_mode(c), None);
        assert_eq!(saved.last_theme(), None, "a mode is no theme");
    }

    #[test]
    fn a_project_s_mode_and_theme_are_kept_together_and_each_choice_leaves_the_other() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let path = kept(dir.path());
        let (a, b) = (Path::new("/work/a"), Path::new("/work/b"));
        keep(&path, theme_in(a, "neo")).expect("kept");
        keep(&path, theme_in(b, "classic")).expect("kept");

        keep(&path, mode_in(a, Mode::Ask)).expect("kept");
        keep(&path, theme_in(a, "modern")).expect("kept");
        keep(&path, mode_in(a, Mode::Auto)).expect("kept");
        let once = std::fs::read_to_string(&path).expect("the file was written");
        keep(&path, mode_in(a, Mode::Auto)).expect("kept");

        let saved = Saved::read(&path);
        assert_eq!(saved.project_mode(a), Some(Mode::Auto));
        assert_eq!(saved.project_theme(a), Some("modern"));
        assert_eq!(saved.project_theme(b), Some("classic"));
        assert_eq!(saved.project_mode(b), None);
        assert_eq!(saved.last_theme(), Some("modern"));
        let text = std::fs::read_to_string(&path).expect("the file was written");
        assert_eq!(text.matches("/work/a").count(), 1, "{text}");
        assert_eq!(text, once, "keeping the same mode again changes nothing");
    }

    #[test]
    fn a_project_chosen_a_mode_in_last_is_the_last_forgotten() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let path = kept(dir.path());
        let deep = "d".repeat(100_000);
        let roots: Vec<std::path::PathBuf> = (0..15)
            .map(|n| Path::new("/").join(&deep).join(n.to_string()))
            .collect();
        for root in &roots {
            keep(&path, theme_in(root, "neo")).expect("kept");
        }

        keep(&path, mode_in(&roots[0], Mode::Ask)).expect("kept");

        let saved = Saved::read(&path);
        assert_eq!(saved.project_mode(&roots[0]), Some(Mode::Ask));
        assert_eq!(
            saved.project_theme(&roots[0]),
            None,
            "its theme was forgotten"
        );
        assert_eq!(saved.project_theme(&roots[1]), None);
        assert_eq!(saved.project_theme(&roots[14]), Some("neo"));
    }

    #[cfg(unix)]
    #[test]
    fn a_mode_chosen_at_a_root_that_is_not_utf_8_is_not_kept() {
        use std::os::unix::ffi::OsStrExt as _;
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let path = kept(dir.path());
        let root = Path::new(std::ffi::OsStr::from_bytes(b"/work/\xff"));

        keep(&path, mode_in(root, Mode::Ask)).expect("kept");

        assert_eq!(Saved::read(&path).project_mode(root), None);
    }

    #[test]
    fn a_mode_the_shell_has_no_name_for_is_passed_over_and_the_entry_s_theme_kept() {
        let saved = Saved::parse(
            "[[projects]]\nroot = \"/a\"\ntheme = \"neo\"\nmode = \"acceptEdits\"\n\
             [[projects]]\nroot = \"/b\"\nmode = \"ask\"\n\
             [[projects]]\nroot = \"/c\"\nmode = 3\n\
             [[projects]]\nroot = \"/d\"\nmode = \"plan\"\n",
        );

        assert_eq!(saved.project_mode(Path::new("/a")), None);
        assert_eq!(saved.project_theme(Path::new("/a")), Some("neo"));
        assert_eq!(saved.project_mode(Path::new("/b")), Some(Mode::Ask));
        assert_eq!(saved.project_theme(Path::new("/b")), None);
        assert_eq!(saved.project_mode(Path::new("/c")), None);
        assert_eq!(saved.project_mode(Path::new("/d")), Some(Mode::Plan));
    }

    #[test]
    fn a_model_and_an_effort_are_kept_for_their_profile_and_nothing_else_moves() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let path = kept(dir.path());
        keep(&path, theme("classic")).expect("kept");
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
                effort: Some("high"),
            },
        )
        .expect("kept");

        let saved = Saved::read(&path);
        assert_eq!(saved.last_theme(), Some("classic"));
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
        keep(&path, theme("neo")).expect("kept");
        keep(&path, theme("classic")).expect("kept");
        let once = std::fs::read_to_string(&path).expect("the file was written");

        keep(&path, theme("classic")).expect("kept");

        assert_eq!(Saved::read(&path).last_theme(), Some("classic"));
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
                effort: Some("low"),
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
    fn an_effort_forgotten_leaves_the_profile_s_model_and_no_effort_in_the_file() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let path = kept(dir.path());
        for change in [
            Change::Model {
                profile: "max",
                model: Some("opus"),
            },
            Change::Effort {
                profile: "max",
                effort: Some("max"),
            },
            Change::Effort {
                profile: "work",
                effort: Some("low"),
            },
        ] {
            keep(&path, change).expect("kept");
        }

        for profile in ["max", "work"] {
            keep(
                &path,
                Change::Effort {
                    profile,
                    effort: None,
                },
            )
            .expect("kept");
        }

        let saved = Saved::read(&path);
        assert_eq!(saved.effort("max"), None);
        assert_eq!(saved.model("max"), Some("opus"));
        let text = std::fs::read_to_string(&path).expect("the file was written");
        assert!(!text.contains("effort ="), "{text}");
        assert!(!text.contains("work"), "{text}");
    }

    /// Names a profile may have and a repository's path may hold — a config
    /// may call a profile `"a\nb"`, and on Unix a directory's name may hold
    /// any byte but `/` and NUL — each of which a basic string must escape.
    const AWKWARD: [&str; 9] = [
        "my \"work\" profile.x",
        "odd\\model\"\nwith a break\u{7}",
        "line\nbreak",
        "carriage\rreturn",
        "tab\there",
        "escape \u{1b}[31m red",
        "delete\u{7f}",
        "\\\"\\\\\"",
        "naïve ünïcode 日本 🦀",
    ];

    #[test]
    fn names_and_values_toml_would_misread_are_written_so_they_read_back_as_chosen() {
        for awkward in AWKWARD {
            let dir = tempfile::tempdir().expect("a temporary directory can be made");
            let path = kept(dir.path());
            let root = Path::new(awkward);

            keep(&path, mode_in(root, Mode::Ask)).expect("kept");
            keep(&path, theme_in(root, awkward)).expect("kept");
            keep(
                &path,
                Change::Model {
                    profile: awkward,
                    model: Some(awkward),
                },
            )
            .expect("kept");
            keep(
                &path,
                Change::Effort {
                    profile: awkward,
                    effort: Some(awkward),
                },
            )
            .expect("kept");

            let saved = Saved::read(&path);
            assert_eq!(saved.model(awkward), Some(awkward), "{awkward:?}");
            assert_eq!(saved.effort(awkward), Some(awkward), "{awkward:?}");
            assert_eq!(saved.project_mode(root), Some(Mode::Ask), "{awkward:?}");
            assert_eq!(saved.project_theme(root), Some(awkward), "{awkward:?}");
            assert_eq!(saved.last_theme(), Some(awkward), "{awkward:?}");
        }
    }

    /// The file is every repository's and every profile's: a name written so
    /// that it does not read back would leave the whole file read as nothing
    /// chosen, and the next choice rewrite it from nothing.
    #[test]
    fn a_profile_named_with_a_line_break_keeps_what_every_other_repository_chose() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let path = kept(dir.path());
        let a = Path::new("/work/a");

        keep(&path, mode_in(a, Mode::Ask)).expect("kept");
        keep(
            &path,
            Change::Model {
                profile: "a\nb",
                model: Some("opus"),
            },
        )
        .expect("kept");

        let saved = Saved::read(&path);
        assert_eq!(saved.project_mode(a), Some(Mode::Ask));
        assert_eq!(saved.model("a\nb"), Some("opus"));
    }

    #[test]
    fn a_file_that_is_not_this_module_s_reads_as_nothing_chosen_and_the_next_choice_replaces_it() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let path = kept(dir.path());
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("made");
        std::fs::write(&path, "theme = [\nnot toml").expect("written");
        assert_eq!(Saved::read(&path), Saved::default());

        keep(&path, theme("neo")).expect("kept");

        assert_eq!(Saved::read(&path).last_theme(), Some("neo"));
    }

    #[test]
    fn values_of_the_wrong_kind_and_keys_it_does_not_write_are_passed_over() {
        let saved = Saved::parse(
            "theme = 3\nother = \"x\"\n[profiles]\nbare = \"x\"\n[profiles.max]\n\
             model = \"  \"\neffort = \"high\"\n[profiles.work]\nmodel = [\"opus\"]\n",
        );

        assert_eq!(saved.last_theme(), None);
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
        assert!(keep(&path, theme("neo")).is_err());
    }
}
