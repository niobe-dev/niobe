// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The profile list `niobe profiles` prints.

use std::path::PathBuf;

use niobe_config::{Config, Withheld};

/// The list: one row per profile under a header, the selected one marked with
/// `*`, and under each row what the profile sets beyond its backend.
///
/// Variables are listed by name only. A profile's environment is where an API
/// key would be put, and a listing is the kind of output that gets pasted into
/// an issue.
///
/// A profile from a file that has not been trusted says so, and says what
/// trusting the file would put in force: the operator needs to know which of
/// the two answers explains a session that is not running the way its profile
/// reads.
pub fn table(config: &Config, selected: Option<&str>) -> Vec<String> {
    let profiles = config.profiles();
    let name_width = profiles
        .keys()
        .map(|name| name.chars().count())
        .max()
        .unwrap_or(0)
        .max("PROFILE".len());

    let mut lines = vec![format!(
        "  {:<name_width$}  {:<7}  DEFINED IN",
        "PROFILE", "BACKEND"
    )];
    for (name, profile) in profiles {
        let marker = if selected == Some(name.as_str()) {
            '*'
        } else {
            ' '
        };
        lines.push(format!(
            "{marker} {name:<name_width$}  {:<7}  {}",
            profile.backend().as_str(),
            profile.source().display()
        ));

        let detail =
            |label: &str, value: String| format!("  {:<name_width$}  {label:<13}{value}", "");
        if !profile.env().is_empty() {
            let names: Vec<&str> = profile.env().keys().map(String::as_str).collect();
            lines.push(detail("env", names.join(", ")));
        }
        if !profile.args().is_empty() {
            lines.push(detail("args", profile.args().join(" ")));
        }
        if !profile.models().is_empty() {
            lines.push(detail("models", profile.models().join(", ")));
        }
        if let Some(command) = profile.auth_refresh() {
            lines.push(detail("auth_refresh", command.to_owned()));
        }
        if let Some(withheld) = profile.withheld() {
            lines.push(detail("not in force", listed(withheld)));
            lines.push(detail(
                "not trusted",
                "`niobe trust` reads that file as it stands and puts them in force".to_owned(),
            ));
        }
    }
    lines
}

/// What an untrusted file set, by name: enough to say what trusting it would
/// put in force, and no value of anything.
fn listed(withheld: &Withheld) -> String {
    let mut parts = Vec::new();
    if !withheld.env.is_empty() {
        parts.push(format!("env ({})", withheld.env.join(", ")));
    }
    if withheld.args {
        parts.push("args".to_owned());
    }
    if withheld.auth_refresh {
        parts.push("auth_refresh".to_owned());
    }
    parts.join(", ")
}

/// What `niobe profiles` prints when no config defines a profile.
pub fn none_defined(searched: &[PathBuf]) -> String {
    let files: Vec<String> = searched
        .iter()
        .map(|path| path.display().to_string())
        .collect();
    format!("no profiles defined; looked in {}", files.join(" and "))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    const CONFIG: &str = r#"
[profiles.personal]
backend = "claude"

[profiles.work]
backend = "claude"
env = { AWS_PROFILE = "example-sso", SECRET_TOKEN = "do-not-print" }
args = ["--model", "opus"]
auth_refresh = "aws sso login"
"#;

    fn config() -> Config {
        Config::parse(CONFIG, Path::new("/u/config.toml")).expect("the config is valid")
    }

    #[test]
    fn rows_are_aligned_and_the_selected_profile_is_marked() {
        assert_eq!(
            table(&config(), Some("work")),
            [
                "  PROFILE   BACKEND  DEFINED IN",
                "  personal  claude   /u/config.toml",
                "* work      claude   /u/config.toml",
                "            env          AWS_PROFILE, SECRET_TOKEN",
                "            args         --model opus",
                "            auth_refresh aws sso login",
            ]
        );
    }

    #[test]
    fn the_models_a_profile_offers_are_listed_so_the_shell_key_has_something_to_show() {
        let config = Config::parse(
            "[profiles.max]\nbackend = \"claude\"\nmodels = [\"opus\", \"haiku\"]\n",
            Path::new("/u/config.toml"),
        )
        .expect("the config is valid");

        assert_eq!(
            table(&config, None),
            [
                "  PROFILE  BACKEND  DEFINED IN",
                "  max      claude   /u/config.toml",
                "           models       opus, haiku",
            ]
        );
    }

    #[test]
    fn an_untrusted_profile_says_so_and_says_what_trusting_would_put_in_force() {
        let config = Config::parse(CONFIG, Path::new("/r/.niobe/config.toml"))
            .expect("the config is valid")
            .untrusted();

        assert_eq!(
            table(&config, None),
            [
                "  PROFILE   BACKEND  DEFINED IN",
                "  personal  claude   /r/.niobe/config.toml",
                "  work      claude   /r/.niobe/config.toml",
                "            not in force env (AWS_PROFILE, SECRET_TOKEN), args, auth_refresh",
                "            not trusted  `niobe trust` reads that file as it stands and puts \
                 them in force",
            ]
        );
        let listing = table(&config, None).join("\n");
        assert!(!listing.contains("do-not-print"), "{listing}");
        assert!(!listing.contains("aws sso login"), "{listing}");
    }

    #[test]
    fn no_value_of_a_variable_is_printed() {
        let listing = table(&config(), None).join("\n");
        assert!(!listing.contains("do-not-print"), "{listing}");
        assert!(!listing.contains("example-sso"), "{listing}");
        assert!(!listing.contains('*'), "{listing}");
    }

    #[test]
    fn with_no_profiles_the_files_looked_in_are_named() {
        assert_eq!(
            none_defined(&[
                PathBuf::from("/u/config.toml"),
                PathBuf::from("/r/.niobe/config.toml")
            ]),
            "no profiles defined; looked in /u/config.toml and /r/.niobe/config.toml"
        );
    }
}
