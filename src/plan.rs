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

//! Declarative write plans.
//!
//! A handler describes a write as data: the structure of the output and an ordered
//! list of [`Step`]s that produce it. It does not touch the output stream or the
//! sink. [`WritePlan::execute`] is the reference executor: it runs the steps
//! synchronously, coalesces reads, delivers every byte to the sink tagged with its
//! segment and role, and checks that the bytes land where the structure says. A caller
//! can run the same plan some other way (asynchronously, or from a state machine
//! that does no I/O itself), because the plan is plain data.

use std::{
    borrow::Cow,
    io::{Read, Seek, SeekFrom, Write},
    sync::Arc,
};

use crate::{
    error::{Error, Result},
    execute::{Action, Execution},
    range::ByteRange,
    segment::{Role, Segment},
    sink::Sink,
    structure::{SegmentId, Structure},
    task::Task,
};

/// Where bytes moved: `(end of the old region, size change)` for each edit.
///
/// A file offset that points at or past the end of an edited region moves by that
/// edit's size change. Containers that store absolute file offsets (ISO BMFF chunk
/// tables, for one) use it to say how those offsets change when an edit shifts the
/// data after it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OffsetMap {
    edits: Vec<(u64, i64)>,
}

impl OffsetMap {
    /// A map that moves nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that the region of the source that ended at `old_end` changed size
    /// by `delta` bytes. A zero change is not recorded.
    pub fn add(&mut self, old_end: u64, delta: i64) {
        if delta != 0 {
            self.edits.push((old_end, delta));
        }
    }

    /// Whether no offset moves.
    pub fn is_identity(&self) -> bool {
        self.edits.is_empty()
    }

    /// Where the source offset `v` is in the output.
    pub fn apply(&self, v: u64) -> Result<u64> {
        let shift: i128 = self
            .edits
            .iter()
            .filter(|(end, _)| *end <= v)
            .map(|(_, d)| *d as i128)
            .sum();
        u64::try_from(v as i128 + shift)
            .map_err(|_| Error::InvalidFormat("offset adjustment out of range".into()))
    }
}

/// Largest entry [`Relocate`] handles.
pub const MAX_RELOCATE_STRIDE: u32 = 4096;

/// How the bytes of a [`Step::Relocate`] are rewritten: they are a table of
/// `stride`-byte entries, each holding a big-endian file offset `width` bytes
/// wide (4 or 8) at `field_off`, and every offset is replaced by its position in
/// the output according to an [`OffsetMap`]. The rest of each entry is unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relocate {
    map: Arc<OffsetMap>,
    stride: u32,
    field_off: u32,
    width: u8,
}

impl Relocate {
    /// A rewrite rule. Fails if `width` is not 4 or 8, the field does not fit in an
    /// entry, or the entry is empty or longer than [`MAX_RELOCATE_STRIDE`].
    pub fn new(map: Arc<OffsetMap>, stride: u32, field_off: u32, width: u8) -> Result<Self> {
        let fits = field_off
            .checked_add(width as u32)
            .is_some_and(|end| end <= stride);
        if !matches!(width, 4 | 8) || stride == 0 || stride > MAX_RELOCATE_STRIDE || !fits {
            return Err(Error::InvalidFormat("invalid offset table geometry".into()));
        }
        Ok(Self {
            map,
            stride,
            field_off,
            width,
        })
    }

    /// Bytes per entry.
    pub fn stride(&self) -> u32 {
        self.stride
    }

    /// Rewrite the offset in each whole entry of `block`.
    pub(crate) fn apply(&self, block: &mut [u8]) -> Result<()> {
        let (off, w) = (self.field_off as usize, self.width as usize);
        for entry in block.chunks_exact_mut(self.stride as usize) {
            let f = &mut entry[off..off + w];
            let v = self
                .map
                .apply(f.iter().fold(0u64, |a, &b| (a << 8) | b as u64))?;
            let range = || Error::InvalidFormat("offset adjustment does not fit its field".into());
            match w {
                4 => f.copy_from_slice(&u32::try_from(v).map_err(|_| range())?.to_be_bytes()),
                _ => f.copy_from_slice(&v.to_be_bytes()),
            }
        }
        Ok(())
    }
}

/// One piece of the output.
///
/// Steps are executed in order and must tile the output: each step supplies the
/// next bytes of segment `dest`, in the `role` (header, payload or trailer) of that
/// segment's structure, and the segments follow one another as the structure lists them.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Step<'a> {
    /// Copy bytes of the source asset.
    Copy {
        /// The output segment the bytes belong to.
        dest: SegmentId,
        /// Which part of that segment they are.
        role: Role,
        /// The source bytes.
        src: ByteRange,
    },
    /// Write bytes the handler supplies.
    Put {
        /// The output segment the bytes belong to.
        dest: SegmentId,
        /// Which part of that segment they are.
        role: Role,
        /// The bytes.
        data: Cow<'a, [u8]>,
    },
    /// Copy bytes of the source asset that are a table of file offsets, rewriting
    /// the offsets. The source range is a whole number of entries.
    Relocate {
        /// The output segment the bytes belong to.
        dest: SegmentId,
        /// Which part of that segment they are.
        role: Role,
        /// The source bytes.
        src: ByteRange,
        /// How the offsets change.
        rule: Relocate,
    },
}

impl Step<'_> {
    /// The output segment this step writes to.
    pub fn dest(&self) -> SegmentId {
        match self {
            Step::Copy { dest, .. } | Step::Put { dest, .. } | Step::Relocate { dest, .. } => *dest,
        }
    }

    /// Which part of the output segment this step writes.
    pub fn role(&self) -> Role {
        match self {
            Step::Copy { role, .. } | Step::Put { role, .. } | Step::Relocate { role, .. } => *role,
        }
    }

    /// How many output bytes this step produces.
    pub fn len(&self) -> u64 {
        match self {
            Step::Copy { src, .. } | Step::Relocate { src, .. } => src.len(),
            Step::Put { data, .. } => data.len() as u64,
        }
    }

    /// Whether this step produces no bytes.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A write, described but not performed.
///
/// Holds the [`Structure`] of the output and the [`Step`]s that produce it.
/// Borrows from the [`Updates`](crate::Updates) it was made from, so the new
/// metadata is not copied.
#[derive(Debug)]
pub struct WritePlan<'a> {
    structure: Structure,
    steps: Vec<Step<'a>>,
}

impl<'a> WritePlan<'a> {
    /// A plan for the output `structure`, with no steps yet.
    pub fn new(structure: Structure) -> Self {
        Self {
            structure,
            steps: Vec::new(),
        }
    }

    pub(crate) fn from_parts(structure: Structure, steps: Vec<Step<'a>>) -> Self {
        Self { structure, steps }
    }

    /// Append a step that copies `src` of the source asset into `role` of `dest`.
    pub fn copy(&mut self, dest: SegmentId, role: Role, src: ByteRange) -> &mut Self {
        self.steps.push(Step::Copy { dest, role, src });
        self
    }

    /// Append a step that writes `data` as `role` of `dest`.
    pub fn put(
        &mut self,
        dest: SegmentId,
        role: Role,
        data: impl Into<Cow<'a, [u8]>>,
    ) -> &mut Self {
        self.steps.push(Step::Put {
            dest,
            role,
            data: data.into(),
        });
        self
    }

    /// Append a step that copies `src` of the source asset into `role` of `dest`,
    /// rewriting the file offsets in it as `rule` says.
    pub fn relocate(
        &mut self,
        dest: SegmentId,
        role: Role,
        src: ByteRange,
        rule: Relocate,
    ) -> &mut Self {
        self.steps.push(Step::Relocate {
            dest,
            role,
            src,
            rule,
        });
        self
    }

    /// Replace the steps that write `role` of `dest` with one that writes `data`,
    /// for bytes that depend on the rest of the output. It must be as long as what
    /// it replaces. Fails if `dest` has no step for `role`.
    pub fn replace(
        &mut self,
        dest: SegmentId,
        role: Role,
        data: impl Into<Cow<'a, [u8]>>,
    ) -> Result<&mut Self> {
        let hit = |s: &Step<'_>| s.dest() == dest && s.role() == role;
        let at = self
            .steps
            .iter()
            .position(hit)
            .ok_or(Error::NotFound { what: "step" })?;
        self.steps.retain(|s| !hit(s));
        self.steps.insert(
            at,
            Step::Put {
                dest,
                role,
                data: data.into(),
            },
        );
        Ok(self)
    }

    /// Append the steps that copy the source segment `src` unchanged into `dest`.
    pub fn copy_segment(&mut self, dest: SegmentId, src: &Segment) -> &mut Self {
        for (role, range) in src.regions() {
            self.copy(dest, role, range);
        }
        self
    }

    /// The structure of the output.
    pub fn structure(&self) -> &Structure {
        &self.structure
    }

    /// The steps, in output order.
    pub fn steps(&self) -> &[Step<'a>] {
        &self.steps
    }

    /// The structure of the output, discarding the steps.
    pub fn into_structure(self) -> Structure {
        self.structure
    }

    /// Where each step starts in the output, plus the end of the last one.
    fn step_offsets(&self) -> Vec<u64> {
        let mut offsets = Vec::with_capacity(self.steps.len() + 1);
        let mut at = 0u64;
        offsets.push(at);
        for step in &self.steps {
            at = at.saturating_add(step.len());
            offsets.push(at);
        }
        offsets
    }

    /// The steps that produce any byte of `range` of the output, in order.
    fn steps_over<'p>(
        &'p self,
        offsets: &'p [u64],
        range: ByteRange,
    ) -> impl Iterator<Item = &'p Step<'a>> + 'p {
        // The first step ending after the range starts.
        let first = offsets[1..].partition_point(|&end| end <= range.offset());
        self.steps[first..]
            .iter()
            .zip(&offsets[first..])
            .take_while(move |(_, &start)| start < range.end())
            .filter(|(step, _)| !step.is_empty())
            .map(|(step, _)| step)
    }

    /// Whether any byte of `range` of the output is written by a
    /// [`Step::Relocate`]: a table of file offsets that the write rewrites, so its
    /// output differs from its source.
    pub fn is_relocated(&self, range: ByteRange) -> bool {
        let offsets = self.step_offsets();
        let relocated = self
            .steps_over(&offsets, range)
            .any(|step| matches!(step, Step::Relocate { .. }));
        relocated
    }

    /// The items of `kind` in the output whose bytes the plan writes from data it
    /// was given ([`Step::Put`]) rather than copying from the source: the items an
    /// [`Updates`](crate::Updates) value set or replaced. Items the write keeps are
    /// copied and not listed. Reads nothing.
    pub fn written_items(&self, kind: crate::ItemKind) -> impl Iterator<Item = &crate::Item> + '_ {
        let offsets = self.step_offsets();
        self.structure.items_of(kind).filter(move |item| {
            let mut extents = item.extents().peekable();
            extents.peek().is_some()
                && extents.all(|extent| {
                    self.steps_over(&offsets, extent.range())
                        .all(|step| matches!(step, Step::Put { .. }))
                })
        })
    }

    /// The output as a reader, without writing it: reads return the bytes
    /// [`execute`](Self::execute) would write, taking source bytes from `src` (the
    /// stream the plan was made from) and rewriting relocated offsets. Only the
    /// requested bytes are read; nothing is buffered beyond one offset-table entry.
    ///
    /// For inspecting a planned output, such as walking its structure or reading
    /// an item, before committing to the write.
    pub fn reader<R: Read + Seek>(&self, src: R) -> PlanReader<'_, 'a, R> {
        PlanReader {
            offsets: self.step_offsets(),
            plan: self,
            src,
            position: 0,
            entry: Vec::new(),
        }
    }

    /// Start carrying the plan out, step by step, with the I/O left to the caller;
    /// see [`Execution`].
    pub fn start(&self) -> Execution<'_, 'a> {
        Execution::new(self)
    }

    /// Perform the write: run every step against `src`, write the bytes to `out`
    /// and deliver them to `sink`. `out` is written from its current position;
    /// offsets reported to the sink are relative to the first byte written.
    ///
    /// Fails, and may leave partial output, if the steps do not produce exactly
    /// the structure: bytes at the wrong offset, in the wrong role, or too few or too
    /// many of them.
    ///
    /// This drives [`start`](Self::start) with blocking reads and writes.
    pub fn execute<R, W, S>(&self, src: &mut R, out: &mut W, sink: &mut S) -> Result<()>
    where
        R: Read + Seek + ?Sized,
        W: Write + ?Sized,
        S: Sink + ?Sized,
    {
        let mut run = self.start();
        // Where the source stream is, so that a read that continues the last one
        // does not seek.
        let mut at: Option<u64> = None;
        loop {
            match run.step(sink)? {
                Action::Read { offset, buf } => {
                    if at != Some(offset) {
                        src.seek(SeekFrom::Start(offset))?;
                    }
                    src.read_exact(buf)?;
                    at = Some(offset + buf.len() as u64);
                }
                Action::Write(bytes) => out.write_all(bytes)?,
                Action::Done => break,
            }
        }
        out.flush()?;
        Ok(())
    }
}

/// A planned output read without writing it; see [`WritePlan::reader`].
#[derive(Debug)]
pub struct PlanReader<'p, 'a, R> {
    plan: &'p WritePlan<'a>,
    offsets: Vec<u64>,
    src: R,
    position: u64,
    entry: Vec<u8>,
}

impl<R> PlanReader<'_, '_, R> {
    /// The source stream, giving it back.
    pub fn into_inner(self) -> R {
        self.src
    }
}

impl<R: Read + Seek> Read for PlanReader<'_, '_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let index = self.offsets[1..].partition_point(|&end| end <= self.position);
        let Some(step) = self.plan.steps.get(index) else {
            return Ok(0);
        };
        let relative = self.position - self.offsets[index];
        let len = (buf.len() as u64).min(step.len() - relative) as usize;
        let n = match step {
            Step::Copy { src, .. } => {
                self.src.seek(SeekFrom::Start(src.offset() + relative))?;
                self.src.read(&mut buf[..len])?
            }
            Step::Put { data, .. } => {
                let start = relative as usize;
                buf[..len].copy_from_slice(&data[start..start + len]);
                len
            }
            Step::Relocate { src, rule, .. } => {
                // Rewrite the whole entry holding `relative`, then hand out its bytes.
                let stride = u64::from(rule.stride());
                let entry_start = relative - relative % stride;
                self.entry.resize(stride as usize, 0);
                self.src.seek(SeekFrom::Start(src.offset() + entry_start))?;
                self.src.read_exact(&mut self.entry)?;
                rule.apply(&mut self.entry).map_err(std::io::Error::other)?;
                let at = (relative - entry_start) as usize;
                let n = len.min(self.entry.len() - at);
                buf[..n].copy_from_slice(&self.entry[at..at + n]);
                n
            }
        };
        self.position += n as u64;
        Ok(n)
    }
}

impl<R> Seek for PlanReader<'_, '_, R> {
    fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
        let base = match from {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::Current(delta) => self.position.checked_add_signed(delta),
            SeekFrom::End(delta) => self.plan.structure.total_size().checked_add_signed(delta),
        };
        self.position = base.ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid planned seek")
        })?;
        Ok(self.position)
    }
}

impl Structure {
    /// The plan for writing this asset with `updates`; see [`WritePlan`]. `src` must
    /// be the stream this structure was parsed from. The container's handler builds
    /// it, and reports [`Error::UnsupportedFormat`] if it describes its writes some
    /// other way.
    ///
    /// An XMP or C2PA value that `updates` sets must appear in the plan's output;
    /// if the handler cannot place it the plan is [`Error::UnsupportedItem`].
    pub fn write_plan<'a>(
        &self,
        src: &mut dyn crate::handler::ReadSeek,
        updates: &'a crate::Updates,
    ) -> Result<WritePlan<'a>> {
        let plan = self.handler().write_plan(self, src, updates)?;
        check_updates_placed(&plan, updates)?;
        Ok(plan)
    }
}

/// [`Error::UnsupportedItem`] for a value `updates` sets that the plan does not
/// write, so no handler can silently drop one.
pub(crate) fn check_updates_placed(plan: &WritePlan<'_>, updates: &crate::Updates) -> Result<()> {
    use crate::{updates::MetadataUpdate, ItemKind};
    for (kind, update) in [
        (ItemKind::Xmp, updates.xmp()),
        (ItemKind::C2pa, updates.c2pa()),
    ] {
        if matches!(update, MetadataUpdate::Set(data) if !data.is_empty())
            && plan.written_items(kind).next().is_none()
        {
            return Err(Error::UnsupportedItem(kind));
        }
    }
    Ok(())
}

impl Structure {
    /// A task that builds the plan for writing this asset with `updates`, for a
    /// caller that does its own I/O; see [`Task`]. The task reads the source (a
    /// stream of [`total_size`](Self::total_size) bytes), which it asks for as it
    /// needs it. The result is the plan [`write_plan`](Self::write_plan) returns.
    pub fn write_plan_task<'f, 'a: 'f>(
        &'f self,
        updates: &'a crate::Updates,
    ) -> Task<'f, WritePlan<'a>> {
        Task::new(self.total_size(), move |io| async move {
            let plan = self.handler().write_plan_async(self, io, updates).await?;
            check_updates_placed(&plan, updates)?;
            Ok(plan)
        })
    }
}

/// Checks a plan's steps against its structure without performing any I/O.
///
/// Useful in tests of a handler: the steps must tile the structure exactly, in the
/// structure's segment and role order, and copy only from inside `source_len` bytes.
pub fn validate(plan: &WritePlan<'_>, source_len: u64) -> Result<()> {
    let structure = plan.structure();
    let mut pos = 0u64;
    for step in plan.steps() {
        let seg = structure.segment(step.dest())?;
        if step.is_empty() {
            continue;
        }
        let region = match step.role() {
            Role::Header => seg.header(),
            Role::Payload => seg.payload(),
            Role::Trailer => seg.trailer(),
        };
        let end = pos.checked_add(step.len());
        let fits = region.offset() <= pos && end.is_some_and(|e| e <= region.end());
        if !fits {
            return Err(Error::invalid_segment(
                pos,
                format!(
                    "step of {} bytes does not fit the {:?} of its segment",
                    step.len(),
                    step.role()
                ),
            ));
        }
        if let Step::Relocate { src, rule, .. } = step {
            if !src.len().is_multiple_of(rule.stride() as u64) {
                return Err(Error::invalid_segment(
                    src.offset(),
                    "offset table is not a whole number of entries",
                ));
            }
        }
        if let Step::Copy { src, .. } | Step::Relocate { src, .. } = step {
            if src.end() > source_len {
                return Err(Error::invalid_segment(
                    src.offset(),
                    "copy reads past the end of the source",
                ));
            }
        }
        pos += step.len();
    }
    if pos != structure.total_size() {
        return Err(Error::invalid_segment(
            pos,
            format!(
                "steps write {pos} bytes, structure is {}",
                structure.total_size()
            ),
        ));
    }
    Ok(())
}
