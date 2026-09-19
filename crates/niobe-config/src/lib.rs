// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Niobe's configuration.
//!
//! A config file is TOML. It defines **profiles**, each a backend plus the
//! environment and arguments that backend runs with, and optionally names the
//! profile used when `--profile` is not given:
//!
//! ```toml
//! default_profile = "personal"
//!
//! [profiles.personal]
//! backend = "claude"
//! settings = "~/.config/niobe/claude-personal.json"
//!
//! [profiles.work]
//! backend = "claude"
//! env = { CLAUDE_CODE_USE_BEDROCK = "1", AWS_PROFILE = "example-sso", AWS_REGION = "eu-west-1" }
//! auth_refresh = "aws sso login --profile example-sso"
//!
//! [profiles.codex]
//! backend = "codex"
//! args = ["--model", "gpt-5-codex"]
//! ```
//!
//! A profile may also name the models it offers, which is what the shell lists
//! when the operator asks to switch. The names are passed to the backend as
//! written, so they are whatever that backend takes — an alias or a full id.
//!
//! A config also carries the standing answers to permission prompts — the
//! rules a session made by answering "always" — which add up across files
//! rather than replacing one another:
//!
//! ```toml
//! [permissions]
//! allow = ["Read", "Bash(cargo test)"]
//! ```
//!
//! and the palette the shell opens in:
//!
//! ```toml
//! theme = "neo"
//! ```
//!
//! Which palettes there are is the shell's business, not this crate's, so the
//! name is carried as written and the caller resolves it; [`ThemeName`] keeps
//! the line so that a name nothing answers to is reported where it was
//! written.
//!
//! A profile may name a settings file the backend is to run under, which is
//! how a machine whose CLI is configured one way runs a session the other: the
//! path is passed to the backend and nothing in it is merged or rewritten
//! here. `env` is the environment the backend is started with; `settings` is
//! the file that backend reads for itself, and where the two disagree the file
//! is what the `claude` CLI goes by.
//!
//! Profiles are what keep several real backends — a personal login, a company
//! cloud account, a second vendor's CLI — as lines of config rather than code
//! paths. The `env` of a profile is kept exactly as written: no variable is
//! expanded, added or dropped, so whatever policy a company account enforces
//! through its environment reaches the CLI intact.
//!
//! Configs are layered: [`Config::load`] reads the user's file, then the
//! repository's, and a later file overrides an earlier one (see
//! [`Config::overlay`]). Where the files live is the caller's business; this
//! crate reads the paths it is given and never looks at the environment.
//!
//! A repository's file arrives with the clone, so what it may put in front of a
//! backend — a profile's `env`, `args`, `settings` and `auth_refresh` — takes
//! effect only once the operator has read it and said so. [`trust`] is where
//! that decision is kept and [`Config::untrusted`] is the config as it applies
//! until it has been made.
//!
//! Depends on `niobe-core` and on no other workspace crate.

mod error;
mod parse;
pub mod trust;
mod write;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use niobe_core::Backend;
use niobe_core::permission::Allowlist;

pub use error::ConfigError;
pub use write::remember;

/// The file name of a config, in the user's config directory and in a
/// repository's `.niobe` directory alike.
pub const FILE_NAME: &str = "config.toml";

/// What a profile lost because the file it came from has not been trusted
/// (see [`trust`]).
///
/// The values are gone rather than hidden behind an accessor: nothing can
/// reach an untrusted environment by asking for it a different way. What is
/// left is the names, which is what a listing shows and what tells the
/// operator what trusting the file would put in force.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Withheld {
    /// The variables the file set, by name. No value is kept.
    pub env: Vec<String>,
    /// Whether the file gave the backend arguments of its own.
    pub args: bool,
    /// Whether the file gave a credential refresh command.
    pub auth_refresh: bool,
    /// Whether the file named a settings file for the backend to run under.
    pub settings: bool,
}

/// A settings file a profile starts its backend under, and where it was
/// written.
///
/// The path is kept exactly as the config wrote it, `~` and all: expanding one
/// means reading the environment, and this crate reads none. The line is kept
/// because a path that is not there is the operator's to fix in the file, and
/// they fix it at that line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    path: String,
    line: usize,
}

impl Settings {
    /// The path, as the config wrote it.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The line it was written on, counted from 1.
    pub fn line(&self) -> usize {
        self.line
    }
}

/// A backend plus the environment, arguments and credential refresh it runs
/// with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    backend: Backend,
    env: BTreeMap<String, String>,
    args: Vec<String>,
    models: Vec<String>,
    settings: Option<Settings>,
    auth_refresh: Option<String>,
    source: PathBuf,
    withheld: Option<Withheld>,
}

impl Profile {
    /// The engine this profile runs.
    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// The variables set for the backend, exactly as the config wrote them.
    /// Every name is non-empty and holds no `=` and no NUL, and no value holds
    /// a NUL, so each can be set in a child process's environment.
    ///
    /// Empty for a profile from a file that has not been trusted; what it set
    /// is named by [`Profile::withheld`].
    pub fn env(&self) -> &BTreeMap<String, String> {
        &self.env
    }

    /// Arguments passed to the backend after the ones Niobe passes itself.
    ///
    /// Empty for a profile from a file that has not been trusted.
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// The models this profile offers, in the order the shell lists them for
    /// picking. Empty where the config names none, which is a profile whose
    /// model is whatever the backend chooses: Niobe never invents a model id.
    pub fn models(&self) -> &[String] {
        &self.models
    }

    /// The settings file the backend is to run under, where the profile names
    /// one. What that file means is the backend's own business: the `claude`
    /// CLI reads an environment, a model and hooks out of it, and reads it in
    /// front of the settings it would otherwise use, which is what lets one
    /// machine hold a profile per account.
    ///
    /// `None` for a profile from a file that has not been trusted.
    pub fn settings(&self) -> Option<&Settings> {
        self.settings.as_ref()
    }

    /// A shell command that renews the backend's credentials when they have
    /// expired, such as a single sign-on login. Never empty when present.
    ///
    /// `None` for a profile from a file that has not been trusted.
    pub fn auth_refresh(&self) -> Option<&str> {
        self.auth_refresh.as_deref()
    }

    /// The config file that defined this profile.
    pub fn source(&self) -> &Path {
        &self.source
    }

    /// What this profile lost because the file it came from has not been
    /// trusted, where anything was.
    pub fn withheld(&self) -> Option<&Withheld> {
        self.withheld.as_ref()
    }

    /// Whether anything this profile carries takes effect only once the file
    /// it came from has been trusted — before the withholding and after it
    /// alike, so a file can still be named as one worth trusting once its
    /// values are gone.
    fn needs_trust(&self) -> bool {
        self.withheld.is_some()
            || !self.env.is_empty()
            || !self.args.is_empty()
            || self.settings.is_some()
            || self.auth_refresh.is_some()
    }

    /// Drops what an untrusted file may not put in front of a backend, keeping
    /// the names of it.
    fn withhold(&mut self) {
        if self.withheld.is_some()
            || (self.env.is_empty()
                && self.args.is_empty()
                && self.settings.is_none()
                && self.auth_refresh.is_none())
        {
            return;
        }
        self.withheld = Some(Withheld {
            env: std::mem::take(&mut self.env).into_keys().collect(),
            args: !std::mem::take(&mut self.args).is_empty(),
            auth_refresh: self.auth_refresh.take().is_some(),
            settings: self.settings.take().is_some(),
        });
    }
}

/// The name a config gives the profile used when none is asked for, and where
/// it was set, so that a name no config defines can be reported at its line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DefaultProfile {
    name: String,
    path: PathBuf,
    line: usize,
}

/// The name a config gives the palette the shell opens in, and where it was
/// set.
///
/// The name is kept as written rather than resolved: the palettes live in the
/// TUI, which this crate cannot name. [`ThemeName::invalid`] is how the caller
/// that can resolve it reports one that nothing answers to, at the line the
/// operator wrote it on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeName {
    name: String,
    path: PathBuf,
    line: usize,
}

impl ThemeName {
    /// The name, as the config wrote it.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Reports `message` against this `theme`, at its key and its line.
    pub fn invalid(&self, message: &str) -> ConfigError {
        ConfigError::Invalid {
            path: self.path.clone(),
            line: self.line,
            key: Some("theme".to_owned()),
            message: message.to_owned(),
        }
    }
}

/// One config file, or several laid over one another.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    profiles: BTreeMap<String, Profile>,
    default_profile: Option<DefaultProfile>,
    theme: Option<ThemeName>,
    allowed: Allowlist,
}

/// The profile a session runs under, with the name it was selected by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selected<'a> {
    /// The profile's name, as the config spells it.
    pub name: &'a str,
    /// The profile.
    pub profile: &'a Profile,
}

impl Selected<'_> {
    /// Reports `message` against `settings`, at the key and the line this
    /// profile wrote it on, so that a path that cannot be used is fixed where
    /// it was written.
    ///
    /// Whether it can be used is the caller's to find out: the path is written
    /// the way a shell would take it, `~` and all, and expanding one means
    /// reading an environment this crate does not read. The settings are
    /// handed back rather than read off the profile again, so that there is no
    /// line to invent for a profile that named no file.
    pub fn settings_invalid(&self, settings: &Settings, message: &str) -> ConfigError {
        ConfigError::Invalid {
            path: self.profile.source.clone(),
            line: settings.line,
            key: Some(parse::settings_key(self.name)),
            message: message.to_owned(),
        }
    }
}

impl Config {
    /// Parses the text of the config file at `path`. `path` is only used to
    /// say where a profile came from and where an error is.
    pub fn parse(text: &str, path: &Path) -> Result<Self, ConfigError> {
        parse::config(text, path)
    }

    /// Reads and parses the config file at `path`. A file that does not exist
    /// is no config at all, which is not an error: most repositories have none.
    pub fn read(path: &Path) -> Result<Option<Self>, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text, path).map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(ConfigError::Read {
                path: path.to_path_buf(),
                error,
            }),
        }
    }

    /// Reads the config files at `paths`, lowest precedence first, and lays
    /// each over the ones before it. Files that do not exist are skipped.
    pub fn load(paths: &[&Path]) -> Result<Self, ConfigError> {
        let mut config = Self::default();
        for path in paths {
            if let Some(layer) = Self::read(path)? {
                config = config.overlay(layer);
            }
        }
        Ok(config)
    }

    /// This config with `over` laid on top of it.
    ///
    /// A profile `over` defines replaces the profile of the same name whole,
    /// rather than key by key: what a file says a profile is, is all of that
    /// profile, and a repository cannot quietly inherit the environment of a
    /// user's profile by reusing its name. A `default_profile` in `over` wins.
    #[must_use]
    pub fn overlay(mut self, over: Self) -> Self {
        self.profiles.extend(over.profiles);
        if over.default_profile.is_some() {
            self.default_profile = over.default_profile;
        }
        if over.theme.is_some() {
            self.theme = over.theme;
        }
        // Permissions add up rather than replacing one another: a rule is a
        // permission the operator granted, and a repository's file is not
        // where one is taken back.
        self.allowed = self.allowed.merge(over.allowed);
        self
    }

    /// This config as it applies while the file it came from has not been
    /// trusted: every profile keeps its backend and the models it offers, and
    /// loses its `env`, its `args`, its `settings` and its `auth_refresh`
    /// (see [`trust`]).
    ///
    /// Applied to the layer, before it is laid over anything, so that a
    /// profile the repository replaces cannot end up holding half of the
    /// user's.
    #[must_use]
    pub fn untrusted(mut self) -> Self {
        for profile in self.profiles.values_mut() {
            profile.withhold();
        }
        self
    }

    /// Whether anything in this config takes effect only once the file it came
    /// from has been trusted, which is what makes trusting worth asking about.
    /// True after [`Config::untrusted`] as well as before it.
    pub fn needs_trust(&self) -> bool {
        self.profiles.values().any(Profile::needs_trust)
    }

    /// The standing answers to permission prompts, from every file laid over
    /// the ones before it.
    pub fn allowed(&self) -> &Allowlist {
        &self.allowed
    }

    /// Every profile, by name.
    pub fn profiles(&self) -> &BTreeMap<String, Profile> {
        &self.profiles
    }

    /// The palette the shell opens in, where a config names one.
    ///
    /// Not gated on trust: a palette is what the shell draws in and nothing a
    /// backend is started with, so a repository that sets one has changed some
    /// colours and no more.
    pub fn theme(&self) -> Option<&ThemeName> {
        self.theme.as_ref()
    }

    /// The profile a session runs under: the one `requested` names, or else
    /// the config's `default_profile`, or else none.
    ///
    /// A requested name no config defines is an error that lists the ones it
    /// does; a `default_profile` no config defines is an error at the line
    /// that set it. Neither falls back to running without a profile, which
    /// would put the session on whatever credentials the CLI finds by itself.
    pub fn select(&self, requested: Option<&str>) -> Result<Option<Selected<'_>>, ConfigError> {
        let name = match (requested, &self.default_profile) {
            (Some(name), _) => name,
            (None, Some(default)) if !self.profiles.contains_key(&default.name) => {
                return Err(ConfigError::Invalid {
                    path: default.path.clone(),
                    line: default.line,
                    key: Some("default_profile".to_owned()),
                    message: format!("names `{}`, which no config defines", default.name),
                });
            }
            (None, Some(default)) => &default.name,
            (None, None) => return Ok(None),
        };

        match self.profiles.get_key_value(name) {
            Some((name, profile)) => Ok(Some(Selected { name, profile })),
            None => Err(ConfigError::UnknownProfile {
                name: name.to_owned(),
                defined: self.profiles.keys().cloned().collect(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(name: &str) -> PathBuf {
        PathBuf::from(format!("/configs/{name}/config.toml"))
    }

    fn parsed(text: &str) -> Config {
        Config::parse(text, &path("user")).expect("the config is valid")
    }

    /// The error for `text`, as the operator reads it.
    fn invalid(text: &str) -> String {
        Config::parse(text, &path("user"))
            .expect_err("the config is invalid")
            .to_string()
    }

    const EXAMPLE: &str = r#"
default_profile = "personal"

[profiles.personal]
backend = "claude"

[profiles.work]
backend = "claude"
env = { CLAUDE_CODE_USE_BEDROCK = "1", AWS_PROFILE = "example-sso", AWS_REGION = "eu-west-1" }
auth_refresh = "aws sso login --profile example-sso"

[profiles.codex]
backend = "codex"
args = ["--model", "gpt-5-codex"]
"#;

    #[test]
    fn a_profile_is_a_backend_plus_an_environment_arguments_and_a_refresh_command() {
        let config = parsed(EXAMPLE);

        assert_eq!(
            config.profiles().keys().collect::<Vec<_>>(),
            ["codex", "personal", "work"]
        );

        let work = &config.profiles()["work"];
        assert_eq!(work.backend(), Backend::Claude);
        assert_eq!(
            work.env().iter().collect::<Vec<_>>(),
            [
                (&"AWS_PROFILE".to_owned(), &"example-sso".to_owned()),
                (&"AWS_REGION".to_owned(), &"eu-west-1".to_owned()),
                (&"CLAUDE_CODE_USE_BEDROCK".to_owned(), &"1".to_owned()),
            ]
        );
        assert_eq!(
            work.auth_refresh(),
            Some("aws sso login --profile example-sso")
        );
        assert!(work.args().is_empty());
        assert_eq!(work.source(), path("user"));

        let codex = &config.profiles()["codex"];
        assert_eq!(codex.backend(), Backend::Codex);
        assert_eq!(codex.args(), ["--model", "gpt-5-codex"]);
        assert!(codex.env().is_empty());
        assert_eq!(codex.auth_refresh(), None);
    }

    #[test]
    fn the_environment_passes_through_exactly_as_written() {
        let config = parsed(
            r#"
[profiles.p]
backend = "claude"
env = { HOME_COPY = "$HOME", TILDE = "~/x", SPACES = "  padded  ", EMPTY = "", "lower.dotted" = "ok" }
"#,
        );
        let env = config.profiles()["p"].env();

        assert_eq!(env["HOME_COPY"], "$HOME");
        assert_eq!(env["TILDE"], "~/x");
        assert_eq!(env["SPACES"], "  padded  ");
        assert_eq!(env["EMPTY"], "");
        assert_eq!(env["lower.dotted"], "ok");
        assert_eq!(env.len(), 5);
    }

    #[test]
    fn the_models_a_profile_offers_are_kept_in_the_order_they_were_written() {
        let config = parsed(
            "[profiles.max]\nbackend = \"claude\"\nmodels = [\"opus\", \"sonnet\", \"haiku\"]\n",
        );

        assert_eq!(
            config.profiles()["max"].models(),
            ["opus", "sonnet", "haiku"]
        );
        assert!(
            parsed(EXAMPLE).profiles()["personal"].models().is_empty(),
            "a profile that names no model was given one"
        );
    }

    #[test]
    fn a_models_list_that_is_not_a_list_of_names_is_reported_at_its_line() {
        assert_eq!(
            invalid("[profiles.max]\nbackend = \"claude\"\nmodels = \"opus\"\n"),
            "/configs/user/config.toml:3: profiles.max.models: expected an array of strings, found a string"
        );
        assert_eq!(
            invalid("[profiles.max]\nbackend = \"claude\"\nmodels = [\"opus\", \"  \"]\n"),
            "/configs/user/config.toml:3: profiles.max.models[1]: is empty"
        );
    }

    #[test]
    fn a_profile_can_name_the_settings_file_its_backend_runs_under() {
        let config = parsed(
            "[profiles.max]\nbackend = \"claude\"\n\nsettings = \"~/.config/niobe/claude-personal.json\"\n",
        );

        let settings = config.profiles()["max"]
            .settings()
            .expect("the profile names a settings file");
        assert_eq!(settings.path(), "~/.config/niobe/claude-personal.json");
        // The line, because a path that is not there is reported at it.
        assert_eq!(settings.line(), 4);
        assert_eq!(parsed(EXAMPLE).profiles()["personal"].settings(), None);
    }

    #[test]
    fn a_settings_path_that_is_not_there_is_reported_at_its_key_and_its_line() {
        let config =
            parsed("[profiles.max]\nbackend = \"claude\"\nsettings = \"~/nowhere.json\"\n");
        let selected = config
            .select(Some("max"))
            .expect("the profile is defined")
            .expect("a profile was selected");

        assert_eq!(
            selected
                .settings_invalid(
                    selected.profile.settings().expect("a settings file"),
                    "no such file: /home/me/nowhere.json"
                )
                .to_string(),
            "/configs/user/config.toml:3: profiles.max.settings: no such file: /home/me/nowhere.json"
        );
    }

    #[test]
    fn a_settings_path_that_says_nothing_is_reported_at_its_line() {
        assert_eq!(
            invalid("[profiles.max]\nbackend = \"claude\"\nsettings = \"  \"\n"),
            "/configs/user/config.toml:3: profiles.max.settings: is empty"
        );
        assert_eq!(
            invalid("[profiles.max]\nbackend = \"claude\"\nsettings = 1\n"),
            "/configs/user/config.toml:3: profiles.max.settings: expected a string, found an integer"
        );
    }

    #[test]
    fn an_empty_file_is_a_config_with_nothing_in_it() {
        assert_eq!(parsed(""), Config::default());
        assert_eq!(parsed("# only a comment\n"), Config::default());
    }

    #[test]
    fn standing_answers_are_read_as_rules_and_add_up_across_files() {
        let user = Config::parse(
            "[permissions]\nallow = [\"Read\", \"Bash(cargo test)\"]\n",
            &path("user"),
        )
        .expect("the config is valid");
        let repo = Config::parse(
            "[permissions]\nallow = [\"Bash(cargo test)\", \"Edit(crates/*)\"]\n",
            &path("repo"),
        )
        .expect("the config is valid");

        let merged = user.overlay(repo);

        assert_eq!(
            merged
                .allowed()
                .rules()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["Read", "Bash(cargo test)", "Edit(crates/*)"]
        );
        assert!(
            merged
                .allowed()
                .allows("Edit", Some("crates/niobe-tui/a.rs"))
        );
        assert!(!merged.allowed().allows("Edit", Some("xtask/a.rs")));
    }

    #[test]
    fn an_untrusted_file_sets_no_environment_no_arguments_and_no_refresh_command() {
        let config = parsed(EXAMPLE).untrusted();

        let work = &config.profiles()["work"];
        assert!(work.env().is_empty());
        assert_eq!(work.auth_refresh(), None);
        // What it is, and what it offers, are not what a clone can abuse.
        assert_eq!(work.backend(), Backend::Claude);

        let withheld = work
            .withheld()
            .expect("the work profile set an environment");
        assert_eq!(
            withheld.env,
            ["AWS_PROFILE", "AWS_REGION", "CLAUDE_CODE_USE_BEDROCK"]
        );
        assert!(withheld.auth_refresh);
        assert!(!withheld.args);

        let codex = &config.profiles()["codex"];
        assert!(codex.args().is_empty());
        assert_eq!(
            codex.withheld().expect("the codex profile set arguments"),
            &Withheld {
                env: Vec::new(),
                args: true,
                auth_refresh: false,
                settings: false,
            }
        );
    }

    #[test]
    fn an_untrusted_file_names_no_settings_file_for_a_backend_to_run_under() {
        // A settings file is where the official CLI is told what to sign in
        // as, so a clone naming one is a clone choosing the credentials.
        let config =
            parsed("[profiles.repo]\nbackend = \"claude\"\nsettings = \"./theirs.json\"\n");
        assert!(config.needs_trust());

        let config = config.untrusted();
        let repo = &config.profiles()["repo"];

        assert_eq!(repo.settings(), None);
        assert_eq!(
            repo.withheld().expect("the profile named a settings file"),
            &Withheld {
                env: Vec::new(),
                args: false,
                auth_refresh: false,
                settings: true,
            }
        );
        assert!(config.needs_trust());
    }

    #[test]
    fn a_profile_that_puts_nothing_in_front_of_a_backend_withholds_nothing() {
        let config = parsed("[profiles.plain]\nbackend = \"claude\"\nmodels = [\"opus\"]\n");
        assert!(!config.needs_trust());

        let config = config.untrusted();
        let plain = &config.profiles()["plain"];

        assert_eq!(plain.withheld(), None);
        assert_eq!(plain.models(), ["opus"]);
        assert!(!config.needs_trust(), "nothing here is worth trusting");
    }

    #[test]
    fn a_file_still_says_it_is_worth_trusting_once_its_values_are_gone() {
        let config = parsed(EXAMPLE);
        assert!(config.needs_trust());
        assert!(config.clone().untrusted().needs_trust());
        // Withholding twice must not report that nothing was ever withheld.
        assert!(config.untrusted().untrusted().needs_trust());
    }

    #[test]
    fn permissions_and_the_default_profile_are_not_what_trust_gates() {
        let config = parsed(
            "default_profile = \"p\"\n[profiles.p]\nbackend = \"claude\"\n\
             \n[permissions]\nallow = [\"Read\"]\n",
        )
        .untrusted();

        assert_eq!(
            config.select(None).expect("valid").map(|s| s.name),
            Some("p")
        );
        assert!(config.allowed().allows("Read", None));
    }

    #[test]
    fn a_rule_that_is_not_one_is_reported_at_its_line() {
        let said = invalid("[permissions]\nallow = [\n  \"Read\",\n  \"Bash(\",\n]\n");

        assert!(said.starts_with("/configs/user/config.toml:4:"), "{said}");
        assert!(said.contains("permissions.allow[1]"), "{said}");
        assert!(said.contains("is not a permission rule"), "{said}");
    }

    #[test]
    fn a_config_with_no_permissions_allows_nothing_by_itself() {
        assert!(parsed(EXAMPLE).allowed().is_empty());
    }

    #[test]
    fn repo_config_overrides_user_config() {
        let user = Config::parse(EXAMPLE, &path("user")).expect("valid");
        let repo = Config::parse(
            r#"
default_profile = "work"

[profiles.work]
backend = "codex"
"#,
            &path("repo"),
        )
        .expect("valid");

        let config = user.overlay(repo);

        let work = &config.profiles()["work"];
        assert_eq!(work.backend(), Backend::Codex);
        assert_eq!(work.source(), path("repo"));
        assert!(
            work.env().is_empty(),
            "a profile the repo defines replaces the user's whole, env included"
        );
        assert_eq!(config.profiles()["personal"].source(), path("user"));
        let selected = config.select(None).expect("valid").expect("a default");
        assert_eq!(selected.name, "work");
    }

    #[test]
    fn a_layer_without_a_default_keeps_the_one_beneath_it() {
        let user = parsed(EXAMPLE);
        let repo = Config::parse("[profiles.extra]\nbackend = \"native\"\n", &path("repo"))
            .expect("valid");

        let config = user.overlay(repo);

        assert_eq!(
            config.select(None).expect("valid").map(|s| s.name),
            Some("personal")
        );
        assert_eq!(config.profiles().len(), 4);
    }

    #[test]
    fn a_requested_profile_wins_over_the_default() {
        let config = parsed(EXAMPLE);
        let selected = config
            .select(Some("work"))
            .expect("valid")
            .expect("selected");
        assert_eq!(selected.name, "work");
        assert_eq!(selected.profile, &config.profiles()["work"]);
    }

    #[test]
    fn without_a_request_or_a_default_no_profile_is_selected() {
        let config = parsed("[profiles.only]\nbackend = \"claude\"\n");
        assert_eq!(config.select(None).expect("valid"), None);
        assert_eq!(Config::default().select(None).expect("valid"), None);
    }

    #[test]
    fn a_requested_profile_that_is_not_defined_lists_the_ones_that_are() {
        let error = parsed(EXAMPLE)
            .select(Some("wrok"))
            .expect_err("no such profile")
            .to_string();
        assert_eq!(
            error,
            "no profile named `wrok`; the profiles defined are `codex`, `personal`, `work`"
        );

        let error = Config::default()
            .select(Some("work"))
            .expect_err("no profiles at all")
            .to_string();
        assert_eq!(
            error,
            "no profile named `work`: no config defines a profile"
        );
    }

    #[test]
    fn a_default_profile_that_is_not_defined_is_reported_where_it_was_set() {
        let config = parsed("\n\ndefault_profile = \"missing\"\n");
        let error = config.select(None).expect_err("undefined").to_string();
        assert_eq!(
            error,
            "/configs/user/config.toml:3: default_profile: names `missing`, which no config defines"
        );
        assert!(
            config.select(Some("missing")).is_err(),
            "a request for it fails too"
        );
    }

    #[test]
    fn a_theme_is_carried_as_written_with_the_line_it_was_written_on() {
        let config = parsed("\n\n\n\ntheme = \"neo\"\n\n[profiles.p]\nbackend = \"claude\"\n");
        let theme = config.theme().expect("the config names a theme");

        assert_eq!(theme.name(), "neo");
        assert_eq!(
            theme.invalid("is not a theme").to_string(),
            "/configs/user/config.toml:5: theme: is not a theme"
        );
        assert_eq!(parsed("[profiles.p]\nbackend = \"claude\"\n").theme(), None);
    }

    #[test]
    fn a_theme_is_not_what_trust_gates_and_the_last_file_to_name_one_wins() {
        // A palette is what the shell draws in: a repository that sets one has
        // changed some colours and started nothing.
        let user = parsed("theme = \"classic\"\n").untrusted();
        assert_eq!(user.theme().map(ThemeName::name), Some("classic"));
        assert!(!user.needs_trust());

        let repo = Config::parse("theme = \"neo\"\n", &path("repo")).expect("valid");
        let config = user.clone().overlay(repo);
        assert_eq!(config.theme().map(ThemeName::name), Some("neo"));

        // A file that names none leaves the one before it standing.
        let quiet =
            Config::parse("[profiles.p]\nbackend = \"claude\"\n", &path("repo")).expect("valid");
        assert_eq!(
            user.overlay(quiet).theme().map(ThemeName::name),
            Some("classic")
        );
    }

    #[test]
    fn a_theme_that_says_nothing_is_a_mistake_in_the_file() {
        assert_eq!(
            invalid("theme = \"  \"\n"),
            "/configs/user/config.toml:1: theme: is empty"
        );
        assert_eq!(
            invalid("theme = 3\n"),
            "/configs/user/config.toml:1: theme: expected a string, found an integer"
        );
    }

    #[test]
    fn a_default_set_by_one_file_may_name_a_profile_another_defines() {
        let user = parsed("[profiles.work]\nbackend = \"claude\"\n");
        let repo = Config::parse("default_profile = \"work\"\n", &path("repo")).expect("valid");
        let config = user.overlay(repo);
        assert_eq!(
            config.select(None).expect("valid").map(|s| s.name),
            Some("work")
        );
    }

    #[test]
    fn an_unknown_backend_names_the_key_the_line_and_the_backends_there_are() {
        let error = invalid("[profiles.work]\n\nbackend = \"clade\"\n");
        assert_eq!(
            error,
            "/configs/user/config.toml:3: profiles.work.backend: `clade` is not a backend; \
             expected `claude`, `codex` or `native`"
        );
    }

    #[test]
    fn a_value_of_the_wrong_type_names_the_key_and_the_line() {
        assert_eq!(
            invalid("[profiles.work]\nbackend = 1\n"),
            "/configs/user/config.toml:2: profiles.work.backend: expected a string, found an integer"
        );
        assert_eq!(
            invalid("[profiles.work]\nbackend = \"claude\"\nargs = \"--verbose\"\n"),
            "/configs/user/config.toml:3: profiles.work.args: expected an array of strings, found a string"
        );
        assert_eq!(
            invalid("[profiles.work]\nbackend = \"claude\"\nargs = [\"-v\", 2]\n"),
            "/configs/user/config.toml:3: profiles.work.args[1]: expected a string, found an integer"
        );
        assert_eq!(
            invalid(
                "[profiles.work]\nbackend = \"claude\"\n\n[profiles.work.env]\nAWS_REGION = true\n"
            ),
            "/configs/user/config.toml:5: profiles.work.env.AWS_REGION: expected a string, found a boolean"
        );
        assert_eq!(
            invalid("default_profile = [\"a\"]\n"),
            "/configs/user/config.toml:1: default_profile: expected a string, found an array"
        );
        assert_eq!(
            invalid("profiles = \"work\"\n"),
            "/configs/user/config.toml:1: profiles: expected a table, found a string"
        );
        assert_eq!(
            invalid("[profiles]\nwork = \"claude\"\n"),
            "/configs/user/config.toml:2: profiles.work: expected a table, found a string"
        );
    }

    #[test]
    fn an_unknown_key_is_an_error_rather_than_ignored() {
        // A misspelt `env` silently ignored would run a company profile on the
        // operator's personal login.
        assert_eq!(
            invalid("[profiles.work]\nbackend = \"claude\"\nenviron = { AWS_PROFILE = \"x\" }\n"),
            "/configs/user/config.toml:3: profiles.work.environ: unknown key; \
             expected `backend`, `env`, `args`, `models`, `settings` or `auth_refresh`"
        );
        assert_eq!(
            invalid("\ndefault = \"work\"\n"),
            "/configs/user/config.toml:2: default: unknown key; \
             expected `default_profile`, `theme`, `profiles` or `permissions`"
        );
    }

    #[test]
    fn a_profile_without_a_backend_is_reported_at_its_name() {
        assert_eq!(
            invalid("[profiles.work]\nenv = { A = \"1\" }\n"),
            "/configs/user/config.toml:1: profiles.work: no `backend`; \
             expected `claude`, `codex` or `native`"
        );
    }

    #[test]
    fn empty_strings_where_a_name_or_a_command_is_needed_are_rejected() {
        assert_eq!(
            invalid("default_profile = \"\"\n"),
            "/configs/user/config.toml:1: default_profile: is empty"
        );
        assert_eq!(
            invalid("[profiles.work]\nbackend = \"claude\"\nauth_refresh = \"  \"\n"),
            "/configs/user/config.toml:3: profiles.work.auth_refresh: is empty"
        );
        assert_eq!(
            invalid("[profiles.\"\"]\nbackend = \"claude\"\n"),
            "/configs/user/config.toml:1: profiles.\"\": a profile name cannot be empty"
        );
    }

    #[test]
    fn a_variable_that_cannot_be_set_in_an_environment_is_rejected() {
        assert_eq!(
            invalid("[profiles.p]\nbackend = \"claude\"\nenv = { \"A=B\" = \"1\" }\n"),
            "/configs/user/config.toml:3: profiles.p.env.\"A=B\": \
             a variable name cannot contain `=`"
        );
        assert_eq!(
            invalid("[profiles.p]\nbackend = \"claude\"\nenv = { \"\" = \"1\" }\n"),
            "/configs/user/config.toml:3: profiles.p.env.\"\": a variable name cannot be empty"
        );
        assert_eq!(
            invalid("[profiles.p]\nbackend = \"claude\"\nenv = { A = \"x\\u0000y\" }\n"),
            "/configs/user/config.toml:3: profiles.p.env.A: a value cannot contain a NUL character"
        );
    }

    #[test]
    fn a_syntax_error_names_the_line() {
        let error = invalid("[profiles.work]\nbackend = \"claude\"\nenv = { A = \"1\" \n");
        assert!(
            error.starts_with("/configs/user/config.toml:3: "),
            "{error}"
        );
        assert!(!error.contains("TOML parse error"), "{error}");

        let error = invalid("[profiles.work]\nbackend = \"claude\"\nbackend = \"codex\"\n");
        assert!(
            error.starts_with("/configs/user/config.toml:3: "),
            "{error}"
        );
    }

    #[test]
    fn the_first_error_in_the_file_is_the_one_reported() {
        // The parsed tables are sorted by key; the report follows the file.
        let error = invalid("[profiles.zeta]\nbackend = 1\n\n[profiles.alpha]\nbackend = 2\n");
        assert!(error.contains(":2: profiles.zeta.backend"), "{error}");
    }

    #[test]
    fn reading_a_file_that_does_not_exist_is_no_config() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let missing = dir.path().join(FILE_NAME);
        assert_eq!(Config::read(&missing).expect("absence is fine"), None);
        assert_eq!(
            Config::load(&[&missing, &missing]).expect("absence is fine"),
            Config::default()
        );
    }

    #[test]
    fn loading_lays_later_files_over_earlier_ones() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let user = dir.path().join("user.toml");
        let repo = dir.path().join("repo.toml");
        std::fs::write(&user, EXAMPLE).expect("the user config is written");
        std::fs::write(&repo, "[profiles.personal]\nbackend = \"native\"\n")
            .expect("the repo config is written");

        let config = Config::load(&[&user, &repo]).expect("both are valid");

        let personal = &config.profiles()["personal"];
        assert_eq!(personal.backend(), Backend::Native);
        assert_eq!(personal.source(), repo);
        assert_eq!(config.profiles()["work"].source(), user);
    }

    #[test]
    fn an_unreadable_config_names_the_file() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        // A directory where the file should be cannot be read as one.
        let error = Config::read(dir.path())
            .expect_err("a directory is not a file")
            .to_string();
        assert!(
            error.starts_with(&format!("cannot read {}", dir.path().display())),
            "{error}"
        );
    }

    #[test]
    fn a_file_that_is_invalid_stops_the_load() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let user = dir.path().join("user.toml");
        std::fs::write(&user, "[profiles.work]\nbackend = \"clade\"\n")
            .expect("the config is written");

        let error = Config::load(&[&user]).expect_err("invalid").to_string();
        assert!(
            error.starts_with(&format!("{}:2: profiles.work.backend:", user.display())),
            "{error}"
        );
    }
}
