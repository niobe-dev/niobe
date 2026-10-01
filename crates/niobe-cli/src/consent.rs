// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! What a session asks before it starts, where the repository's config sets
//! what only a trusted file may, and how the answer is recorded.
//!
//! The question shows what trusting the file would put in force, values
//! included. `niobe profiles` names a variable without its value because a
//! listing is what gets pasted into an issue; this is the one place the
//! operator decides whether to let the file start a backend, and
//! `ANTHROPIC_BASE_URL` is harmless or not by what it is set to.

use std::path::{Path, PathBuf};

use niobe_config::Config;
use niobe_config::trust::Trusted;
use niobe_tui::trust::Question;

use crate::config::{self, Untrusted};

/// The question about `untrusted`: the file, and a row for each thing in it
/// that takes effect only once it is trusted.
pub fn question(untrusted: &Untrusted) -> Question {
    Question {
        path: untrusted.path.display().to_string(),
        grants: grants(&untrusted.config, &untrusted.replaces),
    }
}

/// What `config`, read whole, sets that takes effect only once it is trusted:
/// its `[permissions]` rules, its `default_profile`, and for each profile what
/// it starts the backend with, how it says the account is billed, and whether
/// it replaces one of `replaces`, the user's profiles.
fn grants(config: &Config, replaces: &[String]) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    let rules: Vec<String> = config
        .allowed()
        .rules()
        .iter()
        .map(ToString::to_string)
        .collect();
    if !rules.is_empty() {
        rows.push(row("permissions", format!("allow {}", rules.join(", "))));
    }
    // What the file names as the default, read the way the file's own
    // withholding reads it, so that the two cannot disagree on what it is.
    if let Some((name, _)) = config.clone().untrusted().withheld_default() {
        rows.push(row("default_profile", name.to_owned()));
    }
    for (name, profile) in config.profiles() {
        if replaces.contains(name) {
            rows.push(row(name, "replaces your profile of this name".to_owned()));
        }
        if !profile.env().is_empty() {
            let env: Vec<String> = profile
                .env()
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect();
            rows.push(row(name, format!("env {}", env.join(", "))));
        }
        if !profile.args().is_empty() {
            rows.push(row(name, format!("args {}", profile.args().join(" "))));
        }
        if let Some(settings) = profile.settings() {
            rows.push(row(name, format!("settings {}", settings.path())));
        }
        if let Some(command) = profile.auth_refresh() {
            rows.push(row(name, format!("auth_refresh {command}")));
        }
        if let Some(billing) = profile.billing() {
            rows.push(row(name, format!("billing {}", billing.as_str())));
        }
    }
    rows
}

fn row(name: &str, value: String) -> (String, String) {
    (name.to_owned(), value)
}

/// Records `path`, holding `text`, as a config the operator has read, so that
/// what it sets is in force from the next time it is loaded.
///
/// What is trusted is what the operator could read and agree to: a file that
/// is not a config says nothing of the kind, and is refused.
pub fn record(path: &Path, text: &str) -> Result<(), String> {
    Config::parse(text, path)
        .map_err(|error| format!("{} is not trusted: {error}", path.display()))?;
    Trusted::update(&record_path()?, |trusted| trusted.trust(path, text)).map_err(|e| e.to_string())
}

/// Where the record of what this machine has trusted is kept.
pub fn record_path() -> Result<PathBuf, String> {
    config::trust_path().ok_or_else(|| {
        "nowhere to keep the record: neither XDG_CONFIG_HOME nor HOME names a directory of \
         your own"
            .to_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPO: &str = r#"
default_profile = "work"

[permissions]
allow = ["Edit", "Bash(cargo test)"]

[profiles.work]
backend = "claude"
models = ["opus"]
env = { ANTHROPIC_BASE_URL = "https://proxy.example", AWS_PROFILE = "work-sso" }
args = ["--model", "opus"]
settings = "./claude.json"
auth_refresh = "aws sso login"
billing = "metered"

[profiles.max]
backend = "claude"
"#;

    fn untrusted(replaces: &[&str]) -> Untrusted {
        let path = PathBuf::from("/r/.niobe/config.toml");
        Untrusted {
            config: Config::parse(REPO, &path).expect("the config is valid"),
            text: REPO.to_owned(),
            path,
            replaces: replaces.iter().map(|name| (*name).to_owned()).collect(),
        }
    }

    #[test]
    fn the_question_shows_everything_trusting_would_put_in_force_with_its_value() {
        let question = question(&untrusted(&["max"]));

        assert_eq!(question.path, "/r/.niobe/config.toml");
        let rows: Vec<(&str, &str)> = question
            .grants
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                ("permissions", "allow Edit, Bash(cargo test)"),
                ("default_profile", "work"),
                ("max", "replaces your profile of this name"),
                (
                    "work",
                    "env ANTHROPIC_BASE_URL=https://proxy.example, AWS_PROFILE=work-sso"
                ),
                ("work", "args --model opus"),
                ("work", "settings ./claude.json"),
                ("work", "auth_refresh aws sso login"),
                ("work", "billing metered"),
            ]
        );
    }

    #[test]
    fn a_profile_that_sets_only_its_backend_and_models_is_not_a_row() {
        let question = question(&untrusted(&[]));

        assert!(
            question.grants.iter().all(|(name, _)| name != "max"),
            "{:?}",
            question.grants
        );
        assert!(
            question
                .grants
                .iter()
                .all(|(_, value)| !value.contains("opus\"")),
            "{:?}",
            question.grants
        );
    }

    #[test]
    fn a_file_that_is_not_a_config_is_not_recorded_as_trusted() {
        let refused = record(
            Path::new("/r/.niobe/config.toml"),
            "[profiles.x]\nbackend = 3\n",
        )
        .expect_err("the text is not a config");

        assert!(refused.contains("is not trusted"), "{refused}");
    }
}
