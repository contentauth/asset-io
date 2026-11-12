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

//! Metadata updates to apply when writing an asset.

use std::{
    any::{Any, TypeId},
    collections::HashMap,
};

/// What to do with one kind of metadata when writing an asset.
///
/// By default, all metadata is kept unchanged. A handler reads it from
/// [`Updates::xmp`] and [`Updates::c2pa`].
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub enum MetadataUpdate {
    /// Keep existing metadata (default)
    #[default]
    Keep,
    /// Remove existing metadata
    Remove,
    /// Replace or add metadata
    Set(Vec<u8>),
}

/// What to do with an item of metadata the asset already has; see
/// [`MetadataSlot::existing`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Existing<'a> {
    /// Write the item as it is.
    Keep,
    /// Leave the item out.
    Drop,
    /// Write these bytes in its place.
    Replace(&'a [u8]),
}

/// Applies a [`MetadataUpdate`] to the items of one kind of metadata (XMP, C2PA)
/// as a handler meets them, and says what is left to insert.
///
/// A handler asks [`existing`](Self::existing) for each item of that kind in the
/// source, in order. A new value replaces the first and drops the rest, so the
/// output never holds two. After the point where new metadata belongs, whether
/// that is a place the format prefers or the end, it asks [`pending`](Self::pending)
/// for a value that no existing item took.
///
/// ```
/// use asset_io::{
///     container_api::{Existing, MetadataSlot},
///     Updates,
/// };
///
/// let updates = Updates::new().set_xmp(b"new".to_vec());
/// let mut xmp = MetadataSlot::new(updates.xmp());
/// assert_eq!(xmp.existing(), Existing::Replace(&b"new"[..]));
/// assert_eq!(xmp.existing(), Existing::Drop);
/// assert_eq!(xmp.pending(), None);
/// ```
#[derive(Debug, Clone, Copy)]
pub struct MetadataSlot<'a> {
    update: &'a MetadataUpdate,
    done: bool,
}

impl<'a> MetadataSlot<'a> {
    /// A slot that applies `update`.
    pub fn new(update: &'a MetadataUpdate) -> Self {
        Self {
            update,
            done: false,
        }
    }

    /// What to do with the next item the source has.
    pub fn existing(&mut self) -> Existing<'a> {
        match self.update {
            MetadataUpdate::Keep => {
                self.done = true;
                Existing::Keep
            }
            MetadataUpdate::Set(d) if !self.done => {
                self.done = true;
                Existing::Replace(d)
            }
            _ => Existing::Drop,
        }
    }

    /// The bytes still to insert, once: a new value no existing item replaced.
    pub fn pending(&mut self) -> Option<&'a [u8]> {
        match self.update {
            MetadataUpdate::Set(d) if !self.done => {
                self.done = true;
                Some(d)
            }
            _ => None,
        }
    }

    /// Whether a new value is still waiting for a place.
    pub fn is_pending(&self) -> bool {
        matches!(self.update, MetadataUpdate::Set(_)) && !self.done
    }
}

/// Updates to apply when writing a file
///
/// This struct uses a builder pattern where the default is to keep all existing
/// metadata unchanged. Use the builder methods to explicitly specify changes.
///
/// # Example
///
/// ```
/// use asset_io::Updates;
///
/// // Default: keep everything
/// let keep = Updates::new();
///
/// // Remove XMP, keep everything else
/// let no_xmp = Updates::new().remove_xmp();
///
/// // Set a new C2PA manifest store, remove XMP, keep everything else
/// let updates = Updates::new()
///     .set_c2pa(b"new manifest store".to_vec())
///     .remove_xmp();
/// ```
#[derive(Debug, Default)]
pub struct Updates {
    /// XMP data update strategy (use builder methods to modify)
    pub(crate) xmp: MetadataUpdate,

    /// C2PA manifest store update strategy (use builder methods to modify)
    pub(crate) c2pa: MetadataUpdate,

    /// The store being set adds to the existing store
    /// (see [`Updates::as_update`]).
    pub(crate) c2pa_is_update: bool,

    /// Set by [`Updates::remove_all`], for handlers that remove more than XMP and
    /// the C2PA store then.
    pub(crate) remove_all: bool,

    /// Options a container defines for itself (see [`Updates::set_extension`]).
    extensions: Extensions,
}

/// Values of any type, one per type, kept by [`Updates`].
#[derive(Default)]
struct Extensions(HashMap<TypeId, Box<dyn Any + Send + Sync>>);

impl std::fmt::Debug for Extensions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Extensions({})", self.0.len())
    }
}

impl Updates {
    /// Create a new `Updates` builder with all metadata set to keep (no changes)
    ///
    /// This is the same as `Updates::default()` but more explicit.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set XMP metadata to a new value
    ///
    /// # Example
    ///
    /// ```no_run
    /// use asset_io::Updates;
    ///
    /// let updates = Updates::new().set_xmp(b"<xmp>...</xmp>".to_vec());
    /// ```
    pub fn set_xmp(mut self, xmp: Vec<u8>) -> Self {
        self.xmp = MetadataUpdate::Set(xmp);
        self
    }

    /// Remove XMP metadata
    ///
    /// # Example
    ///
    /// ```no_run
    /// use asset_io::Updates;
    ///
    /// let updates = Updates::new().remove_xmp();
    /// ```
    pub fn remove_xmp(mut self) -> Self {
        self.xmp = MetadataUpdate::Remove;
        self
    }

    /// Keep existing XMP metadata (explicit, same as default)
    ///
    /// # Example
    ///
    /// ```no_run
    /// use asset_io::Updates;
    ///
    /// let updates = Updates::new().keep_xmp();
    /// ```
    pub fn keep_xmp(mut self) -> Self {
        self.xmp = MetadataUpdate::Keep;
        self
    }

    /// Set the C2PA manifest store (a JUMBF superbox) to a new value.
    ///
    /// The store replaces the existing one, or is added where the container's C2PA
    /// embedding puts it. Other JUMBF, which containers such as JPEG may carry
    /// alongside, is left as it is. A container that carries JUMBF of any type
    /// tells a store by its superbox type, so there the store must have the type
    /// [`C2PA_STORE_TYPE`](crate::jumbf::C2PA_STORE_TYPE).
    ///
    /// # Example
    ///
    /// ```no_run
    /// use asset_io::Updates;
    ///
    /// let updates = Updates::new().set_c2pa(b"manifest store".to_vec());
    /// ```
    pub fn set_c2pa(mut self, store: Vec<u8>) -> Self {
        self.c2pa = MetadataUpdate::Set(store);
        self.c2pa_is_update = false;
        self
    }

    /// What to do with the XMP.
    pub fn xmp(&self) -> &MetadataUpdate {
        &self.xmp
    }

    /// What to do with the C2PA manifest store.
    pub fn c2pa(&self) -> &MetadataUpdate {
        &self.c2pa
    }

    /// Whether the store being set is an update to the existing store; see
    /// [`as_update`](Self::as_update).
    pub fn is_c2pa_update(&self) -> bool {
        self.c2pa_is_update
    }

    /// Whether these updates came from [`remove_all`](Self::remove_all). A handler
    /// that has options of its own (see [`extension`](Self::extension)) treats an
    /// option that was not set as removed then.
    pub fn removes_all(&self) -> bool {
        self.remove_all
    }

    /// Attach an option that one container defines for itself, such as the merkle
    /// boxes of ISO BMFF. One value is kept per type; a later value of the same
    /// type replaces the earlier. Containers that do not know the type ignore it.
    /// A container that defines an option usually offers builder methods that call
    /// this, so callers rarely do.
    pub fn set_extension<T: Any + Send + Sync>(mut self, value: T) -> Self {
        self.extensions.0.insert(TypeId::of::<T>(), Box::new(value));
        self
    }

    /// The option of type `T`, if one was set with
    /// [`set_extension`](Self::set_extension).
    pub fn extension<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.extensions
            .0
            .get(&TypeId::of::<T>())
            .and_then(|v| v.downcast_ref::<T>())
    }

    /// Forget the option of type `T`.
    pub fn clear_extension<T: Any + Send + Sync>(mut self) -> Self {
        self.extensions.0.remove(&TypeId::of::<T>());
        self
    }

    /// Mark the store given to [`set_c2pa`](Self::set_c2pa) as an update
    /// manifest added to the existing store: `set_c2pa(store).as_update()`. Call it
    /// after `set_c2pa`; a later `set_c2pa`, `remove_c2pa` or `keep_c2pa`
    /// clears it.
    ///
    /// Most containers ignore this. A container that keeps the original store apart
    /// from later additions (ISO BMFF), or in the store of each revision (PDF), uses
    /// it: if the new store begins with the
    /// existing store's manifests, byte for byte, it leaves them in place and
    /// stores only the rest as an update. Otherwise (nothing to update, or the
    /// existing manifests changed) it writes the new store as a whole. The hint is
    /// needed because a standard manifest added to a signed file extends the store
    /// in the same way, and must not be treated as an update.
    pub fn as_update(mut self) -> Self {
        self.c2pa_is_update = true;
        self
    }

    /// Remove the C2PA manifest store. Other JUMBF is kept.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use asset_io::Updates;
    ///
    /// let updates = Updates::new().remove_c2pa();
    /// ```
    pub fn remove_c2pa(mut self) -> Self {
        self.c2pa = MetadataUpdate::Remove;
        self.c2pa_is_update = false;
        self
    }

    /// Keep the existing C2PA manifest store (explicit, same as default)
    ///
    /// # Example
    ///
    /// ```no_run
    /// use asset_io::Updates;
    ///
    /// let updates = Updates::new().keep_c2pa();
    /// ```
    pub fn keep_c2pa(mut self) -> Self {
        self.c2pa = MetadataUpdate::Keep;
        self.c2pa_is_update = false;
        self
    }

    /// Create updates that keep all existing metadata (no changes)
    ///
    /// This is an alias for `Updates::new()` or `Updates::default()`.
    pub fn keep_all() -> Self {
        Self::default()
    }

    /// Create updates that remove all metadata
    ///
    /// # Example
    ///
    /// ```no_run
    /// use asset_io::Updates;
    ///
    /// let updates = Updates::remove_all();
    /// ```
    pub fn remove_all() -> Self {
        let mut updates = Self::new().remove_xmp().remove_c2pa();
        updates.remove_all = true;
        updates
    }

    /// Create updates that set new XMP (legacy constructor)
    ///
    /// Prefer using `Updates::new().set_xmp(xmp)` for consistency with
    /// the builder pattern.
    pub fn with_xmp(xmp: Vec<u8>) -> Self {
        Self::new().set_xmp(xmp)
    }

    /// Create updates that set a new C2PA manifest store (legacy constructor)
    ///
    /// Prefer using `Updates::new().set_c2pa(store)` for consistency with
    /// the builder pattern.
    pub fn with_c2pa(store: Vec<u8>) -> Self {
        Self::new().set_c2pa(store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    struct Opt(u8);

    #[test]
    fn extensions_are_kept_per_type() {
        let u = Updates::new();
        assert!(u.extension::<Opt>().is_none());
        let u = u.set_extension(Opt(1)).set_extension(7u32);
        assert_eq!(u.extension::<Opt>(), Some(&Opt(1)));
        assert_eq!(u.extension::<u32>(), Some(&7));
        let u = u.set_extension(Opt(2));
        assert_eq!(u.extension::<Opt>(), Some(&Opt(2)));
        let u = u.clear_extension::<Opt>();
        assert!(u.extension::<Opt>().is_none());
        assert_eq!(u.extension::<u32>(), Some(&7));
    }

    #[test]
    fn remove_all_is_remembered() {
        assert!(!Updates::new().remove_xmp().remove_c2pa().removes_all());
        assert!(Updates::remove_all().removes_all());
    }
}
