// Copyright 2026 Adobe. All rights reserved.
// This file is licensed to you under the Apache License,
// Version 2.0 (http://www.apache.org/licenses/LICENSE-2.0)
// or the MIT license (http://opensource.org/licenses/MIT),
// at your option.

// Unless required by applicable law or agreed to in writing,
// this software is distributed on an "AS IS" BASIS, WITHOUT
// WARRANTIES OR REPRESENTATIONS OF ANY KIND, either express or
// implied. See the LICENSE-MIT and LICENSE-APACHE files for the
// specific language governing permissions and limitations under
// each license.

//! Error types

use std::io;

/// Result type for container operations
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can occur while reading or writing a container
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// I/O error
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// A caller-supplied callback (for example a [`Sink`](crate::Sink))
    /// stopped the operation, such as on user cancel.
    #[error("operation canceled by a callback")]
    UserCanceled,

    /// A caller-supplied callback (for example a [`Sink`](crate::Sink)) failed
    /// with its own error, returned unchanged so the caller can recover it by
    /// downcasting.
    #[error(transparent)]
    Callback(Box<dyn std::error::Error + Send + Sync>),

    /// Invalid file format
    #[error("Invalid container: {0}")]
    InvalidFormat(String),

    /// Unsupported file format
    #[error("Unsupported format")]
    UnsupportedFormat,

    /// A recognized format uses a feature or operation this reader cannot support.
    #[error("Unsupported feature: {0}")]
    UnsupportedFeature(&'static str),

    /// The container cannot hold, or this handler cannot write, an item of
    /// this kind. A write plan reports it for an
    /// [`Updates`](crate::Updates) value it cannot place, before anything is
    /// written.
    #[error("cannot write {0} to this container")]
    UnsupportedItem(crate::ItemKind),

    /// Invalid segment
    #[error("Invalid segment at offset {offset}: {reason}")]
    InvalidSegment { offset: u64, reason: String },

    /// A required item is absent.
    #[error("{what} not found")]
    NotFound { what: &'static str },

    /// More than one candidate where exactly one is required.
    #[error("{what} is ambiguous ({count} candidates)")]
    Ambiguous { what: &'static str, count: usize },

    /// A resource limit was exceeded.
    #[error("limit exceeded: {what} (limit {limit})")]
    LimitExceeded { what: &'static str, limit: u64 },

    /// XML parsing error (from quick-xml)
    #[cfg(feature = "xmp")]
    #[error("XML error: {0}")]
    Xml(#[from] quick_xml::Error),
}

impl Error {
    /// [`Error::Callback`] holding `error`, for a callback to return its own error
    /// through asset-io.
    pub fn callback(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        Error::Callback(error.into())
    }

    /// [`Error::InvalidSegment`]: the data at `offset` is not a valid element of the
    /// container, for the given `reason`.
    pub fn invalid_segment(offset: u64, reason: impl Into<String>) -> Self {
        Error::InvalidSegment {
            offset,
            reason: reason.into(),
        }
    }
}
