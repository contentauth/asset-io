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

//! A parsed asset together with the blocking stream it was parsed from.

use std::{
    cell::{RefCell, RefMut},
    fmt,
    io::{Read, Seek, Write},
    ops::Deref,
};

use crate::{
    embedded::{EmbeddedAsset, ScanLimits},
    error::{Error, Result},
    item::Item,
    plan::WritePlan,
    range::ByteRange,
    registry::HandlerRegistry,
    sink::Sink,
    structure::Structure,
    thumbnail::Thumbnail,
    updates::Updates,
    walk::Node,
};

/// A parsed [`Structure`] together with the blocking stream it describes.
///
/// [`Structure`] is I/O-free: every method that needs bytes takes the source
/// again. An `Asset` holds the source, so those calls need only their own
/// arguments:
///
/// ```no_run
/// # use asset_io::{Asset, NullSink, Updates};
/// # fn main() -> asset_io::Result<()> {
/// // Enable the `jpeg` feature for this example.
/// let asset = Asset::open(std::fs::File::open("image.jpg")?, None)?;
/// if let Some(item) = asset.c2pa_item()? {
///     let exclusions = asset.c2pa_data_hash_exclusions(item)?;
///     let store = asset.read_content(item)?;
///     println!(
///         "{} store bytes, {} excluded ranges",
///         store.len(),
///         exclusions.len()
///     );
/// }
/// let mut out = std::fs::File::create("out.jpg")?;
/// asset.write(&mut out, &Updates::new().remove_c2pa(), &mut NullSink)?;
/// # Ok(())
/// # }
/// ```
///
/// It dereferences to its [`Structure`], so the methods that need no I/O
/// (`items`, `segments`, `c2pa_item`, `supports_named_boxes`, `c2pa_data_hash_exclusions`, ...) are
/// available directly. Where both define a method, the `Asset` one reads from
/// the held source.
///
/// `S` may be owned (a [`File`](std::fs::File)) or borrowed (`&mut File`). The
/// source must keep the bytes the structure describes; nothing checks this.
///
/// Methods take `&self`, so an [`Item`] borrowed from the asset can be passed
/// back to it. The source is borrowed mutably for the duration of each call;
/// calling back into the same asset from inside a [`Sink`] or a callback fails
/// with [`Error::Io`] instead of reading concurrently.
///
/// For async I/O, or for driving reads yourself, use the `*_async` and `*_task`
/// methods of [`Structure`].
pub struct Asset<S> {
    structure: Structure,
    source: RefCell<S>,
}

impl<S> Asset<S> {
    /// An asset from a structure and the stream it describes: one parsed
    /// elsewhere, or the structure a write returned paired with its output, so
    /// the output can be read or [patched](Self::patch_exact) without parsing it
    /// again.
    pub fn from_parts(structure: Structure, source: S) -> Self {
        Self {
            structure,
            source: RefCell::new(source),
        }
    }

    /// The structure.
    pub fn structure(&self) -> &Structure {
        &self.structure
    }

    /// The source, for reads this type does not provide, such as an
    /// [`EmbeddedAsset::reader`].
    pub fn source_mut(&mut self) -> &mut S {
        self.source.get_mut()
    }

    /// The structure and the source.
    pub fn into_parts(self) -> (Structure, S) {
        (self.structure, self.source.into_inner())
    }

    /// The source, discarding the structure.
    pub fn into_source(self) -> S {
        self.source.into_inner()
    }

    fn src(&self) -> Result<RefMut<'_, S>> {
        self.source.try_borrow_mut().map_err(|_| {
            Error::Io(std::io::Error::other(
                "the asset's source is in use by an operation that is still running",
            ))
        })
    }
}

impl<S: Read + Seek> Asset<S> {
    /// Parse `source` with the [built-in](HandlerRegistry::builtin) handlers of
    /// the enabled container features.
    ///
    /// Content detection takes precedence. If no handler recognizes the content,
    /// `Some(format)` selects a handler by extension or MIME type; with `None`,
    /// parsing fails. With no container feature enabled, every format is unsupported.
    ///
    /// For applications. A library should take a registry from its caller and
    /// use [`open_with`](Self::open_with), so the caller's handlers are used.
    pub fn open(source: S, format: Option<&str>) -> Result<Self> {
        Self::open_with(&HandlerRegistry::builtin(), source, format)
    }

    /// Parse `source` with a handler from `registry`; see [`Structure::parse`].
    /// Format selection is as for [`open`](Self::open).
    pub fn open_with(
        registry: &HandlerRegistry,
        mut source: S,
        format: Option<&str>,
    ) -> Result<Self> {
        let structure = Structure::parse(registry, &mut source, format)?;
        Ok(Self::from_parts(structure, source))
    }

    /// See [`Structure::read_item`].
    pub fn read_item(&self, item: &Item) -> Result<Vec<u8>> {
        self.structure.read_item(&mut *self.src()?, item)
    }

    /// See [`Structure::read_content`].
    pub fn read_content(&self, item: &Item) -> Result<Vec<u8>> {
        self.structure.read_content(&mut *self.src()?, item)
    }

    /// See [`Structure::read_range`].
    pub fn read_range(&self, range: ByteRange) -> Result<Vec<u8>> {
        self.structure.read_range(&mut *self.src()?, range)
    }

    /// See [`Structure::stream_range`].
    pub fn stream_range<F>(&self, range: ByteRange, buf: &mut [u8], f: F) -> Result<()>
    where
        F: FnMut(&[u8]) -> Result<()>,
    {
        self.structure
            .stream_range(&mut *self.src()?, range, buf, f)
    }

    /// See [`Structure::read`].
    pub fn read(&self, sink: &mut dyn Sink) -> Result<()> {
        self.structure.read(&mut *self.src()?, sink)
    }

    /// See [`Structure::walk`].
    pub fn walk<F>(&self, f: F) -> Result<()>
    where
        F: FnMut(&Node<'_>) -> Result<()>,
    {
        self.structure.walk(&mut *self.src()?, f)
    }

    /// See [`Structure::c2pa_store`].
    pub fn c2pa_store(&self) -> Result<Option<Vec<u8>>> {
        self.structure.c2pa_store(&mut *self.src()?)
    }

    /// See [`Structure::xmp`].
    pub fn xmp(&self) -> Result<Option<Vec<u8>>> {
        self.structure.xmp(&mut *self.src()?)
    }

    /// See [`Structure::embedded_thumbnail`].
    pub fn embedded_thumbnail(&self) -> Result<Option<Thumbnail>> {
        self.structure.embedded_thumbnail(&mut *self.src()?)
    }

    /// See [`Structure::exif_info`].
    #[cfg(feature = "exif")]
    pub fn exif_info(&self) -> Result<Option<crate::ExifInfo>> {
        self.structure.exif_info(&mut *self.src()?)
    }

    /// See [`Structure::scan_embedded`]. Read an embedded asset's bytes through
    /// [`source_mut`](Self::source_mut) and [`EmbeddedAsset::reader`].
    pub fn scan_embedded(
        &self,
        registry: &HandlerRegistry,
        limits: ScanLimits,
    ) -> Result<Vec<EmbeddedAsset>> {
        self.structure
            .scan_embedded(&mut *self.src()?, registry, limits)
    }

    /// See [`Structure::plan`].
    pub fn plan(&self, updates: &Updates) -> Result<Structure> {
        self.structure.plan(&mut *self.src()?, updates)
    }

    /// See [`Structure::write_plan`](Structure::write_plan).
    pub fn write_plan<'a>(&self, updates: &'a Updates) -> Result<WritePlan<'a>> {
        self.structure.write_plan(&mut *self.src()?, updates)
    }

    /// Write the asset with `updates` applied to `out`, which is written from its
    /// current position, and return the output's structure; see
    /// [`Structure::write`]. Pair the result with `out` in
    /// [`from_parts`](Self::from_parts) to read or patch the output without
    /// parsing it again.
    pub fn write(
        &self,
        out: &mut dyn Write,
        updates: &Updates,
        sink: &mut dyn Sink,
    ) -> Result<Structure> {
        self.structure.write(&mut *self.src()?, out, updates, sink)
    }
}

impl<S: Read + Write + Seek> Asset<S> {
    /// Overwrite an item's bytes in the source, zero-filling the rest; see
    /// [`Structure::patch`]. The structure is unchanged.
    pub fn patch(&self, item: &Item, data: &[u8]) -> Result<()> {
        self.structure.patch(&mut *self.src()?, item, data)
    }

    /// Fill a placeholder in the source with exactly as many bytes as it holds;
    /// see [`Structure::patch_exact`]. The structure is unchanged.
    pub fn patch_exact(&self, item: &Item, data: &[u8]) -> Result<()> {
        self.structure.patch_exact(&mut *self.src()?, item, data)
    }
}

impl<S> Deref for Asset<S> {
    type Target = Structure;

    fn deref(&self) -> &Structure {
        &self.structure
    }
}

impl<S> fmt::Debug for Asset<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Asset")
            .field("structure", &self.structure)
            .finish_non_exhaustive()
    }
}
