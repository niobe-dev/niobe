// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The choices the operator makes in the shell — the theme, of the project
//! the session runs in and the last anywhere, the mode of that project, and
//! the model and effort of the profile it runs under — kept in the user's
//! config directory, and read back when the next shell opens.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use niobe_config::preferences::{Change, FILE_NAME, Saved};
use niobe_tui::preferences::{Preference, Preferences, PreferencesError};

/// Where the choices are kept: [`FILE_NAME`] in the user's config directory,
/// where `XDG_CONFIG_HOME` or `HOME` names one.
pub fn path(xdg_config_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    crate::config::user_file(xdg_config_home, home, FILE_NAME)
}

/// The choices the last sessions kept, read from where this process's
/// environment says they are; none where it names nowhere.
pub fn saved() -> Saved {
    path(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
    .map(|path| Saved::read(&path))
    .unwrap_or_default()
}

/// Keeps the shell's choices in the user's config directory, the theme and
/// the mode as the project's the session runs in, and the model and effort
/// under the profile it runs under.
#[derive(Debug, Clone)]
pub struct ChoicesFile {
    path: Option<PathBuf>,
    project: Option<PathBuf>,
    profile: Option<String>,
}

impl ChoicesFile {
    /// Keeps choices where this process's environment says, for the project
    /// rooted at `root`, under `profile`.
    pub fn for_session(root: &Path, profile: Option<&str>) -> Self {
        Self::at(
            path(
                std::env::var_os("XDG_CONFIG_HOME"),
                std::env::var_os("HOME"),
            ),
            Some(root),
            profile,
        )
    }

    /// Keeps choices in the file at `path`, for the project rooted at
    /// `project`, under `profile`.
    fn at(path: Option<PathBuf>, project: Option<&Path>, profile: Option<&str>) -> Self {
        Self {
            path,
            project: project.map(Path::to_path_buf),
            profile: profile.map(str::to_owned),
        }
    }
}

/// Said where neither variable names a directory to keep choices in.
const NOWHERE: &str = "neither XDG_CONFIG_HOME nor HOME names a directory to keep it in";

impl Preferences for ChoicesFile {
    /// Keeps `preference`. A model or an effort chosen under no profile is
    /// kept nowhere, and that is not a failure: with no profile there is no
    /// backend, and nothing for the next session to start with it. A mode
    /// chosen in no project is kept nowhere either, since it is a project's.
    fn keep(&mut self, preference: &Preference) -> Result<(), PreferencesError> {
        let change = match (preference, self.profile.as_deref()) {
            (Preference::Theme(theme), _) => Change::Theme {
                project: self.project.as_deref(),
                theme,
            },
            (Preference::Mode(mode), _) => match self.project.as_deref() {
                Some(project) => Change::Mode {
                    project,
                    mode: *mode,
                },
                None => return Ok(()),
            },
            (Preference::Model(model), Some(profile)) => Change::Model {
                profile,
                model: model.as_deref(),
            },
            (Preference::Effort(effort), Some(profile)) => Change::Effort { profile, effort },
            (Preference::Model(_) | Preference::Effort(_), None) => return Ok(()),
        };
        let path = self.path.as_deref().ok_or(NOWHERE)?;
        niobe_config::preferences::keep(path, change).map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use niobe_core::event::Mode;

    #[test]
    fn the_choices_kept_are_the_ones_the_next_session_reads_in_its_project_under_its_profile() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let path = path(Some(dir.path().into()), None).expect("an absolute directory");
        let (here, there) = (Path::new("/work/here"), Path::new("/work/there"));
        let mut max = ChoicesFile::at(Some(path.clone()), Some(here), Some("max"));
        let mut work = ChoicesFile::at(Some(path.clone()), Some(there), Some("work"));

        for preference in [
            Preference::Theme("neo".to_owned()),
            Preference::Model(Some("opus".to_owned())),
            Preference::Effort("high".to_owned()),
            Preference::Mode(Mode::Ask),
        ] {
            max.keep(&preference).expect("kept");
        }
        work.keep(&Preference::Model(Some("sonnet".to_owned())))
            .expect("kept");
        work.keep(&Preference::Mode(Mode::Auto)).expect("kept");
        work.keep(&Preference::Theme("classic".to_owned()))
            .expect("kept");

        let saved = Saved::read(&path);
        assert_eq!(saved.project_theme(here), Some("neo"));
        assert_eq!(saved.project_theme(there), Some("classic"));
        assert_eq!(saved.last_theme(), Some("classic"));
        assert_eq!(saved.model("max"), Some("opus"));
        assert_eq!(saved.effort("max"), Some("high"));
        assert_eq!(saved.model("work"), Some("sonnet"));
        assert_eq!(saved.effort("work"), None);
        assert_eq!(saved.project_mode(here), Some(Mode::Ask));
        assert_eq!(saved.project_mode(there), Some(Mode::Auto));
    }

    #[test]
    fn a_model_under_no_profile_and_a_mode_in_no_project_are_kept_nowhere_and_a_theme_still_is() {
        let dir = tempfile::tempdir().expect("a temporary directory can be made");
        let path = path(Some(dir.path().into()), None).expect("an absolute directory");
        let mut none = ChoicesFile::at(Some(path.clone()), None, None);

        none.keep(&Preference::Model(Some("opus".to_owned())))
            .expect("nothing to keep is no failure");
        none.keep(&Preference::Mode(Mode::Ask))
            .expect("nothing to keep is no failure");
        none.keep(&Preference::Theme("classic".to_owned()))
            .expect("kept");

        assert_eq!(Saved::read(&path).last_theme(), Some("classic"));
        assert!(
            !std::fs::read_to_string(&path)
                .expect("written")
                .contains("opus")
        );
        let text = std::fs::read_to_string(&path).expect("written");
        assert!(!text.contains("[[projects]]"), "{text}");
    }

    #[test]
    fn with_no_directory_to_keep_it_in_a_choice_says_why_it_was_not_kept() {
        let mut nowhere = ChoicesFile::at(path(None, None), None, Some("max"));

        let error = nowhere
            .keep(&Preference::Theme("neo".to_owned()))
            .expect_err("there is nowhere to keep it");

        assert_eq!(error.to_string(), NOWHERE);
    }
}
