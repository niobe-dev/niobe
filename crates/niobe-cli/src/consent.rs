// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! What a session asks before it starts, where the repository's config sets
//! what only a trusted file may, and how the answer is recorded.
//!
//! The question shows what trusting the file would put in force, values
//! included, and `niobe trust` prints the same rows before it asks.
//! `niobe profiles` names a variable without its value because a listing is
//! what gets pasted into an issue; these two are where the operator decides
//! whether to let the file start a backend, and `ANTHROPIC_BASE_URL` is
//! harmless or not by what it is set to.

use std::path::PathBuf;

use niobe_config::Config;
use niobe_tui::trust::Question;

use crate::config::{self, Attached, Untrusted};
use crate::printable::on_one_line;

/// The question about `untrusted`: the file, and a row for each thing in it
/// that takes effect only once it is trusted.
pub fn question(untrusted: &Untrusted) -> Question {
    Question {
        path: untrusted.path.display().to_string(),
        grants: grants(&untrusted.config, &untrusted.replaces, &untrusted.attached),
    }
}

/// What `config`, read whole, sets that takes effect only once it is trusted:
/// its `[permissions]` rules, its `default_profile`, a `titles = "model"`
/// that has the model title each new session, and for each profile what
/// it starts the backend with, how it says the account is billed, and whether
/// it replaces one of `replaces`, the user's profiles. Under a profile's
/// `settings` row go what the file `attached` holds that is put in front of the
/// CLI with it — its `env` with values, the events it has hooks for and every
/// other key it sets, with its value — and under its `args` a row for each
/// file they name.
fn grants(config: &Config, replaces: &[String], attached: &[Attached]) -> Vec<(String, String)> {
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
    // What the file names as the default, and whether it turns the model's
    // titles on, read the way the file's own withholding reads them, so that
    // the two cannot disagree on what is withheld.
    let withheld = config.clone().untrusted();
    if let Some((name, _)) = withheld.withheld_default() {
        rows.push(row("default_profile", name.to_owned()));
    }
    if withheld.withholds_model_titles() {
        rows.push(row(
            "titles",
            "model, which spends tokens titling every new session".to_owned(),
        ));
    }
    for (name, profile) in config.profiles() {
        if replaces.contains(name) {
            rows.push(row(name, "replaces your profile of this name".to_owned()));
        }
        if !profile.env().is_empty() {
            let env: Vec<String> = profile
                .env()
                .iter()
                .map(|(key, value)| format!("{}={}", one_word(key), one_word(value)))
                .collect();
            rows.push(row(name, format!("env {}", env.join(", "))));
        }
        if !profile.args().is_empty() {
            let args: Vec<String> = profile.args().iter().map(|arg| one_word(arg)).collect();
            rows.push(row(name, format!("args {}", args.join(" "))));
        }
        if let Some(settings) = profile.settings() {
            rows.push(row(name, format!("settings {}", settings.path())));
            for file in attached
                .iter()
                .filter(|a| a.settings && a.profiles.contains(name))
            {
                rows.extend(settings_rows(name, file));
            }
        }
        for file in attached
            .iter()
            .filter(|a| !a.settings && a.profiles.contains(name))
        {
            rows.push(row(name, format!("file {}", file.path.display())));
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

/// The rows for a settings file: the environment it sets with its values, the
/// events it runs hooks on and every other key it sets with its value, or why
/// what it holds cannot be shown.
///
/// What the CLI is started under is what the file says, so a file shown only
/// by name would be agreed to unread.
fn settings_rows(profile: &str, file: &Attached) -> Vec<(String, String)> {
    let text = match &file.text {
        Ok(text) => text,
        Err(error) => {
            return vec![row(
                profile,
                format!("settings file cannot be read: {error}"),
            )];
        }
    };
    let Ok(serde_json::Value::Object(settings)) = serde_json::from_str(text) else {
        return vec![row(
            profile,
            "settings file is not a JSON object".to_owned(),
        )];
    };
    let mut rows = Vec::new();
    if let Some(serde_json::Value::Object(env)) = settings.get("env")
        && !env.is_empty()
    {
        let env: Vec<String> = env
            .iter()
            .map(|(key, value)| {
                let value = match value {
                    serde_json::Value::String(value) => value.clone(),
                    other => other.to_string(),
                };
                format!("{}={}", one_word(key), one_word(&value))
            })
            .collect();
        rows.push(row(profile, format!("settings env {}", env.join(", "))));
    }
    if let Some(serde_json::Value::Object(hooks)) = settings.get("hooks")
        && !hooks.is_empty()
    {
        let events: Vec<String> = hooks.keys().map(|event| one_word(event)).collect();
        rows.push(row(
            profile,
            format!("settings hooks that run commands on {}", events.join(", ")),
        ));
    }
    // Every other key, value and all — an `env` or `hooks` that is not an
    // object included, since the rows above passed it over — rather than a
    // list of the ones thought dangerous: `apiKeyHelper` is a command the CLI
    // signs in with, `permissions` lets calls through unasked, and the CLI
    // adds keys of the kind with its releases.
    for (key, value) in settings
        .iter()
        .filter(|(key, value)| !(matches!(key.as_str(), "env" | "hooks") && value.is_object()))
    {
        let value = match value {
            serde_json::Value::String(value) => value.clone(),
            other => other.to_string(),
        };
        rows.push(row(
            profile,
            format!("settings {} {}", one_word(key), one_word(&value)),
        ));
    }
    rows
}

/// What `niobe trust` prints before it asks: the file, and the same rows the
/// question in the shell shows, values included, one line each under it.
pub fn listing(untrusted: &Untrusted) -> Vec<String> {
    let path = untrusted.path.display().to_string();
    let path = on_one_line(&path);
    let rows = grants(&untrusted.config, &untrusted.replaces, &untrusted.attached);
    if rows.is_empty() {
        return vec![format!(
            "{path} sets nothing that takes effect only once it is trusted"
        )];
    }
    let width = rows
        .iter()
        .map(|(name, _)| name.chars().count())
        .max()
        .unwrap_or(0);
    std::iter::once(format!("{path} puts in force once trusted:"))
        .chain(
            rows.iter()
                .map(|(name, value)| format!("  {name:<width$}  {value}")),
        )
        .collect()
}

/// One row, its name and value each on one line with every control in it
/// shown, so that neither can print lines of its own under it.
fn row(name: &str, value: String) -> (String, String) {
    (
        on_one_line(name).into_owned(),
        on_one_line(&value).into_owned(),
    )
}

/// `value` as one word of a row: as it is where it reads as one, and quoted
/// the way TOML quotes a string where it is empty or holds a space, a comma, a
/// quote, a backslash or a control. Otherwise `["--x", "a b"]` and `["--x",
/// "a", "b"]` would read alike, and an `env` value holding `, B=2` as a second
/// variable.
fn one_word(value: &str) -> String {
    let plain = !value.is_empty()
        && !value
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, ',' | '"' | '\\'));
    if plain {
        return value.to_owned();
    }
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for c in value.chars() {
        if matches!(c, '"' | '\\') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    quoted
}

/// Records the config of `untrusted`, and each file it names that the CLI
/// reads, as read by the operator, at the contents they were shown, so that what
/// it sets is in force from the next time it is loaded.
///
/// What is trusted is what the operator could read and agree to: a file that
/// is not a config says nothing of the kind, and is refused.
pub fn record(untrusted: &Untrusted) -> Result<(), String> {
    let path = &untrusted.path;
    Config::parse(&untrusted.text, path)
        .map_err(|error| format!("{} is not trusted: {error}", path.display()))?;
    config::record_trust(&record_path()?, untrusted)
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
            attached: Vec::new(),
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
    fn the_listing_shows_the_questions_rows_under_the_file_aligned() {
        assert_eq!(
            listing(&untrusted(&["max"])),
            [
                "/r/.niobe/config.toml puts in force once trusted:",
                "  permissions      allow Edit, Bash(cargo test)",
                "  default_profile  work",
                "  max              replaces your profile of this name",
                "  work             env ANTHROPIC_BASE_URL=https://proxy.example, \
                 AWS_PROFILE=work-sso",
                "  work             args --model opus",
                "  work             settings ./claude.json",
                "  work             auth_refresh aws sso login",
                "  work             billing metered",
            ]
        );
    }

    #[test]
    fn a_file_that_sets_nothing_gated_is_listed_as_setting_nothing() {
        let path = PathBuf::from("/r/.niobe/config.toml");
        let plain = "[profiles.plain]\nbackend = \"codex\"\n";
        let untrusted = Untrusted {
            config: Config::parse(plain, &path).expect("the config is valid"),
            text: plain.to_owned(),
            path,
            replaces: Vec::new(),
            attached: Vec::new(),
        };

        assert_eq!(
            listing(&untrusted),
            ["/r/.niobe/config.toml sets nothing that takes effect only once it is trusted"]
        );
    }

    #[test]
    fn a_file_that_turns_the_models_titles_on_says_so_in_the_question_and_the_listing() {
        let path = PathBuf::from("/r/.niobe/config.toml");
        let text = "titles = \"model\"\n";
        let untrusted = Untrusted {
            config: Config::parse(text, &path).expect("the config is valid"),
            text: text.to_owned(),
            path,
            replaces: Vec::new(),
            attached: Vec::new(),
        };

        assert_eq!(
            question(&untrusted).grants,
            [(
                "titles".to_owned(),
                "model, which spends tokens titling every new session".to_owned()
            )]
        );
        assert_eq!(
            listing(&untrusted),
            [
                "/r/.niobe/config.toml puts in force once trusted:",
                "  titles  model, which spends tokens titling every new session",
            ]
        );
    }

    #[test]
    fn a_file_that_turns_the_models_titles_off_is_not_a_row() {
        let path = PathBuf::from("/r/.niobe/config.toml");
        let text = "titles = \"first-prompt\"\n";
        let untrusted = Untrusted {
            config: Config::parse(text, &path).expect("the config is valid"),
            text: text.to_owned(),
            path,
            replaces: Vec::new(),
            attached: Vec::new(),
        };

        assert!(question(&untrusted).grants.is_empty());
    }

    fn untrusted_text(text: &str) -> Untrusted {
        let path = PathBuf::from("/r/.niobe/config.toml");
        Untrusted {
            config: Config::parse(text, &path).expect("the config is valid"),
            text: text.to_owned(),
            path,
            replaces: Vec::new(),
            attached: Vec::new(),
        }
    }

    #[test]
    fn a_value_with_line_breaks_is_listed_on_one_line_with_the_breaks_shown() {
        let untrusted = untrusted_text(
            "[profiles.p]\nbackend = \"claude\"\nargs = [\"--x\\n\\n\\nx sets nothing\"]\n\
             env = { A = \"1\\n2\\u001b[31m\" }\n",
        );

        let listing = listing(&untrusted);

        assert_eq!(listing.len(), 1 + question(&untrusted).grants.len());
        assert!(
            listing.iter().all(|line| !line.contains('\n')),
            "{listing:?}"
        );
        assert!(
            listing
                .iter()
                .any(|line| line.contains("--x␤␤␤x sets nothing")),
            "{listing:?}"
        );
        assert!(
            listing.iter().any(|line| line.contains("1␤2␛[31m")),
            "{listing:?}"
        );
    }

    #[test]
    fn args_that_split_a_value_differently_are_listed_differently_and_keep_their_spacing() {
        let one =
            untrusted_text("[profiles.p]\nbackend = \"claude\"\nargs = [\"--x\", \"a  b\"]\n");
        let two =
            untrusted_text("[profiles.p]\nbackend = \"claude\"\nargs = [\"--x\", \"a\", \"b\"]\n");

        let one = question(&one).grants;
        let two = question(&two).grants;

        assert_eq!(one, [("p".to_owned(), "args --x \"a  b\"".to_owned())]);
        assert_eq!(two, [("p".to_owned(), "args --x a b".to_owned())]);
    }

    #[test]
    fn an_env_value_that_reads_as_more_than_one_variable_is_quoted() {
        let untrusted = untrusted_text(
            "[profiles.p]\nbackend = \"claude\"\nenv = { A = \"1, B=2\", C = \"\" }\n",
        );

        assert_eq!(
            question(&untrusted).grants,
            [("p".to_owned(), "env A=\"1, B=2\", C=\"\"".to_owned())]
        );
    }

    #[test]
    fn a_file_that_is_not_a_config_is_not_recorded_as_trusted() {
        let mut not_a_config = untrusted(&[]);
        not_a_config.text = "[profiles.x]\nbackend = 3\n".to_owned();

        let refused = record(&not_a_config).expect_err("the text is not a config");

        assert!(refused.contains("is not trusted"), "{refused}");
    }

    /// A settings file attached to the `work` profile of [`untrusted`].
    fn with_settings_file(text: Result<String, String>) -> Untrusted {
        let mut untrusted = untrusted(&[]);
        untrusted.attached = vec![Attached {
            path: PathBuf::from("/r/claude.json"),
            text,
            profiles: vec!["work".to_owned()],
            settings: true,
        }];
        untrusted
    }

    fn settings_rows_of(untrusted: &Untrusted) -> Vec<String> {
        question(untrusted)
            .grants
            .into_iter()
            .filter(|(name, value)| name == "work" && value.starts_with("settings "))
            .map(|(_, value)| value)
            .collect()
    }

    #[test]
    fn a_settings_file_is_shown_with_the_environment_and_hooks_it_puts_in_front_of_the_cli() {
        let untrusted = with_settings_file(Ok(
            r#"{"env":{"ANTHROPIC_BASE_URL":"https://a.example","N":3},"hooks":{"PreToolUse":[]}}"#
                .to_owned(),
        ));

        assert_eq!(
            settings_rows_of(&untrusted),
            [
                "settings ./claude.json",
                "settings env ANTHROPIC_BASE_URL=https://a.example, N=3",
                "settings hooks that run commands on PreToolUse",
            ]
        );
    }

    /// `apiKeyHelper` is a shell command whose output the CLI signs in with,
    /// and `permissions` lets calls through without a question: a file agreed
    /// to with those left off the question was agreed to unread.
    #[test]
    fn a_settings_file_is_shown_with_every_other_key_it_sets_and_its_value() {
        let untrusted = with_settings_file(Ok(r#"{
                "apiKeyHelper": "curl -s https://a.example/key",
                "permissions": {"allow": ["Bash"]},
                "env": {"A": "1"},
                "model": "opus"
            }"#
        .to_owned()));

        assert_eq!(
            settings_rows_of(&untrusted),
            [
                "settings ./claude.json",
                "settings env A=1",
                "settings apiKeyHelper \"curl -s https://a.example/key\"",
                "settings model opus",
                r#"settings permissions "{\"allow\":[\"Bash\"]}""#,
            ]
        );
    }

    #[test]
    fn a_settings_file_that_cannot_be_shown_says_so_rather_than_nothing() {
        let unreadable = with_settings_file(Err("not a regular file".to_owned()));
        let not_json = with_settings_file(Ok("env = 1".to_owned()));

        assert_eq!(
            settings_rows_of(&unreadable)[1],
            "settings file cannot be read: not a regular file"
        );
        assert_eq!(
            settings_rows_of(&not_json)[1],
            "settings file is not a JSON object"
        );
    }

    #[test]
    fn a_file_the_operator_could_not_be_shown_is_not_recorded_as_trusted() {
        let refused = record(&with_settings_file(Err("not a regular file".to_owned())))
            .expect_err("it cannot be read");

        assert!(
            refused.contains("/r/claude.json cannot be read"),
            "{refused}"
        );
    }
}
