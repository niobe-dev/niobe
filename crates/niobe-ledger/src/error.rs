// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Why a price table could not be used.

use std::fmt;
use std::path::PathBuf;

use crate::Origin;

/// A price file that could not be read, or a value in one that is not valid.
///
/// An invalid value names the file, its line and the key it is under, so that
/// a mistyped price is fixed by opening the file at that line.
#[derive(Debug)]
pub enum PriceError {
    /// The file exists but could not be read.
    Read {
        /// The file.
        path: PathBuf,
        /// What the operating system said.
        error: std::io::Error,
    },
    /// The text is not valid TOML, or a value in it is not a valid price.
    Invalid {
        /// Where the text came from.
        origin: Origin,
        /// The line, counted from 1.
        line: usize,
        /// The path of the key the value is under, such as
        /// `model[2].price[0].input`. `None` for a syntax error, which may not
        /// be under any key.
        key: Option<String>,
        /// What is wrong, as a sentence fragment.
        message: String,
    },
}

impl fmt::Display for PriceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, error } => write!(f, "cannot read {}: {error}", path.display()),
            Self::Invalid {
                origin,
                line,
                key: Some(key),
                message,
            } => write!(f, "{origin}:{line}: {key}: {message}"),
            Self::Invalid {
                origin,
                line,
                key: None,
                message,
            } => write!(f, "{origin}:{line}: {message}"),
        }
    }
}

impl std::error::Error for PriceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { error, .. } => Some(error),
            Self::Invalid { .. } => None,
        }
    }
}
