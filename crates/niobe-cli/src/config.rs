// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Where the config files are, and the profile a session runs under.
//!
//! Two files, the later overriding the earlier: the user's, in
//! `$XDG_CONFIG_HOME/niobe/config.toml` or `~/.config/niobe/config.toml`, and
//! the repository's, in `.niobe/config.toml` at its root.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use niobe_config::{Config, FILE_NAME};
use niobe_tui::app::SelectedProfile;

use crate::repo;

/// The config of a repository, and the files it was looked for in.
#[derive(Debug)]
pub struct Loaded {
    /// Every file laid over the ones before it.
    pub config: Config,
    /// The files looked in, lowest precedence first, whether or not they exist.
    pub searched: Vec<PathBuf>,
}

impl Loaded {
    /// The profile a session runs under, as the shell names it.
    pub fn selected(&self, requested: Option<&str>) -> Result<Option<SelectedProfile>, String> {
        let selected = self.config.select(requested).map_err(|e| e.to_string())?;
        Ok(selected.map(|s| SelectedProfile {
            name: s.name.to_owned(),
            backend: s.profile.backend(),
        }))
    }
}

/// Reads the user's config and the config of the repository rooted at `root`.
pub fn load(root: &Path) -> Result<Loaded, String> {
    let searched: Vec<PathBuf> = user_path(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
    .into_iter()
    .chain([repo::config_path(root)])
    .collect();

    let paths: Vec<&Path> = searched.iter().map(PathBuf::as_path).collect();
    let config = Config::load(&paths).map_err(|e| e.to_string())?;
    Ok(Loaded { config, searched })
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

    fn os(value: &str) -> Option<OsString> {
        Some(OsString::from(value))
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
