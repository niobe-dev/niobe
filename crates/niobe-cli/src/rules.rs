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

use niobe_core::permission::Rule;
use niobe_tui::rules::{Rules, RulesError};

/// Writes rules into the config of the repository rooted at a path.
#[derive(Debug, Clone)]
pub struct ConfigRules {
    path: PathBuf,
}

impl ConfigRules {
    /// Rules kept in the config of the repository rooted at `root`, whether or
    /// not that file exists yet.
    pub fn at(root: &Path) -> Self {
        Self {
            path: crate::repo::config_path(root),
        }
    }
}

impl Rules for ConfigRules {
    fn remember(&mut self, rule: &Rule) -> Result<(), RulesError> {
        niobe_config::remember(&self.path, rule).map_err(|error| error.to_string().into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
