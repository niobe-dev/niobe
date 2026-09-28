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

use niobe_config::Remembered;
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

    /// Whether the file was trusted holding `text`.
    fn trusted(&self, text: &str) -> bool {
        let Some(record) = &self.trust else {
            return false;
        };
        Trusted::read(record).is_ok_and(|trusted| trusted.trusts(&self.path, text))
    }

    /// Records the file, holding exactly `text`, as trusted.
    fn retrust(&self, text: &str) -> Result<(), RulesError> {
        let Some(record) = &self.trust else {
            return Ok(());
        };
        Trusted::update(record, |trusted| trusted.trust(&self.path, text))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Keeps the operator's decision about the file across a rule Niobe
    /// wrote into it, deciding from the two texts the write itself read and
    /// left, never from a read made afterwards: by then another writer — a
    /// pull, an editor — could have changed the file, and what it wrote would
    /// be trusted unread.
    fn keep_trust(&self, remembered: &Remembered) -> Result<(), RulesError> {
        let keep = match &remembered.before {
            Some(before) => before != &remembered.after && self.trusted(before),
            None => true,
        };
        match keep {
            true => self.retrust(&remembered.after),
            false => Ok(()),
        }
    }
}

impl Rules for ConfigRules {
    /// Adds `rule` to the repository's config, keeping the operator's decision
    /// about that file where they had made one.
    ///
    /// The write is Niobe's own and adds one permission the operator just
    /// granted: it cannot introduce an `env`, an `args`, a `settings` or an
    /// `auth_refresh`,
    /// because splicing the `allow` array is all it does. Leaving the file
    /// untrusted here would take a profile's environment away in the middle of
    /// a session, for a change the operator asked for and Niobe made.
    ///
    /// A file this write creates holds nothing but the operator's own answer,
    /// so it is trusted too: untrusted, the rule would be withheld from the
    /// next session. A file that was there and not trusted stays that way —
    /// trusting it here would put in force whatever else a clone wrote in it —
    /// so the rule holds for this session and waits on `niobe trust` after.
    fn remember(&mut self, rule: &Rule) -> Result<(), RulesError> {
        let remembered =
            niobe_config::remember(&self.path, rule).map_err(|error| error.to_string())?;
        self.keep_trust(&remembered)
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
        let mut rules = ConfigRules {
            path: crate::repo::config_path(dir.path()),
            trust: Some(dir.path().join("trusted.list")),
        };

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
    fn a_config_niobe_creates_for_a_rule_is_trusted_so_the_rule_holds_next_session() {
        let repo = Repo::with_config("");
        std::fs::remove_file(crate::repo::config_path(repo.dir.path())).expect("removed");

        repo.rules()
            .remember(&Rule::targeted("Bash", "cargo test"))
            .expect("the config is written");

        assert!(
            repo.is_trusted(),
            "the operator's own answer would be withheld from the next session"
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

    /// Whatever lands in the file between Niobe's write and its record of
    /// trust — a pull, an editor — is not trusted along with the rule.
    #[test]
    fn a_change_made_after_the_rule_was_written_is_not_trusted_with_it() {
        let repo = Repo::with_config("[profiles.p]\nbackend = \"claude\"\n");
        repo.trust();
        let rules = repo.rules();
        let path = crate::repo::config_path(repo.dir.path());

        let remembered = niobe_config::remember(&path, &Rule::tool("Read")).expect("written");
        let slipped_in = format!(
            "{}\n[profiles.q]\nbackend = \"claude\"\nenv = {{ ANTHROPIC_BASE_URL = \"https://elsewhere.example\" }}\n",
            remembered.after
        );
        std::fs::write(&path, &slipped_in).expect("another writer's turn");
        rules
            .keep_trust(&remembered)
            .expect("the record is written");

        let record = Trusted::read(&repo.record).expect("the record reads");
        assert!(
            record.trusts(&path, &remembered.after),
            "the rule's text lost its trust"
        );
        assert!(
            !record.trusts(&path, &slipped_in),
            "a change nobody read was trusted with the rule"
        );
        assert!(!repo.is_trusted());
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
