// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The profile list `niobe profiles` prints.

use std::path::PathBuf;

use niobe_config::Config;

/// The list: one row per profile under a header, the selected one marked with
/// `*`, and under each row what the profile sets beyond its backend.
///
/// Variables are listed by name only. A profile's environment is where an API
/// key would be put, and a listing is the kind of output that gets pasted into
/// an issue.
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
        if let Some(command) = profile.auth_refresh() {
            lines.push(detail("auth_refresh", command.to_owned()));
        }
    }
    lines
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
