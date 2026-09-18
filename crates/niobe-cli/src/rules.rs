// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Where a standing answer the operator gives is written.
//!
//! The repository's own config, `.niobe/config.toml`, and not the user's: a
//! rule is made about the work in front of the operator — this repository's
//! commands, this repository's files — and a repository's file is committable,
//! so a rule made once is a rule the next person working here does not have to
//! make again. A rule that should hold everywhere goes in the user's file by
//! hand, where it can be seen alongside everything else it covers.

use std::path::{Path, PathBuf};

use niobe_config::trust::Trusted;
use niobe_core::permission::Rule;
use niobe_tui::rules::{Rules, RulesError};

/// Writes rules into the config of the repository rooted at a path.
#[derive(Debug, Clone)]
pub struct ConfigRules {
    path: PathBuf,
    /// Where this machine records the config files it has trusted, so that a
    /// file Niobe itself edits does not lose the operator's decision about it.
    trust: Option<PathBuf>,
}

impl ConfigRules {
    /// Rules kept in the config of the repository rooted at `root`, whether or
    /// not that file exists yet.
    pub fn at(root: &Path) -> Self {
        Self {
            path: crate::repo::config_path(root),
            trust: crate::config::trust_path(),
        }
    }

    /// Whether the file is trusted as it now stands.
    fn trusted(&self) -> bool {
        let Some(record) = &self.trust else {
            return false;
        };
        let (Ok(trusted), Ok(text)) = (Trusted::read(record), std::fs::read_to_string(&self.path))
        else {
            return false;
        };
        trusted.trusts(&self.path, &text)
    }

    /// Records the file's new contents as trusted.
    fn retrust(&self) -> Result<(), RulesError> {
        let Some(record) = &self.trust else {
            return Ok(());
        };
        let text = std::fs::read_to_string(&self.path)
            .map_err(|e| format!("cannot read {}: {e}", self.path.display()))?;
        let mut trusted = Trusted::read(record).map_err(|e| e.to_string())?;
        trusted
            .trust(&self.path, &text)
            .map_err(|e| e.to_string())?;
        trusted.write(record).map_err(|e| e.to_string())?;
        Ok(())
    }
}

impl Rules for ConfigRules {
    /// Adds `rule` to the repository's config, keeping the operator's decision
    /// about that file where they had made one.
    ///
    /// The write is Niobe's own and adds one permission the operator just
    /// granted: it cannot introduce an `env`, an `args` or an `auth_refresh`,
    /// because splicing the `allow` array is all it does. Leaving the file
    /// untrusted here would take a profile's environment away in the middle of
    /// a session, for a change the operator asked for and Niobe made.
    fn remember(&mut self, rule: &Rule) -> Result<(), RulesError> {
        let was_trusted = self.trusted();
        niobe_config::remember(&self.path, rule).map_err(|error| error.to_string())?;
        if was_trusted {
            self.retrust()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repository whose config is trusted under a record of the test's own.
    struct Repo {
        dir: tempfile::TempDir,
        record: PathBuf,
    }

    impl Repo {
        fn with_config(text: &str) -> Self {
            let dir = tempfile::tempdir().expect("a temporary directory");
            let path = crate::repo::config_path(dir.path());
            std::fs::create_dir_all(path.parent().unwrap_or(dir.path())).expect("the directory");
            std::fs::write(&path, text).expect("the config is written");
            let record = dir.path().join("trusted.list");
            Self { dir, record }
        }

        fn rules(&self) -> ConfigRules {
            ConfigRules {
                path: crate::repo::config_path(self.dir.path()),
                trust: Some(self.record.clone()),
            }
        }

        fn trust(&self) {
            let path = crate::repo::config_path(self.dir.path());
            let text = std::fs::read_to_string(&path).expect("the config reads");
            let mut trusted = Trusted::read(&self.record).expect("the record reads");
            trusted.trust(&path, &text).expect("a plain path");
            trusted.write(&self.record).expect("the record is written");
        }

        fn is_trusted(&self) -> bool {
            let path = crate::repo::config_path(self.dir.path());
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            Trusted::read(&self.record)
                .expect("the record reads")
                .trusts(&path, &text)
        }
    }

    #[test]
    fn a_rule_is_written_into_the_repositorys_own_config() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let mut rules = ConfigRules::at(dir.path());

        rules
            .remember(&Rule::targeted("Bash", "cargo test"))
            .expect("the config is written");

        let config = niobe_config::Config::read(&crate::repo::config_path(dir.path()))
            .expect("what was written reads back")
            .expect("the file is there");
        assert_eq!(
            config.allowed().rules(),
            [Rule::targeted("Bash", "cargo test")]
        );
    }

    #[test]
    fn a_config_that_cannot_be_written_is_reported_as_the_operator_reads_it() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = crate::repo::config_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap_or(dir.path())).expect("the directory");
        std::fs::write(&path, "[permissions]\n").expect("a table with no array in it");

        let said = ConfigRules::at(dir.path())
            .remember(&Rule::tool("Read"))
            .expect_err("Niobe will not guess where the rule goes")
            .to_string();

        assert!(said.contains("no `allow` array"), "{said}");
    }

    #[test]
    fn a_trusted_config_stays_trusted_when_niobe_writes_a_rule_into_it() {
        let repo = Repo::with_config("[profiles.p]\nbackend = \"claude\"\nenv = { A = \"1\" }\n");
        repo.trust();

        repo.rules()
            .remember(&Rule::tool("Read"))
            .expect("the config is written");

        assert!(
            repo.is_trusted(),
            "a rule the operator granted took away the profile's environment"
        );
    }

    #[test]
    fn a_config_nobody_trusted_is_not_trusted_by_a_rule_being_written_into_it() {
        let repo = Repo::with_config("[profiles.p]\nbackend = \"claude\"\nenv = { A = \"1\" }\n");

        repo.rules()
            .remember(&Rule::tool("Read"))
            .expect("the config is written");

        assert!(!repo.is_trusted());
    }
}
