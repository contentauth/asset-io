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

//! A portable, dynamically extensible container model for media assets.
//!
//! `asset-io` provides one abstract model for reading, inspecting and updating a
//! wide variety of image, audio, video and document formats, including JPEG, PNG,
//! ISO BMFF (HEIF, AVIF, MP4), RIFF (WebP, WAV, AVI), TIFF, SVG and PDF. Applications
//! work with common structures and metadata items rather than format-specific
//! parsers and writers.
//!
//! Container support is dynamically extensible: applications register built-in
//! or third-party handlers with [`HandlerRegistry`] at runtime. Handler authors
//! use [`container_api`] to add formats without changing the client API.
//!
//! The core is portable and independent of any async runtime. The same handler
//! implementation runs with blocking streams, async adapters or caller-driven
//! I/O. It is designed for high performance: updates stream the output in a single
//! pass with bounded buffers, copying unchanged media without loading the whole
//! asset into memory. [`Sink`]s observe tagged bytes during reads and writes,
//! allowing callers to hash output without a second pass.
//!
//! Used by [c2pa-rs](https://github.com/contentauth/c2pa-rs) for media-container
//! access, `asset-io` separates container mechanics from manifest interpretation,
//! hashing and signing.
//!
//! A [`Structure`] describes a file as an ordered tiling of [`Segment`]s, each
//! split into header, payload and trailer, plus [`Item`]s: logical units such as
//! XMP, JUMBF, EXIF or an embedded file, assembled from [`Extent`]s. It carries
//! geometry only: no data, and no hashing or signing rules. Callers decide what
//! each byte range means.
//!
//! # Glossary
//!
//! - **Asset**: a complete byte stream. Assets are recursive: an asset can contain
//!   embedded assets. The [`Asset`] type pairs one with its parsed structure.
//! - **Structure**: the parsed description of an asset ([`Structure`]): its
//!   segments, items and sub-assets, and the handler that parsed it. It is
//!   geometry only and holds no bytes, so its reads take the stream as an
//!   argument; an [`Asset`] holds the stream for them. A planned structure
//!   ([`Structure::plan`]) describes an output before it is written.
//! - **Container**: a structural family (ISO BMFF, JPEG, RIFF). **Format**: a media
//!   type within one (HEIC and MP4 are both ISO BMFF).
//! - **Handler**: implements a container's specifications
//!   ([`ContainerHandler`](container_api::ContainerHandler)).
//! - **Range**: an offset and a length ([`ByteRange`]).
//! - **Segment**: a unit of the asset's tiling. Segments are ordered, do not
//!   overlap and cover every byte; a segment is what a writer copies, replaces or
//!   moves. Its [`SegmentKind`] gives its dominant purpose: `Preamble` (the leading
//!   bytes that identify the format; "signature" is avoided, as C2PA uses it for
//!   cryptographic signatures), `Metadata`, `MediaData` (coded media of any type)
//!   or `Other`. Items, not segment kinds, say what bytes belong to.
//! - **Role**: within a segment, framing ([`Role::Header`], [`Role::Trailer`])
//!   versus content ([`Role::Payload`]). "Header" always means framing.
//! - **Region**: a segment, a role and a range: the view a hasher takes
//!   ([`Region`]).
//! - **Element**: a native unit of the container (box, chunk, marker segment, PDF
//!   object, IFD). [`Structure::elements`] is the finest tiling by elements.
//! - **Node**: an element and its position in the container's tree
//!   ([`Node`], [`Structure::walk`]).
//! - **Item**: a logical unit the format defines, with an [`ItemKind`] (the
//!   specification that defines it), an [`ItemCategory`] derived from the kind
//!   (metadata or asset) and an [`Encoding`] (its serialization).
//! - **Transfer encoding**: how the bytes a container stores for an item wrap its
//!   content, such as SVG's base64, a PDF `FlateDecode` (zlib) stream or a HEIF
//!   `content_encoding` ([`TransferEncoding`], [`Item::transfer`]). It is distinct
//!   from the [`Encoding`], which is the serialization of the content itself. The
//!   core always identifies it; decoders are optional features (`flate`,
//!   `brotli`). [`Structure::read_item`] returns the
//!   stored bytes (what a hash covers); [`Structure::read_content`] the content.
//! - **Extent**: one contiguous run of an item's bytes. An item may be
//!   multi-extent; "fragment" is reserved for fragmented ISO BMFF.
//! - **Identifiers**: native names, kept exactly: FourCCs, markers, UUIDs, JUMBF
//!   types, URIs, media types and container labels ([`Name`],
//!   [`Item::native_type`], [`Item::native_id`]).
//! - **Relation**: a reference from one item to another that keeps its native type
//!   (HEIF `thmb`, `auxl`, `cdsc`, `dimg`); [`RelationKind`] gives common names.
//! - **Sub-asset**: a contiguous, top-level embedded asset, such as each image of a
//!   Multi-Picture Format file ([`SubAsset`]).
//! - **Embedded asset**: an item the format declares to be a file of a media type
//!   ([`ItemKind::EmbeddedAsset`]). Items whose bytes the format does not type are
//!   [`ItemKind::Other`]; [`Structure::scan_embedded`] also finds those that turn out
//!   to be files.
//!
//! # Layering
//!
//! 1. Account for every byte: segments always tile the asset.
//! 2. Expose the structure the format's own specifications define, including
//!    companion specifications (EXIF/TIFF, MPF, JPEG XT, the HEIF item model), and
//!    preserve native identifiers.
//! 3. Never interpret content that is portable across formats (C2PA manifests, XMP
//!    schemas, IPTC vocabularies). That belongs to optional higher-level helpers
//!    built on this API. Where such content is placed in each container is
//!    container knowledge, and is here.
//!
//! # Design principles
//!
//! - **Container agnostic**: one API for every format. Format knowledge lives in a
//!   [`ContainerHandler`](container_api::ContainerHandler); callers never test
//!   which format they have.
//! - **Streaming**: a file is parsed and written in one pass, in bounded memory.
//! - **Safe on hostile input**: every length is checked, resources are capped,
//!   and errors are returned rather than guessed around.
//! - **Pluggable**: [`HandlerRegistry`] finds handlers by format string or by
//!   content, and callers can register their own.
//!
//! # Containers and features
//!
//! Each built-in container is a Cargo feature (`jpeg`, `png`, `bmff`, `riff`,
//! `svg`, `pdf`, `tiff`, `c2pa`, `gif`, or `all-formats`) and a module under
//! [`containers`]. No container is enabled by default.
//! [`HandlerRegistry::builtin`] holds the handlers of the enabled features, and
//! handlers from other crates are registered at runtime. Without any container
//! feature, the crate is the API a handler crate builds on, and nothing more.
//!
//! # API namespaces
//!
//! The root exposes the client model, [`HandlerRegistry`], [`Asset`],
//! [`Structure`], [`Updates`] and [`Sink`], including C2PA geometry and exact
//! patching. Handler authors use [`container_api`] for handler traits, builders,
//! metadata-update decisions and container utilities. Built-in factories and
//! typed options remain under [`containers`].
//!
//! Advanced clients use [`plan`] for declarative write plans, [`execute`] for
//! caller-driven execution, [`io`] for async reads and [`task`] for read requests
//! and blocking or caller-driven task execution. The `*_async`, planning and
//! task methods on [`Structure`] still compose these APIs; no runtime is required.
//! [`HandlerRegistry::handler`] and [`HandlerRegistry::handlers`] retain handler
//! lookup and composition, including custom use of
//! [`ContainerHandler::compose`](container_api::ContainerHandler::compose).
//!
//! # Example
//!
//! ```no_run
//! use asset_io::{Asset, ItemKind, NullSink, Updates};
//!
//! # fn main() -> asset_io::Result<()> {
//! // Enable the `jpeg` feature for this example.
//! let asset = Asset::open(std::fs::File::open("image.jpg")?, None)?;
//!
//! // Read a metadata item, which may have extents in several segments.
//! if let Some(jumbf) = asset.item(ItemKind::Jumbf)? {
//!     let bytes = asset.read_item(jumbf)?;
//!     println!("JUMBF: {} bytes", bytes.len());
//! }
//!
//! // Write a copy with changes. Every output byte also goes to a sink, tagged
//! // with its segment and role, so it can be hashed without a second pass.
//! let mut out = std::fs::File::create("out.jpg")?;
//! let updates = Updates::new()
//!     .set_xmp(b"<x:xmpmeta/>".to_vec())
//!     .remove_jumbf();
//! asset.write(&mut out, &updates, &mut NullSink)?;
//! # Ok(())
//! # }
//! ```
//!
//! [`Asset`] holds the source; [`Structure`] is the I/O-free description it
//! wraps, whose methods take the source as an argument. Async and caller-driven
//! code uses [`Structure::parse_task`] and the `*_async` methods directly.

#![cfg_attr(docsrs, feature(doc_cfg))]

mod asset;
pub mod container_api;
pub mod containers;
mod crc;
mod embedded;
mod error;
pub mod execute;
mod handler;
pub mod io;
mod item;
pub mod jumbf;
#[cfg(feature = "memory-mapped")]
mod mapped;
mod parse;
pub mod plan;
mod plan_builder;
mod range;
mod registry;
mod segment;
mod sink;
mod structure;
pub mod task;
pub mod testing;
mod thumbnail;
#[cfg(feature = "exif")]
mod tiff;
#[cfg(feature = "tokio")]
#[cfg_attr(docsrs, doc(cfg(feature = "tokio")))]
pub mod tokio;
pub mod transfer;
mod updates;
mod walk;
#[cfg(feature = "xmp")]
mod xmp;

pub use asset::Asset;
pub use embedded::{EmbeddedAsset, EmbeddedReader, ScanLimits};
pub use error::{Error, Result};
pub use handler::C2paHash;
pub use item::{Encoding, Extent, Item, ItemCategory, ItemKind, Relation, RelationKind};
#[cfg(feature = "memory-mapped")]
pub use mapped::MappedFile;
pub use range::ByteRange;
pub use registry::HandlerRegistry;
pub use segment::{labels, Name, Role, Segment, SegmentKind, AFTER_BOX, C2PA_BOX};
pub use sink::{Chunk, NullSink, Sink};
pub use structure::{C2paBox, Region, SegmentId, Structure, SubAsset};
pub use thumbnail::Thumbnail;
#[cfg(feature = "exif")]
pub use tiff::ExifInfo;
pub use transfer::TransferEncoding;
pub use updates::Updates;
pub use walk::Node;
#[cfg(feature = "xmp")]
pub use xmp::MiniXmp;
