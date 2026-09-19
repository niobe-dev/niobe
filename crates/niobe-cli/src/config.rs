// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Where the config files are, and the profile a session runs under.
//!
//! Two files, the later overriding the earlier: the user's, in
//! `$XDG_CONFIG_HOME/niobe/config.toml` or `~/.config/niobe/config.toml`, and
//! the repository's, in `.niobe/config.toml` at its root.
//!
//! The repository's file arrives with the clone, so what it may put in front
//! of a backend is gated on the operator having read it: see
//! [`niobe_config::trust`]. The record of that decision sits beside the user's
//! config, which is the one directory a repository cannot write to.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use niobe_config::trust::Trusted;
use niobe_config::{Config, FILE_NAME, Selected, Settings};
use niobe_tui::app::SelectedProfile;

use crate::repo;

/// The config of a repository, and the files it was looked for in.
#[derive(Debug)]
pub struct Loaded {
    /// Every file laid over the ones before it.
    pub config: Config,
    /// The files looked in, lowest precedence first, whether or not they exist.
    pub searched: Vec<PathBuf>,
    /// The repository's config, where it sets something that takes effect only
    /// once it has been trusted and has not been trusted. What it set is not
    /// in `config`.
    pub untrusted: Option<PathBuf>,
}

impl Loaded {
    /// The profile a session runs under: the backend it runs and everything it
    /// runs with.
    pub fn select(&self, requested: Option<&str>) -> Result<Option<Selected<'_>>, String> {
        self.config.select(requested).map_err(|e| e.to_string())
    }

    /// The profile a session runs under, as the shell names it.
    pub fn selected(&self, requested: Option<&str>) -> Result<Option<SelectedProfile>, String> {
        Ok(self.select(requested)?.map(named))
    }
}

/// A selected profile as the shell names it: the name, the backend it runs and
/// the models it offers. The shell is given no more, because it can use no
/// more.
pub fn named(selected: Selected<'_>) -> SelectedProfile {
    SelectedProfile {
        name: selected.name.to_owned(),
        backend: selected.profile.backend(),
        models: selected.profile.models().to_vec(),
    }
}

/// The settings file `selected` starts its backend under, where it names one,
/// as a path on this machine.
///
/// A leading `~` stands for the home directory, the way it does in a shell and
/// the way a settings path is written by hand. Nothing else is expanded: a
/// path only some shells would resolve is a path that runs a session on
/// credentials nobody chose.
///
/// The file has to be there, and this is looked at before a backend is
/// spawned. The `claude` CLI started without the settings a profile named
/// would run on the settings the machine holds for every session — which, on a
/// machine that has a second account configured, is the account the profile
/// was written to keep it off.
pub fn settings_path(
    selected: &Selected<'_>,
    home: Option<OsString>,
) -> Result<Option<String>, String> {
    let Some(settings) = selected.profile.settings() else {
        return Ok(None);
    };
    let said = |message: String| selected.settings_invalid(settings, &message).to_string();
    let path = expanded(settings, home.as_deref()).ok_or_else(|| {
        said(format!(
            "names `{}`, and HOME does not say what `~` is",
            settings.path()
        ))
    })?;

    if !Path::new(&path).is_file() {
        let problem = match Path::new(&path).exists() {
            true => "is not a file",
            false => "no such file",
        };
        return Err(said(format!("{problem}: {path}")));
    }
    Ok(Some(path))
}

/// `settings` as a path, with a leading `~` replaced by `home`.
///
/// `None` where the path starts with `~/` and `home` does not name a
/// directory this can expand it against, which is a path Niobe would
/// otherwise have to guess at. A `~` followed by anything else — `~other` —
/// is a path this does not expand, and is taken as written.
fn expanded(settings: &Settings, home: Option<&OsStr>) -> Option<String> {
    let written = settings.path();
    let Some(under) = written.strip_prefix('~') else {
        return Some(written.to_owned());
    };
    if !under.is_empty() && !under.starts_with('/') {
        return Some(written.to_owned());
    }
    let home = home
        .and_then(OsStr::to_str)
        .filter(|home| !home.is_empty())?;
    let under = under.trim_start_matches('/');
    match under.is_empty() {
        true => Some(home.to_owned()),
        false => Some(format!("{}/{under}", home.trim_end_matches('/'))),
    }
}

/// Reads the user's config and the config of the repository rooted at `root`,
/// the second laid over the first and gated on having been trusted.
pub fn load(root: &Path) -> Result<Loaded, String> {
    let user = user_path(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    );
    let repo = repo::config_path(root);
    let searched: Vec<PathBuf> = user.iter().cloned().chain([repo.clone()]).collect();

    let mut config = match &user {
        Some(user) => Config::read(user)
            .map_err(|e| e.to_string())?
            .unwrap_or_default(),
        None => Config::default(),
    };
    let mut untrusted = None;
    if let Some(layer) = repository(&repo)? {
        config = config.overlay(layer.config);
        untrusted = layer.untrusted;
    }

    Ok(Loaded {
        config,
        searched,
        untrusted,
    })
}

/// One config file, as it applies.
struct Layer {
    config: Config,
    /// The file, where what it sets is being held back for want of trust.
    untrusted: Option<PathBuf>,
}

/// The config of the repository whose file is `path`: whole where the operator
/// has trusted these contents, and without what an untrusted file may not put
/// in front of a backend otherwise.
///
/// The file is read once and both parsed and hashed from that text, so the
/// config that is laid on is the one the digest was taken of.
fn repository(path: &Path) -> Result<Option<Layer>, String> {
    let Some(text) = read(path)? else {
        return Ok(None);
    };
    let config = Config::parse(&text, path).map_err(|e| e.to_string())?;
    if trusted(path, &text)? {
        return Ok(Some(Layer {
            config,
            untrusted: None,
        }));
    }
    let untrusted = config.needs_trust().then(|| path.to_path_buf());
    Ok(Some(Layer {
        config: config.untrusted(),
        untrusted,
    }))
}

/// Whether the repository config at `path`, holding `text`, has been trusted
/// as it now stands.
///
/// A machine that says where nothing of the operator's lives has nowhere to
/// keep the decision, so it has not been made: a repository is gated rather
/// than ungated by an environment that says nothing.
fn trusted(path: &Path, text: &str) -> Result<bool, String> {
    let Some(record) = trust_path() else {
        return Ok(false);
    };
    Ok(Trusted::read(&record)
        .map_err(|e| e.to_string())?
        .trusts(path, text))
}

/// Where this machine keeps the record of the config files it has trusted.
pub fn trust_path() -> Option<PathBuf> {
    user_file(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
        niobe_config::trust::FILE_NAME,
    )
}

/// The text of `path`, or `None` where there is no such file.
fn read(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("cannot read {}: {error}", path.display())),
    }
}

/// The user's config file, from the values of `XDG_CONFIG_HOME` and `HOME`.
pub fn user_path(xdg_config_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    user_file(xdg_config_home, home, FILE_NAME)
}

/// The file `name` in the user's Niobe config directory, from the values of
/// `XDG_CONFIG_HOME` and `HOME`.
///
/// `XDG_CONFIG_HOME` counts only when it is an absolute path, as the XDG base
/// directory specification says; a relative one would name a different file
/// in every directory. With neither variable usable there is no user file.
pub fn user_file(
    xdg_config_home: Option<OsString>,
    home: Option<OsString>,
    name: &str,
) -> Option<PathBuf> {
    let base = xdg_config_home
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| {
            home.map(PathBuf::from)
                .filter(|dir| dir.is_absolute())
                .map(|home| home.join(".config"))
        })?;
    Some(base.join(niobe_core::APP_NAME).join(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shell_is_told_which_models_the_selected_profile_offers() {
        let config = niobe_config::Config::parse(
            "[profiles.max]\nbackend = \"claude\"\nmodels = [\"opus\", \"haiku\"]\n",
            Path::new("/u/config.toml"),
        )
        .expect("the config is valid");
        let selected = config
            .select(Some("max"))
            .expect("the profile is defined")
            .expect("a profile was selected");

        assert_eq!(named(selected).models, ["opus", "haiku"]);
    }

    fn os(value: &str) -> Option<OsString> {
        Some(OsString::from(value))
    }

    /// A config whose `max` profile names `path` as its settings file.
    fn with_settings(path: &str) -> niobe_config::Config {
        niobe_config::Config::parse(
            &format!("[profiles.max]\nbackend = \"claude\"\nsettings = \"{path}\"\n"),
            Path::new("/u/config.toml"),
        )
        .expect("the config is valid")
    }

    /// The `max` profile of `config`, selected.
    fn max(config: &niobe_config::Config) -> Selected<'_> {
        config
            .select(Some("max"))
            .expect("the profile is defined")
            .expect("a profile was selected")
    }

    #[test]
    fn a_settings_path_is_expanded_against_the_home_directory() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let file = dir.path().join("claude-personal.json");
        std::fs::write(&file, "{}").expect("the settings file is written");
        let config = with_settings("~/claude-personal.json");

        assert_eq!(
            settings_path(&max(&config), Some(OsString::from(dir.path()))),
            Ok(Some(file.display().to_string()))
        );
    }

    #[test]
    fn a_settings_path_that_is_not_under_the_home_directory_is_taken_as_written() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let file = dir.path().join("elsewhere.json");
        std::fs::write(&file, "{}").expect("the settings file is written");
        let config = with_settings(&file.display().to_string());

        assert_eq!(
            settings_path(&max(&config), os("/home/me")),
            Ok(Some(file.display().to_string()))
        );
    }

    #[test]
    fn a_profile_that_names_no_settings_file_runs_its_backend_under_none() {
        let config = niobe_config::Config::parse(
            "[profiles.max]\nbackend = \"claude\"\n",
            Path::new("/u/config.toml"),
        )
        .expect("the config is valid");

        assert_eq!(settings_path(&max(&config), os("/home/me")), Ok(None));
    }

    #[test]
    fn a_settings_file_that_is_not_there_is_reported_at_its_key_and_its_line() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let missing = dir.path().join("gone.json");
        let config = with_settings(&missing.display().to_string());

        assert_eq!(
            settings_path(&max(&config), os("/home/me")),
            Err(format!(
                "/u/config.toml:3: profiles.max.settings: no such file: {}",
                missing.display()
            ))
        );

        // A directory where the file should be is not a settings file either.
        let config = with_settings(&dir.path().display().to_string());
        assert_eq!(
            settings_path(&max(&config), os("/home/me")),
            Err(format!(
                "/u/config.toml:3: profiles.max.settings: is not a file: {}",
                dir.path().display()
            ))
        );
    }

    #[test]
    fn a_settings_path_under_a_home_nothing_names_is_reported_rather_than_guessed() {
        let config = with_settings("~/claude-personal.json");

        assert_eq!(
            settings_path(&max(&config), None),
            Err("/u/config.toml:3: profiles.max.settings: names `~/claude-personal.json`, and HOME does not say what `~` is".to_owned())
        );
    }

    #[test]
    fn the_user_config_is_under_xdg_config_home_or_else_under_the_home_directory() {
        assert_eq!(
            user_path(os("/xdg"), os("/home/me")),
            Some(PathBuf::from("/xdg/niobe/config.toml"))
        );
        assert_eq!(
            user_path(None, os("/home/me")),
            Some(PathBuf::from("/home/me/.config/niobe/config.toml"))
        );
    }

    #[test]
    fn other_user_files_sit_beside_the_config() {
        assert_eq!(
            user_file(os("/xdg"), None, "prices.toml"),
            Some(PathBuf::from("/xdg/niobe/prices.toml"))
        );
    }

    #[test]
    fn a_relative_config_home_is_ignored() {
        assert_eq!(
            user_path(os("relative/xdg"), os("/home/me")),
            Some(PathBuf::from("/home/me/.config/niobe/config.toml"))
        );
        assert_eq!(user_path(os(""), None), None);
        assert_eq!(user_path(None, os("not/absolute")), None);
    }
}
