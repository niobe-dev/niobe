// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! What reads the agent's memory for the shell's Memory view.
//!
//! The files are read when the view opens, on the loop's thread: a handful of
//! instruction files and notes, each read only where it is a regular file and
//! no larger than the bridge's limit, without waiting on one that is not. A
//! read each time the view opens is what lets it show a file the session has
//! just changed as it is now.

use std::ffi::OsString;
use std::path::PathBuf;

use niobe_tui::{Memory, MemoryFile};

use crate::backend;

/// Reads the memory the selected profile's backend gives its agent, for a
/// session at a repository's root.
#[derive(Debug)]
pub struct Files {
    root: PathBuf,
    profile: Option<niobe_config::Profile>,
    config_dir: Option<OsString>,
    home: Option<OsString>,
}

impl Files {
    /// What the backend of `profile`, started at `root`, would be given, with
    /// the CLI's configuration directory and the home directory as this
    /// process's environment names them.
    pub fn at(root: PathBuf, profile: Option<niobe_config::Profile>) -> Self {
        Self {
            root,
            profile,
            config_dir: std::env::var_os(niobe_bridge_claude::transcript::CONFIG_DIR_VAR),
            home: std::env::var_os("HOME"),
        }
    }
}

impl Memory for Files {
    fn read(&mut self) -> Vec<MemoryFile> {
        backend::memory(
            self.profile.as_ref(),
            &self.root,
            self.config_dir.clone(),
            self.home.clone(),
        )
    }
}
