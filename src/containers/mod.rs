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

//! The built-in container handlers, one module per Cargo feature.
//!
//! Each enabled module exposes its handler type, `handler()` for registration
//! and its `ID`; a few add options only their container has, such as ISO BMFF's
//! `bmff::BmffUpdates`. Most callers never name a module: they
//! use [`HandlerRegistry::builtin`], which holds every enabled handler.
//!
//! # Isolation
//!
//! A handler module uses only the crate's public API, as a handler in another
//! crate would, and keeps its helpers private in its own directory. The rest of
//! the crate depends on no handler; [`HandlerRegistry::builtin`] is the one place
//! the enabled handlers are assembled.

use crate::HandlerRegistry;

#[cfg(feature = "bmff")]
#[cfg_attr(docsrs, doc(cfg(feature = "bmff")))]
pub mod bmff;
#[cfg(feature = "c2pa")]
#[cfg_attr(docsrs, doc(cfg(feature = "c2pa")))]
pub mod c2pa;
#[cfg(feature = "id3")]
#[cfg_attr(docsrs, doc(cfg(feature = "id3")))]
pub mod id3;
#[cfg(feature = "jpeg")]
#[cfg_attr(docsrs, doc(cfg(feature = "jpeg")))]
pub mod jpeg;
#[cfg(feature = "pdf")]
#[cfg_attr(docsrs, doc(cfg(feature = "pdf")))]
pub mod pdf;
#[cfg(feature = "png")]
#[cfg_attr(docsrs, doc(cfg(feature = "png")))]
pub mod png;
#[cfg(feature = "riff")]
#[cfg_attr(docsrs, doc(cfg(feature = "riff")))]
pub mod riff;
#[cfg(feature = "svg")]
#[cfg_attr(docsrs, doc(cfg(feature = "svg")))]
pub mod svg;
#[cfg(feature = "tiff")]
#[cfg_attr(docsrs, doc(cfg(feature = "tiff")))]
pub mod tiff;

/// Add the handler of every enabled format to `registry`.
///
/// The one place the handlers are assembled; [`HandlerRegistry::builtin`] calls
/// it.
#[allow(unused_variables)]
pub(crate) fn register_builtin(registry: &mut HandlerRegistry) {
    #[cfg(feature = "jpeg")]
    registry.add_arc(jpeg::handler());
    #[cfg(feature = "png")]
    registry.add_arc(png::handler());
    #[cfg(feature = "bmff")]
    registry.add_arc(bmff::handler());
    #[cfg(feature = "riff")]
    registry.add_arc(riff::handler());
    #[cfg(feature = "svg")]
    registry.add_arc(svg::handler());
    #[cfg(feature = "pdf")]
    registry.add_arc(pdf::handler());
    #[cfg(feature = "tiff")]
    registry.add_arc(tiff::handler());
    #[cfg(feature = "c2pa")]
    registry.add_arc(c2pa::handler());
    #[cfg(feature = "id3")]
    registry.add_arc(id3::handler());
    #[cfg(feature = "tiff")]
    registry.add_arc(tiff::readonly_handler());
}
