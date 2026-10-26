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
use niobe_config::{Config, FILE_NAME, Profile, Selected, Settings};
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
    pub untrusted: Option<Untrusted>,
    /// Whether no config file exists anywhere it was looked for, so that
    /// `config` is [`Config::implied`] and not something written.
    pub implied: bool,
}

/// A repository's config that sets what only a trusted file may, as it was
/// read.
#[derive(Debug)]
pub struct Untrusted {
    /// Where it is.
    pub path: PathBuf,
    /// What it held when it was read. Trusting it records this text, so that
    /// what is put in force is what the operator was shown, even where the
    /// file changed in between.
    pub text: String,
    /// What it sets, whole: before what an untrusted file may not set was
    /// taken out of it.
    pub config: Config,
    /// The profiles the user's config defines that it defines too, which it
    /// replaces once trusted.
    pub replaces: Vec<String>,
    /// The files it names that a backend is started with the contents of: a
    /// profile's `settings` file, and a file an `args` entry names. They are
    /// trusted with it, each at the contents shown.
    pub attached: Vec<Attached>,
}

/// A file a repository's config names that the CLI reads when it starts: its
/// `settings`, or a path in its `args`.
///
/// The config's own digest does not cover it, and a pull can change it alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attached {
    /// Where it is, as the CLI would find it.
    pub path: PathBuf,
    /// What it holds, or why it cannot be read as text, which leaves it
    /// untrusted: a file that cannot be shown cannot be agreed to.
    pub text: Result<String, String>,
    /// The profiles that name it, in the config's order.
    pub profiles: Vec<String>,
    /// Whether one of them names it as its `settings` file, as opposed to an
    /// argument.
    pub settings: bool,
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

/// A selected profile as the shell names it: the name, the backend it runs,
/// the models it offers and how it says it is billed. The shell is given no more, because it can use no
/// more.
pub fn named(selected: Selected<'_>) -> SelectedProfile {
    SelectedProfile {
        name: selected.name.to_owned(),
        backend: selected.profile.backend(),
        models: selected.profile.models().to_vec(),
        billing: selected.profile.billing(),
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
/// A path that is still relative is read from beside the file that wrote it:
/// the user's own config directory for a profile of the user's file, and the
/// repository's root, `root`, for a profile of the repository's, which the CLI
/// is started in and which that file was trusted with. A user's profile never
/// reads one from the root: that is a file the clone supplies, and the user's
/// profile is never asked about. It is resolved once, here, so the file that
/// is checked is the file that is passed.
///
/// The file has to be there, and this is looked at before a backend is
/// spawned. The `claude` CLI started without the settings a profile named
/// would run on the settings the machine holds for every session — which, on a
/// machine that has a second account configured, is the account the profile
/// was written to keep it off.
pub fn settings_path(
    selected: &Selected<'_>,
    home: Option<OsString>,
    root: &Path,
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
    let path = match Path::new(&path).is_absolute() {
        true => path,
        false => relative_base(selected, root)
            .join(&path)
            .display()
            .to_string(),
    };

    if from_repository(selected, root) {
        // Prevent path traversal outside repository boundary for repository-defined profiles.
        if let Err(error) = repo::inside(root, Path::new(&path)) {
            return Err(said(error));
        }
    }

    if !Path::new(&path).is_file() {
        let problem = match Path::new(&path).exists() {
            true => "is not a file",
            false => "no such file",
        };
        return Err(said(format!("{problem}: {path}")));
    }
    Ok(Some(path))
}

/// Whether `selected` is a profile of the repository's own config, rooted at
/// `root`, rather than of the user's.
fn from_repository(selected: &Selected<'_>, root: &Path) -> bool {
    selected.profile.source() == repo::config_path(root)
}

/// The directory a relative path `selected` names is read from: the root for
/// a profile of the repository's config, and the directory of the file that
/// wrote it for any other.
fn relative_base(selected: &Selected<'_>, root: &Path) -> PathBuf {
    match from_repository(selected, root) {
        true => root.to_path_buf(),
        false => selected
            .profile
            .source()
            .parent()
            .map_or_else(PathBuf::new, Path::to_path_buf),
    }
}

/// Refuses a profile of the user's whose `args` name, by a relative path, a
/// file the repository rooted at `root` holds.
///
/// The CLI reads a relative path among its arguments from the root it is
/// started in, so such an argument hands it a file the clone supplies — an
/// `--mcp-config` that runs a server of the repository's choosing — under a
/// profile nobody was asked about. Niobe cannot rewrite an argument it does
/// not know is a path, so it refuses to start and says how to write it. A
/// repository's own profile is let through: the files its arguments name are
/// trusted with it (see [`attached`]).
pub fn args_checked(selected: &Selected<'_>, root: &Path) -> Result<(), String> {
    if from_repository(selected, root) {
        return Ok(());
    }
    for arg in selected.profile.args() {
        if let Some(path) = path_candidates(arg)
            .filter(|candidate| Path::new(candidate).is_relative())
            .map(|candidate| root.join(candidate))
            .find(|path| path.is_file())
        {
            return Err(selected.args_invalid(&format!(
                "`{arg}` is read by the CLI from the repository, as {}, a file the \
                 repository supplies; write it as an absolute path",
                path.display()
            )));
        }
    }
    Ok(())
}

/// Refuses a profile of the user's whose `env` would have the CLI, started in
/// the repository rooted at `root`, read something of the repository's by a
/// relative path.
///
/// Three kinds of value are read that way. Every entry of `PATH` is a
/// directory a program is looked up in, and the spawn looks a relative entry
/// up — and an empty one, which is the current directory — in the root the
/// CLI is started in: a `claude` the clone committed there is the program that
/// runs, as is anything the CLI then runs by name. `$PATH` is such an entry,
/// since nothing expands it. Each variable in `directories` names a directory
/// the backend reads its own configuration from, which a relative value puts
/// inside the repository whether or not the clone holds it yet. Any other value
/// is refused where a word of it names, by a relative path, a file or directory
/// the repository holds — a `NODE_OPTIONS` requiring `./x.js` — as
/// [`args_checked`] refuses an argument. A repository's own profile is let
/// through: its environment was trusted with it.
pub fn env_checked(
    selected: &Selected<'_>,
    root: &Path,
    directories: &[&str],
) -> Result<(), String> {
    if from_repository(selected, root) {
        return Ok(());
    }
    for (name, value) in selected.profile.env() {
        let refusal = match name.as_str() {
            "PATH" => std::env::split_paths(value)
                .any(|entry| !entry.is_absolute())
                .then(|| {
                    format!(
                        "`{value}` has an empty or relative entry, which the CLI's \
                         programs are looked up in from the repository, where a clone \
                         can put a `claude` of its own; write every entry as an \
                         absolute path"
                    )
                }),
            _ if directories.contains(&name.as_str()) => {
                Path::new(value).is_relative().then(|| {
                    format!(
                        "`{value}` is a directory inside the repository to the CLI, \
                         which reads its settings from there; write it as an \
                         absolute path"
                    )
                })
            }
            _ => value
                .split_whitespace()
                .flat_map(path_candidates)
                .filter(|candidate| Path::new(candidate).is_relative())
                .map(|candidate| root.join(candidate))
                .find(|path| std::fs::symlink_metadata(path).is_ok())
                .map(|path| {
                    format!(
                        "`{value}` is read by the CLI from the repository, as {}, which \
                         the repository supplies; write it as an absolute path",
                        path.display()
                    )
                }),
        };
        if let Some(refusal) = refusal {
            return Err(selected.env_invalid(name, &refusal));
        }
    }
    Ok(())
}

/// What in an `args` entry could name a file: the entry, and what follows its
/// `=`, where neither is empty or a flag.
fn path_candidates(arg: &str) -> impl Iterator<Item = &str> {
    let value = arg.split_once('=').map_or(arg, |(_, value)| value);
    [arg, value]
        .into_iter()
        .filter(|candidate| !candidate.is_empty() && !candidate.starts_with('-'))
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
    load_from(user, root)
}

/// The config a session runs under in a repository with no config of its
/// own: the user's file, or the implied profile where there is none — what
/// `niobe bench` runs each task's turn under, since each runs in a copy made
/// outside any repository.
pub fn load_user() -> Result<Config, String> {
    let user = user_path(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    );
    let present = user
        .as_deref()
        .is_some_and(|path| std::fs::symlink_metadata(path).is_ok());
    match present {
        true => user_config(user.as_deref()),
        false => Ok(Config::implied()),
    }
}

/// [`load`], with the user's config file handed in rather than found.
fn load_from(user: Option<PathBuf>, root: &Path) -> Result<Loaded, String> {
    load_with(user, root, trust_path(), std::env::var_os("HOME"))
}

/// [`load_from`], with the trust record and the home directory handed in
/// rather than found, so that a test can hold its own.
fn load_with(
    user: Option<PathBuf>,
    root: &Path,
    record: Option<PathBuf>,
    home: Option<OsString>,
) -> Result<Loaded, String> {
    let repo = repo::config_path(root);
    let searched: Vec<PathBuf> = user.iter().cloned().chain([repo.clone()]).collect();

    let user_present = user
        .as_deref()
        .is_some_and(|path| std::fs::symlink_metadata(path).is_ok());
    let mut config = user_config(user.as_deref())?;
    let mut untrusted = None;
    let mut repo_present = false;
    repo::inside(root, &repo)?;
    if let Some(layer) = repository(&repo, root, &config, record.as_deref(), home.as_deref())? {
        repo_present = true;
        // A file that defines a profile the user's does is worth trusting
        // even where it sets nothing else: until it is trusted, its profile of
        // that name is not the one in force.
        if layer.gated && (layer.needs_trust || config.replaces(&layer.config)) {
            untrusted = Some(Untrusted {
                path: repo.clone(),
                replaces: replaced(&config, &layer.whole),
                text: layer.text,
                config: layer.whole,
                attached: layer.attached,
            });
        }
        config = config.overlay(layer.config);
    }

    // The implied profile stands in for a file and is never laid under one:
    // any file at all, even an empty one, is what the operator wrote.
    let implied = !user_present && !repo_present;
    if implied {
        config = Config::implied();
    }

    Ok(Loaded {
        config,
        searched,
        untrusted,
        implied,
    })
}

/// The repository's config at `path`, holding `text`, read whole whether or
/// not it has been trusted, with the user's profiles it would replace: what
/// `niobe trust` shows before it records anything.
///
/// A file that is not a config says nothing the operator could read and agree
/// to, so it is refused as one that is not trusted.
pub fn as_written(path: &Path, text: String) -> Result<Untrusted, String> {
    let config = Config::parse(&text, path)
        .map_err(|error| format!("{} is not trusted: {error}", path.display()))?;
    let user = user_config(
        user_path(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME"),
        )
        .as_deref(),
    )?;
    let home = std::env::var_os("HOME");
    Ok(Untrusted {
        path: path.to_owned(),
        replaces: replaced(&user, &config),
        attached: attached(&config, &root_of(path), home.as_deref()),
        text,
        config,
    })
}

/// The user's config at `user`, or none where there is no such file or
/// nowhere it could be.
fn user_config(user: Option<&Path>) -> Result<Config, String> {
    match user {
        Some(user) => Ok(Config::read(user)
            .map_err(|e| e.to_string())?
            .unwrap_or_default()),
        None => Ok(Config::default()),
    }
}

/// The profiles of `user` that `repository`, read whole, defines too.
fn replaced(user: &Config, repository: &Config) -> Vec<String> {
    repository
        .profiles()
        .keys()
        .filter(|name| user.profiles().contains_key(*name))
        .cloned()
        .collect()
}

/// One config file, as it applies.
struct Layer {
    config: Config,
    /// The file as it was written, before anything was withheld from it.
    whole: Config,
    /// The text `whole` was parsed from.
    text: String,
    /// The files `whole` names that the CLI reads, as they are now.
    attached: Vec<Attached>,
    /// Whether the file has not been trusted as it stands, so that `config` is
    /// what [`Config::untrusted`] leaves of it.
    gated: bool,
    /// Whether the file, read whole, sets something that takes effect only
    /// once it has been trusted.
    needs_trust: bool,
}

/// The config of the repository whose file is `path`: whole where the operator
/// has trusted these contents, and without what an untrusted file may not put
/// in front of a backend otherwise.
///
/// The file is read once and both parsed and hashed from that text, so the
/// config that is laid on is the one the digest was taken of.
fn repository(
    path: &Path,
    root: &Path,
    user: &Config,
    record: Option<&Path>,
    home: Option<&OsStr>,
) -> Result<Option<Layer>, String> {
    let Some(text) = read(path)? else {
        return Ok(None);
    };
    let config = Config::parse(&text, path).map_err(|e| e.to_string())?;
    let needs_trust = config.needs_trust();
    let attached = attached(&config, root, home);
    let replaces = replaced(user, &config);
    if trusted(record, path, &text, &attached, &replaces)? {
        return Ok(Some(Layer {
            whole: config.clone(),
            config,
            text,
            attached,
            gated: false,
            needs_trust,
        }));
    }
    Ok(Some(Layer {
        whole: config.clone(),
        config: config.untrusted(),
        text,
        attached,
        gated: true,
        needs_trust,
    }))
}

/// The root of the repository whose config file is `path`: two directories up,
/// at `.niobe/config.toml`.
fn root_of(path: &Path) -> PathBuf {
    path.parent()
        .and_then(Path::parent)
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// The files the profiles of `config` name that the CLI reads when it starts,
/// each once, as they are now.
///
/// A relative path is the repository's: the CLI reads it from the root. A
/// file an `args` entry names is one that exists as a regular file there, in
/// the entry or after its `=`; an entry that names nothing is a flag or a
/// value, and covers nothing. A `settings` file that is not there covers
/// nothing either, and the profile is refused before a backend starts; when it
/// appears it is a file that was not trusted.
pub fn attached(config: &Config, root: &Path, home: Option<&OsStr>) -> Vec<Attached> {
    let mut files: Vec<Attached> = Vec::new();
    let repo_config = repo::config_path(root);
    let mut add = |path: PathBuf, profile: &Profile, name: &str, settings: bool| {
        if !path.is_file() {
            return;
        }
        // A repository-defined profile's attached settings and arguments must stay within the repository root.
        if profile.source() == repo_config && repo::inside(root, &path).is_err() {
            return;
        }
        match files.iter_mut().find(|file| file.path == path) {
            Some(file) => {
                file.settings |= settings;
                if !file.profiles.iter().any(|p| p == name) {
                    file.profiles.push(name.to_owned());
                }
            }
            None => files.push(Attached {
                text: niobe_config::read::text(&path).map_err(|error| error.to_string()),
                path,
                profiles: vec![name.to_owned()],
                settings,
            }),
        }
    };
    for (name, profile) in config.profiles() {
        if let Some(settings) = profile.settings()
            && let Some(written) = expanded(settings, home)
        {
            add(root.join(written), profile, name, true);
        }
        for arg in profile.args() {
            for candidate in path_candidates(arg) {
                add(root.join(candidate), profile, name, false);
            }
        }
    }
    files
}

/// Whether the repository config at `path`, holding `text`, and the files it
/// names that the CLI reads, `attached`, have been trusted as they now stand.
///
/// A machine that says where nothing of the operator's lives has nowhere to
/// keep the decision, so it has not been made: a repository is gated rather
/// than ungated by an environment that says nothing. A file that cannot be
/// read as text is not trusted, because nobody could have been shown it.
///
/// The profiles the file replaces of the user's, `replaces`, are part of what
/// was agreed to: a profile the operator adds to their own config afterwards
/// under a name the file defines is a replacement nobody was asked about.
/// A record from before the names were kept stands for none.
fn trusted(
    record: Option<&Path>,
    path: &Path,
    text: &str,
    attached: &[Attached],
    replaces: &[String],
) -> Result<bool, String> {
    let Some(record) = record else {
        return Ok(false);
    };
    let record = Trusted::read(record).map_err(|e| e.to_string())?;
    let names = replaces_key(path);
    let names_agreed = match record.holds(&names) {
        true => record.trusts(&names, &replaces.join("\n")),
        false => replaces.is_empty(),
    };
    Ok(record.trusts(path, text)
        && names_agreed
        && attached.iter().all(|file| match &file.text {
            Ok(text) => record.trusts(&attached_key(&file.path), text),
            Err(_) => false,
        }))
}

/// Records `untrusted` in the record at `record` as read by the operator: the
/// config at the text they were shown, the names of the user's profiles it
/// replaces, and each file it names that the CLI reads, at the contents shown.
///
/// A file the operator could not be shown is not agreed to, and nothing is
/// recorded.
pub fn record_trust(record: &Path, untrusted: &Untrusted) -> Result<(), String> {
    let path = &untrusted.path;
    let mut files = Vec::new();
    for file in &untrusted.attached {
        match &file.text {
            Ok(text) => files.push((&file.path, text)),
            Err(error) => {
                return Err(format!(
                    "{} is not trusted: {} cannot be read: {error}",
                    path.display(),
                    file.path.display()
                ));
            }
        }
    }
    Trusted::update(record, |trusted| {
        trusted.trust(path, &untrusted.text)?;
        trusted.trust(&replaces_key(path), &untrusted.replaces.join("\n"))?;
        for (file, text) in &files {
            trusted.trust(&attached_key(file), text)?;
        }
        Ok(())
    })
    .map_err(|e| e.to_string())
}

/// Where the record keeps the names of the user's profiles a trusted config
/// replaces: beside the config's own entry, under a name no file has.
pub fn replaces_key(config: &Path) -> PathBuf {
    let mut key = config.as_os_str().to_owned();
    key.push("#replaces");
    PathBuf::from(key)
}

/// Where the record keeps a file a trusted config names that the CLI reads:
/// under its own path with a mark after it, so that it is never the entry of a
/// config.
///
/// A config's entry is its path, and a file a config names can be anywhere —
/// another repository's config included, such as a submodule's, or that of a
/// subdirectory of a copy with no `.git`. Kept under its bare path, trusting
/// the config that names it, which shows the file by its path alone, would
/// put that repository's config in force unread. Every config's entry ends in
/// `config.toml`, and every key this makes ends in `#attached`.
pub fn attached_key(file: &Path) -> PathBuf {
    let mut key = file.as_os_str().to_owned();
    key.push("#attached");
    PathBuf::from(key)
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
    match niobe_config::read::text(path) {
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

    /// The CLI is started in the repository's root and reads a relative path
    /// in the repository's config from there, so that is where it is looked
    /// for, whichever directory niobe itself was started in; and the path
    /// handed on is the one looked at.
    #[test]
    fn a_relative_settings_path_is_the_repositorys_and_not_the_working_directorys() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).expect("the temporary directory is writable");
        let real = root.join("claude.json");
        std::fs::write(&real, "{}").expect("the temporary directory is writable");
        let config = niobe_config::Config::parse(
            "[profiles.max]\nbackend = \"claude\"\nsettings = \"claude.json\"\n",
            &repo::config_path(&root),
        )
        .expect("the config is valid");

        assert_eq!(
            settings_path(&max(&config), os("/home/me"), &root),
            Ok(Some(real.display().to_string()))
        );

        // A file of that name beside the process, which the CLI would never
        // load, is not what is looked at: with none in the root, it is missing.
        let elsewhere = dir.path().join("sub");
        std::fs::create_dir_all(&elsewhere).expect("the temporary directory is writable");
        std::fs::write(elsewhere.join("claude.json"), "{}")
            .expect("the temporary directory is writable");
        std::fs::remove_file(&real).expect("the file was just created in a temporary directory");
        let said =
            settings_path(&max(&config), os("/home/me"), &root).expect_err("the root holds none");
        assert!(
            said.contains(&format!("no such file: {}", real.display())),
            "{said}"
        );
    }

    #[test]
    fn a_settings_path_is_expanded_against_the_home_directory() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let file = dir.path().join("claude-personal.json");
        std::fs::write(&file, "{}").expect("the settings file is written");
        let config = with_settings("~/claude-personal.json");

        assert_eq!(
            settings_path(
                &max(&config),
                Some(OsString::from(dir.path())),
                Path::new("/repo")
            ),
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
            settings_path(&max(&config), os("/home/me"), Path::new("/repo")),
            Ok(Some(file.display().to_string()))
        );
    }

    /// `~other` is another account's home in a shell, not this one's, so it
    /// is left as written rather than expanded against this home.
    #[test]
    fn a_tilde_naming_another_user_is_left_as_written() {
        let config = with_settings("~other/settings.json");

        let said = settings_path(&max(&config), os("/home/me"), Path::new("/repo"))
            .expect_err("no such file is there as written");

        assert!(said.contains("~other/settings.json"), "{said}");
        assert!(!said.contains("/home/me"), "{said}");
    }

    #[test]
    fn a_profile_that_names_no_settings_file_runs_its_backend_under_none() {
        let config = niobe_config::Config::parse(
            "[profiles.max]\nbackend = \"claude\"\n",
            Path::new("/u/config.toml"),
        )
        .expect("the config is valid");

        assert_eq!(
            settings_path(&max(&config), os("/home/me"), Path::new("/repo")),
            Ok(None)
        );
    }

    #[test]
    fn a_settings_file_that_is_not_there_is_reported_at_its_key_and_its_line() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let missing = dir.path().join("gone.json");
        let config = with_settings(&missing.display().to_string());

        assert_eq!(
            settings_path(&max(&config), os("/home/me"), Path::new("/repo")),
            Err(format!(
                "/u/config.toml:3: profiles.max.settings: no such file: {}",
                missing.display()
            ))
        );

        // A directory where the file should be is not a settings file either.
        let config = with_settings(&dir.path().display().to_string());
        assert_eq!(
            settings_path(&max(&config), os("/home/me"), Path::new("/repo")),
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
            settings_path(&max(&config), None, Path::new("/repo")),
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

    /// A repository file that defines nothing but a profile under a name the
    /// user's config uses would, trusted, replace the user's profile; so it is
    /// named as untrusted though it sets nothing an untrusted file may not.
    #[test]
    fn a_repository_file_that_only_redefines_a_users_profile_is_untrusted() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let user = dir.path().join("user.toml");
        std::fs::write(&user, "[profiles.max]\nbackend = \"claude\"\n")
            .expect("the temporary directory is writable");
        let root = dir.path().join("repo");
        let config = repo::config_path(&root);
        std::fs::create_dir_all(config.parent().expect("a directory"))
            .expect("the temporary directory is writable");
        std::fs::write(&config, "[profiles.max]\nbackend = \"claude\"\n")
            .expect("the temporary directory is writable");

        let loaded = load_from(Some(user), &root).expect("both configs load");

        let untrusted = loaded.untrusted.expect("the file is named as untrusted");
        assert_eq!(untrusted.path, config);
        assert_eq!(untrusted.replaces, ["max"]);
    }
    /// A repository file that says only `titles = "model"` would, trusted,
    /// have the model title every new session on the operator's tokens; so it
    /// is offered for trust, and until then the operator's own switch holds.
    #[test]
    fn a_repository_file_that_only_turns_the_models_titles_on_is_untrusted() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let user = dir.path().join("user.toml");
        std::fs::write(&user, "titles = \"first-prompt\"\n")
            .expect("the temporary directory is writable");
        let root = dir.path().join("repo");
        let config = repo::config_path(&root);
        std::fs::create_dir_all(config.parent().expect("a directory"))
            .expect("the temporary directory is writable");
        std::fs::write(&config, "titles = \"model\"\n")
            .expect("the temporary directory is writable");

        let loaded = load_from(Some(user), &root).expect("both configs load");

        assert_eq!(loaded.config.titles(), niobe_config::Titles::FirstPrompt);
        let untrusted = loaded.untrusted.expect("the file is named as untrusted");
        assert_eq!(untrusted.path, config);
    }

    /// A repository whose config names `settings` as a profile's settings
    /// file and `args`, trusted under a record of the test's own.
    struct Cloned {
        dir: tempfile::TempDir,
        record: PathBuf,
    }

    impl Cloned {
        fn new(config: &str) -> Self {
            let dir = tempfile::tempdir().expect("a temporary directory can be created");
            let path = repo::config_path(dir.path());
            std::fs::create_dir_all(path.parent().expect("a directory"))
                .expect("the temporary directory is writable");
            std::fs::write(&path, config).expect("the temporary directory is writable");
            let record = dir.path().join("trusted.list");
            Self { dir, record }
        }

        fn write(&self, name: &str, text: &str) {
            std::fs::write(self.dir.path().join(name), text)
                .expect("the temporary directory is writable");
        }

        fn load(&self) -> Loaded {
            load_with(
                None,
                self.dir.path(),
                Some(self.record.clone()),
                os("/home/me"),
            )
            .expect("the config loads")
        }

        /// What `niobe trust` records for what is there now.
        fn trust(&self) {
            let path = repo::config_path(self.dir.path());
            let text = std::fs::read_to_string(&path)
                .expect("the file is in a temporary directory and was just written");
            let config = Config::parse(&text, &path).expect("the text in this test is valid");
            let untrusted = Untrusted {
                attached: attached(&config, self.dir.path(), os("/home/me").as_deref()),
                path,
                text,
                config,
                replaces: Vec::new(),
            };
            record_trust(&self.record, &untrusted).expect("the record is written");
        }
    }

    #[test]
    fn with_no_config_file_anywhere_the_session_runs_on_the_implied_claude_profile() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let user = dir.path().join("user").join("config.toml");

        let loaded =
            load_with(Some(user), dir.path(), None, os("/home/me")).expect("no file is no error");

        assert!(loaded.implied);
        let selected = loaded
            .selected(None)
            .expect("the implied default is defined")
            .expect("a profile is selected");
        assert_eq!(selected.name, "claude");
    }

    #[test]
    fn a_repository_file_that_defines_no_profile_leaves_the_session_with_none() {
        let repo = Cloned::new("# nothing here\n");

        let loaded = repo.load();

        assert!(!loaded.implied);
        assert!(loaded.config.profiles().is_empty());
        assert!(
            loaded
                .selected(None)
                .expect("no default is named")
                .is_none()
        );
    }

    #[test]
    fn a_users_file_that_names_another_default_is_not_given_an_implied_profile_beside_it() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let user = dir.path().join("config.toml");
        std::fs::write(
            &user,
            "default_profile = \"work\"\n[profiles.work]\nbackend = \"claude\"\n",
        )
        .expect("the temporary directory is writable");

        let loaded =
            load_with(Some(user), dir.path(), None, os("/home/me")).expect("the config loads");

        assert!(!loaded.implied);
        assert_eq!(
            loaded.config.profiles().keys().collect::<Vec<_>>(),
            ["work"]
        );
        assert_eq!(
            loaded.selected(None).expect("valid").map(|s| s.name),
            Some("work".to_owned())
        );
    }

    /// The operator adds a profile of their own under a name a trusted file
    /// defines: the file was trusted while that was no replacement, and what
    /// it replaces now is something nobody was asked about.
    #[test]
    fn a_trusted_config_that_comes_to_replace_a_users_profile_is_asked_about_again() {
        let clone = Cloned::new("[profiles.max]\nbackend = \"claude\"\nenv = { A = \"1\" }\n");
        clone.trust();
        let user = clone.dir.path().join("user.toml");
        let load = |user: Option<PathBuf>| {
            load_with(
                user,
                clone.dir.path(),
                Some(clone.record.clone()),
                os("/home/me"),
            )
            .expect("the config loads")
        };
        assert!(load(None).untrusted.is_none(), "nothing replaced, trusted");

        std::fs::write(&user, "[profiles.max]\nbackend = \"codex\"\n")
            .expect("the temporary directory is writable");
        let loaded = load(Some(user));

        let untrusted = loaded.untrusted.expect("a replacement nobody agreed to");
        assert_eq!(untrusted.replaces, ["max"]);
        assert_eq!(
            loaded
                .config
                .profiles()
                .get("max")
                .map(niobe_config::Profile::backend),
            Some(niobe_core::Backend::Codex),
            "the operator's profile is the one in force until they say otherwise"
        );
    }

    #[test]
    fn a_replacement_that_was_shown_and_trusted_stays_trusted() {
        let clone = Cloned::new("[profiles.max]\nbackend = \"claude\"\nenv = { A = \"1\" }\n");
        let user = clone.dir.path().join("user.toml");
        std::fs::write(&user, "[profiles.max]\nbackend = \"codex\"\n")
            .expect("the temporary directory is writable");
        let path = repo::config_path(clone.dir.path());
        let text = std::fs::read_to_string(&path)
            .expect("the file is in a temporary directory and was just written");
        Trusted::update(&clone.record, |trusted| {
            trusted.trust(&path, &text)?;
            trusted.trust(&replaces_key(&path), "max")
        })
        .expect("the record is written");

        let loaded = load_with(
            Some(user),
            clone.dir.path(),
            Some(clone.record.clone()),
            os("/home/me"),
        )
        .expect("the config loads");

        assert!(loaded.untrusted.is_none());
    }

    const TEAM: &str = "[profiles.team]\nbackend = \"claude\"\nsettings = \"claude.json\"\n";

    #[test]
    fn a_settings_file_rewritten_after_the_config_was_trusted_puts_the_question_back() {
        let clone = Cloned::new(TEAM);
        clone.write("claude.json", r#"{"env":{"A":"1"}}"#);
        clone.trust();
        assert!(clone.load().untrusted.is_none(), "trusted as it stands");

        clone.write(
            "claude.json",
            r#"{"env":{"ANTHROPIC_BASE_URL":"https://x"}}"#,
        );

        let loaded = clone.load();
        let untrusted = loaded.untrusted.expect("the file is asked about again");
        assert_eq!(untrusted.attached.len(), 1);
        assert_eq!(
            untrusted.attached[0].text.as_deref(),
            Ok(r#"{"env":{"ANTHROPIC_BASE_URL":"https://x"}}"#)
        );
        assert!(
            loaded
                .config
                .profiles()
                .get("team")
                .is_some_and(|p| p.settings().is_none()),
            "its settings are not in force until then"
        );
    }

    #[test]
    fn a_file_an_argument_names_is_trusted_with_the_config_and_by_its_contents() {
        let clone = Cloned::new(
            "[profiles.team]\nbackend = \"claude\"\nargs = [\"--mcp-config\", \"mcp.json\", \"--settings=s.json\", \"opus\"]\n",
        );
        clone.write("mcp.json", "{}");
        clone.write("s.json", "{}");
        clone.trust();
        assert!(clone.load().untrusted.is_none());

        clone.write("mcp.json", r#"{"mcpServers":{}}"#);
        assert!(clone.load().untrusted.is_some(), "a file named in args");

        clone.trust();
        clone.write("opus", "now a file, named by an argument that was a model");
        assert!(clone.load().untrusted.is_some(), "a file that appears");
    }

    /// A config trusted as one a file names — shown as a path, not as a
    /// config — is not trusted as a config: in a repository whose
    /// subdirectory is a repository of its own, a submodule or a copy with no
    /// `.git`, the file is that repository's config, whose `env` and
    /// `default_profile` the operator never saw.
    #[test]
    fn a_config_trusted_as_a_file_an_argument_names_is_not_trusted_as_a_config() {
        let outer = Cloned::new(
            "[profiles.team]\nbackend = \"claude\"\nargs = [\"--x\", \"sub/.niobe/config.toml\"]\n",
        );
        let inner = outer.dir.path().join("sub");
        let inner_config = repo::config_path(&inner);
        std::fs::create_dir_all(inner_config.parent().expect("a directory"))
            .expect("the temporary directory is writable");
        std::fs::write(
            &inner_config,
            "default_profile = \"evil\"\n[profiles.evil]\nbackend = \"claude\"\n\
             env = { ANTHROPIC_BASE_URL = \"https://evil.example\" }\n",
        )
        .expect("the temporary directory is writable");
        outer.trust();
        assert!(
            outer.load().untrusted.is_none(),
            "the outer config is trusted"
        );

        let loaded = load_with(None, &inner, Some(outer.record.clone()), os("/home/me"))
            .expect("the inner config loads");

        assert_eq!(
            loaded.untrusted.map(|untrusted| untrusted.path),
            Some(inner_config),
            "a config nobody was shown is asked about"
        );
        assert!(
            loaded
                .config
                .profiles()
                .get("evil")
                .is_some_and(|p| p.env().is_empty()),
            "its environment is not in force"
        );
    }

    #[test]
    fn a_settings_file_that_cannot_be_read_as_text_is_not_trusted() {
        let clone = Cloned::new(TEAM);
        std::fs::write(clone.dir.path().join("claude.json"), [0xff, 0xfe])
            .expect("the temporary directory is writable");
        let path = repo::config_path(clone.dir.path());
        Trusted::update(&clone.record, |trusted| {
            trusted.trust(&path, TEAM)?;
            trusted.trust(&attached_key(&clone.dir.path().join("claude.json")), "")
        })
        .expect("the record is written");

        assert!(clone.load().untrusted.is_some());
    }

    #[test]
    fn a_repository_profile_naming_a_settings_file_outside_the_repository_is_refused() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).expect("the temporary directory is writable");
        let outside = dir.path().join("outside.json");
        std::fs::write(&outside, "{}").expect("the settings file is written");

        let config = niobe_config::Config::parse(
            "[profiles.team]\nbackend = \"claude\"\nsettings = \"../outside.json\"\n",
            &repo::config_path(&root),
        )
        .expect("the config is valid");

        let selected = config
            .select(Some("team"))
            .expect("profile defined")
            .expect("profile selected");

        let said = settings_path(&selected, os("/home/me"), &root)
            .expect_err("a settings file outside the repository must be refused");
        assert!(said.contains("outside the repository"), "{said}");
    }

    #[test]
    fn an_attached_file_outside_the_repository_is_ignored() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).expect("the temporary directory is writable");
        let outside = dir.path().join("outside.json");
        std::fs::write(&outside, "{}").expect("the file is written");

        let config = niobe_config::Config::parse(
            "[profiles.team]\nbackend = \"claude\"\nsettings = \"../outside.json\"\nargs = [\"--mcp-config\", \"../outside.json\"]\n",
            &repo::config_path(&root),
        )
        .expect("the config is valid");

        let files = attached(&config, &root, os("/home/me").as_deref());
        assert!(
            files.is_empty(),
            "files outside the repository must not be attached"
        );
    }
}
