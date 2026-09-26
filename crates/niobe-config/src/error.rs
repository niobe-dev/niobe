// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Why a config could not be used.

use std::fmt;
use std::path::PathBuf;

/// A config file that could not be read or written, a value in one that is not valid, or a
/// profile asked for that no config defines.
///
/// Every message names the file, and every invalid value names its line and,
/// where there is one, its key: an operator fixes a config by opening the file
/// at that line, and a deserializer's account of the types it expected does not
/// get them there.
#[derive(Debug)]
pub enum ConfigError {
    /// The file exists but could not be read.
    Read {
        /// The file.
        path: PathBuf,
        /// What the operating system said.
        error: std::io::Error,
    },
    /// The file could not be written, and is as it was before the attempt.
    Write {
        /// The file.
        path: PathBuf,
        /// What the operating system said.
        error: std::io::Error,
    },
    /// The file is not valid TOML, or a value in it is not valid config.
    Invalid {
        /// The file.
        path: PathBuf,
        /// The line, counted from 1.
        line: usize,
        /// The dotted path of the key the value is under, such as
        /// `profiles.work.backend`. `None` for a syntax error, which may not
        /// be under any key.
        key: Option<String>,
        /// What is wrong, as a sentence fragment.
        message: String,
    },
    /// A config file whose trust could not be recorded, so the operator's
    /// decision about it could not be kept.
    Untrustable {
        /// The file.
        path: PathBuf,
        /// Why, as a sentence fragment.
        message: String,
    },
    /// `--profile` named a profile that no config defines.
    UnknownProfile {
        /// The name asked for.
        name: String,
        /// The profiles the config does define, sorted.
        defined: Vec<String>,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, error } => write!(f, "cannot read {}: {error}", path.display()),
            Self::Write { path, error } => write!(f, "cannot write {}: {error}", path.display()),
            Self::Invalid {
                path,
                line,
                key: Some(key),
                message,
            } => write!(f, "{}:{line}: {key}: {message}", path.display()),
            Self::Invalid {
                path,
                line,
                key: None,
                message,
            } => write!(f, "{}:{line}: {message}", path.display()),
            Self::Untrustable { path, message } => {
                write!(f, "cannot record trust for {}: {message}", path.display())
            }
            Self::UnknownProfile { name, defined } if defined.is_empty() => {
                write!(f, "no profile named `{name}`: no config defines a profile")
            }
            Self::UnknownProfile { name, defined } => write!(
                f,
                "no profile named `{name}`; the profiles defined are {}",
                defined
                    .iter()
                    .map(|n| format!("`{n}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { error, .. } | Self::Write { error, .. } => Some(error),
            Self::Invalid { .. } | Self::Untrustable { .. } | Self::UnknownProfile { .. } => None,
        }
    }
}
