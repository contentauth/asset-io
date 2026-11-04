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

//! API for authors of container handlers.
//!
//! Implement [`ContainerHandler`], declare its [`FormatSpec`]s, parse geometry
//! with [`StructureBuilder`] and describe writes with [`PlanBuilder`] or
//! [`WritePlan`]. Register the handler with [`crate::HandlerRegistry`].
//! No built-in container feature is required.
//!
//! [`Io`] and the stream traits support handler hooks; plan primitives support
//! formats needing custom replacement and offset relocation (JPEG, TIFF, RIFF
//! and BMFF). [`Crc32`] supports framing repair, and the limits bound parsers and
//! builders on hostile input. Client drivers live in [`crate::io`],
//! [`crate::task`] and [`crate::execute`]; plan inspection lives in [`crate::plan`].
//! [`block_on`] runs synchronous handler hooks over their caller's stream without
//! an async runtime, including BMFF's optimized structure-only planner.
//!
//! Import author types from this module and client drivers from their own modules:
//!
//! ```
//! use asset_io::{
//!     container_api::{ContainerHandler, ItemInfo, MetadataUpdate, StructureBuilder, WritePlan},
//!     task::Task,
//! };
//! ```
//!
//! Author hooks can access the retained handler, opaque state, and item
//! descriptions through [`handler`], [`attachment`], and [`item_info`]:
//!
//! ```
//! use asset_io::{
//!     container_api::{attachment, handler, item_info, ItemInfo},
//!     Item, Structure,
//! };
//!
//! fn describe(structure: &Structure, item: &Item) -> ItemInfo {
//!     let container_id = handler(structure).id();
//!     let state = attachment::<String>(structure);
//!     item_info(item).clone()
//! }
//! ```

use std::{any::Any, sync::Arc};

pub use crate::{
    crc::Crc32,
    handler::{ContainerHandler, FormatSpec, ReadSeek, ReadWriteSeek, DETECT_LEN},
    io::{BoxFuture, Io},
    item::ItemInfo,
    plan::{OffsetMap, Relocate, Step, WritePlan},
    plan_builder::PlanBuilder,
    segment::MAX_SEGMENT_SIZE,
    structure::{StructureBuilder, MAX_SEGMENTS},
    task::block_on,
    updates::{Existing, MetadataSlot, MetadataUpdate},
};

/// The handler that parsed a structure, for author hooks and custom planning.
pub fn handler(structure: &crate::Structure) -> &Arc<dyn ContainerHandler> {
    structure.handler()
}

/// Container-private state attached while building a structure.
pub fn attachment<T: Any>(structure: &crate::Structure) -> Option<&T> {
    structure.attachment()
}

/// An item's descriptive fields, for carrying them into a new structure.
/// Clients can read these individually through [`crate::Item`]'s accessors.
pub fn item_info(item: &crate::Item) -> &ItemInfo {
    item.info()
}

#[cfg(doctest)]
mod api_boundary_tests {
    //! Author types and advanced drivers must not become root exports.
    //!
    //! ```compile_fail
    //! use asset_io::ContainerHandler;
    //! ```
    //! ```compile_fail
    //! use asset_io::StructureBuilder;
    //! ```
    //! ```compile_fail
    //! use asset_io::ItemInfo;
    //! ```
    //! ```compile_fail
    //! use asset_io::MetadataUpdate;
    //! ```
    //! ```compile_fail
    //! use asset_io::Task;
    //! ```
    //! ```compile_fail
    //! use asset_io::WritePlan;
    //! ```
    //!
    //! Author-only access must stay outside the read-only client model.
    //!
    //! ```compile_fail
    //! fn raw(s: &asset_io::Structure) { let _ = s.handler(); }
    //! ```
    //! ```compile_fail
    //! fn raw(s: &asset_io::Structure) { let _ = s.attachment::<()>(); }
    //! ```
    //! ```compile_fail
    //! fn raw(i: &asset_io::Item) { let _ = i.info(); }
    //! ```
}
