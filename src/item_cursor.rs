// Copyright 2026 Adobe. All rights reserved.
// This file is licensed under the Apache License, Version 2.0 or the MIT license,
// at your option.

//! Reading an item's bytes a piece at a time, in bounded memory.
//!
//! An [`Item`](crate::Item) may be stored in several extents, in any physical
//! order, with unrelated bytes between them. [`ItemCursor`] maps the item's own
//! byte positions onto those extents. It does no I/O and holds no data: it says
//! which bytes of the source to read next, and the caller reads them however it
//! likes. Every driver is a loop around it:
//!
//! - [`ItemReader`]: [`Read`] + [`Seek`] over a [`Read`] + [`Seek`] source.
//! - [`Structure::stream_item`](crate::Structure::stream_item): a callback for
//!   each chunk, through a buffer the caller owns.
//! - [`Structure::stream_item_async`](crate::Structure::stream_item_async): the
//!   same, reading through an [`Io`](crate::io::Io), so it runs under
//!   [`Task`](crate::task::Task) or any async driver.
//!
//! None of them holds more than the buffer or window the caller chose, whatever
//! the size of the item.
//!
//! ```
//! # use asset_io::{ByteRange, ItemCursor, ReadAt};
//! # fn main() -> asset_io::Result<()> {
//! let source = b"..WORLD..HELLO ..";
//! // The item is "HELLO WORLD": two extents, stored in reverse order.
//! let mut cursor = ItemCursor::new([ByteRange::new(9, 6)?, ByteRange::new(2, 5)?])?;
//! let mut item = Vec::new();
//! while let Some(ReadAt { offset, len }) = cursor.next_read(4) {
//!     // Read `len` bytes at `offset` however the source allows.
//!     let at = offset as usize;
//!     item.extend_from_slice(&source[at..at + len]);
//!     cursor.advance(len);
//! }
//! assert_eq!(item, b"HELLO WORLD");
//! # Ok(())
//! # }
//! ```

use std::io::{self, Read, Seek, SeekFrom};

use crate::{
    error::{Error, Result},
    range::ByteRange,
};

/// A read to make: `len` bytes of the source stream, starting at `offset`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadAt {
    /// Where in the source stream the bytes are.
    pub offset: u64,
    /// How many bytes to read: at least 1, and at most the limit given to
    /// [`ItemCursor::next_read`].
    pub len: usize,
}

/// The extents of an item that hold bytes, in logical order.
#[derive(Debug, Clone)]
enum Runs {
    /// One run. An item with no bytes holds an empty range here.
    One(ByteRange),
    /// Two or more non-empty runs, and where each begins in the item's own bytes
    /// (`starts[0]` is 0, and the starts increase).
    Many {
        runs: Box<[ByteRange]>,
        starts: Box<[u64]>,
    },
}

/// A position in an item's bytes, and the reads that move it.
///
/// The cursor is the sans-I/O core of reading an item in pieces. Ask it for the
/// next read with [`next_read`](Self::next_read), make that read from the source,
/// and report how many bytes were consumed with [`advance`](Self::advance). A read
/// never crosses from one extent into the next, so it never covers a byte that is
/// not part of the item. [`seek`](Self::seek) moves anywhere in the item in
/// `O(log extents)` at the next read.
///
/// Extents with no bytes are dropped. The cursor holds one range per extent and
/// nothing else, so it is cheap to clone, and it does not borrow the structure
/// it came from. An item with a single extent does not allocate.
#[derive(Debug, Clone)]
pub struct ItemCursor {
    runs: Runs,
    len: u64,
    pos: u64,
}

impl ItemCursor {
    /// A cursor over the bytes of `ranges`, in that order, positioned at the start.
    ///
    /// The ranges are not checked against any stream: [`Structure::item_cursor`]
    /// does that. Fails if their lengths add up to more than `u64::MAX`.
    ///
    /// [`Structure::item_cursor`]: crate::Structure::item_cursor
    pub fn new(ranges: impl IntoIterator<Item = ByteRange>) -> Result<Self> {
        let mut ranges = ranges.into_iter().filter(|r| !r.is_empty());
        let Some(first) = ranges.next() else {
            return Ok(Self {
                runs: Runs::One(ByteRange::raw(0, 0)),
                len: 0,
                pos: 0,
            });
        };
        let Some(second) = ranges.next() else {
            return Ok(Self {
                len: first.len(),
                runs: Runs::One(first),
                pos: 0,
            });
        };
        let too_long = || Error::LimitExceeded {
            what: "item length",
            limit: u64::MAX,
        };
        let mut runs = vec![first, second];
        let mut starts = vec![0, first.len()];
        let mut len = first.len().checked_add(second.len()).ok_or_else(too_long)?;
        for range in ranges {
            starts.push(len);
            runs.push(range);
            len = len.checked_add(range.len()).ok_or_else(too_long)?;
        }
        Ok(Self {
            runs: Runs::Many {
                runs: runs.into_boxed_slice(),
                starts: starts.into_boxed_slice(),
            },
            len,
            pos: 0,
        })
    }

    /// The length of the item: the bytes of all its extents.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the item has no bytes.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The position, in the item's own bytes. It can be past the end after a
    /// [`seek`](Self::seek).
    pub fn pos(&self) -> u64 {
        self.pos
    }

    /// The bytes from the position to the end of the item.
    pub fn remaining(&self) -> u64 {
        self.len.saturating_sub(self.pos)
    }

    /// Move to `pos` in the item's own bytes. Any value is accepted; at or past the
    /// end there is nothing more to read.
    pub fn seek(&mut self, pos: u64) {
        self.pos = pos;
    }

    /// The next read to make: up to `max` bytes, all within one extent, starting at
    /// the position. `None` at the end of the item, or if `max` is 0.
    ///
    /// The position does not move until [`advance`](Self::advance), so a read that
    /// fails can be asked for again.
    pub fn next_read(&self, max: usize) -> Option<ReadAt> {
        if max == 0 || self.pos >= self.len {
            return None;
        }
        let (run, start) = match &self.runs {
            Runs::One(run) => (*run, 0),
            Runs::Many { runs, starts } => {
                // `starts[0]` is 0, so at least one start is not past the position.
                let i = starts
                    .partition_point(|&start| start <= self.pos)
                    .checked_sub(1)?;
                (*runs.get(i)?, *starts.get(i)?)
            }
        };
        let within = self.pos.checked_sub(start)?;
        let left = run.len().checked_sub(within)?;
        // `min` with a `usize` value, so the length fits in a `usize`.
        let len = left.min(max as u64) as usize;
        Some(ReadAt {
            offset: run.offset().checked_add(within)?,
            len,
        })
    }

    /// Move `n` bytes on, after reading them. `n` is the `len` of the last
    /// [`ReadAt`], or less if fewer bytes were read. The position stops at the end
    /// of the item.
    pub fn advance(&mut self, n: usize) {
        if self.pos < self.len {
            self.pos = self.pos.saturating_add(n as u64).min(self.len);
        }
    }
}

/// An item's bytes as a [`Read`] + [`Seek`] stream over the source it is stored in.
///
/// Positions are in the item's own bytes: the extents are joined in logical order
/// and nothing else of the source is visible. Reads go straight into the caller's
/// buffer and the reader holds no data, so its memory does not depend on the size
/// of the item. It reads from the source only what the caller asks for, one extent
/// at a time, so wrap it in a [`BufReader`](std::io::BufReader) if the caller
/// reads in small pieces. It seeks the source only when a read does not continue
/// the last one.
///
/// A source that ends before an extent does is an [`io::ErrorKind::UnexpectedEof`]
/// error, never a short item. Seeking past the end is allowed, and reading there
/// gives 0 bytes, as for a file.
///
/// Get one from [`Structure::item_reader`](crate::Structure::item_reader). To keep
/// the source and the structure in one [`Asset`](crate::Asset), take the cursor
/// first, which ends the borrow of the structure:
///
/// ```no_run
/// # use asset_io::{Asset, ItemKind, ItemReader};
/// # fn main() -> asset_io::Result<()> {
/// let mut asset = Asset::open(std::fs::File::open("photo.heic")?, None)?;
/// let cursor = match asset.item(ItemKind::EmbeddedAsset)? {
///     Some(item) => asset.structure().item_cursor(item)?,
///     None => return Ok(()),
/// };
/// let mut reader = ItemReader::new(asset.source_mut(), cursor);
/// std::io::copy(&mut reader, &mut std::io::sink())?;
/// # Ok(())
/// # }
/// ```
pub struct ItemReader<'a, R: ?Sized> {
    src: &'a mut R,
    cursor: ItemCursor,
    /// Where `src` is positioned, when that is known.
    at: Option<u64>,
}

impl<'a, R: ?Sized> ItemReader<'a, R> {
    /// A reader of the item `cursor` describes, from `src`, which must be the
    /// stream the cursor's ranges address. The reader starts at the cursor's
    /// position.
    pub fn new(src: &'a mut R, cursor: ItemCursor) -> Self {
        Self {
            src,
            cursor,
            at: None,
        }
    }

    /// The length of the item.
    pub fn len(&self) -> u64 {
        self.cursor.len()
    }

    /// Whether the item has no bytes.
    pub fn is_empty(&self) -> bool {
        self.cursor.is_empty()
    }

    /// The reader's cursor: the item's extents and the position in them.
    pub fn cursor(&self) -> &ItemCursor {
        &self.cursor
    }
}

impl<R: Read + Seek + ?Sized> Read for ItemReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let Some(ReadAt { offset, len }) = self.cursor.next_read(buf.len()) else {
            return Ok(0);
        };
        let wanted = buf.get_mut(..len).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "read longer than the buffer")
        })?;
        // The source's position is not known again until the read succeeds.
        let known = self.at.take();
        if known != Some(offset) {
            self.src.seek(SeekFrom::Start(offset))?;
        }
        let n = self.src.read(wanted)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the source ended before the item's extent did",
            ));
        }
        if n > len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the source read more bytes than were asked for",
            ));
        }
        self.at = offset.checked_add(n as u64);
        self.cursor.advance(n);
        Ok(n)
    }
}

impl<R: ?Sized> Seek for ItemReader<'_, R> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        // Wide enough that adding a signed offset to a `u64` cannot overflow.
        let pos = match from {
            SeekFrom::Start(p) => i128::from(p),
            SeekFrom::Current(d) => i128::from(self.cursor.pos()) + i128::from(d),
            SeekFrom::End(d) => i128::from(self.cursor.len()) + i128::from(d),
        };
        let pos = u64::try_from(pos).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek to a negative or overflowing position",
            )
        })?;
        self.cursor.seek(pos);
        Ok(pos)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn r(offset: u64, len: u64) -> ByteRange {
        ByteRange::new(offset, len).unwrap()
    }

    /// 1000 bytes that tell their own position apart.
    fn source() -> Vec<u8> {
        (0..1000u32).map(|i| (i % 251) as u8).collect()
    }

    fn model(src: &[u8], ranges: &[ByteRange]) -> Vec<u8> {
        ranges
            .iter()
            .flat_map(|r| src[r.offset() as usize..r.end() as usize].iter().copied())
            .collect()
    }

    fn drain(cursor: &mut ItemCursor, src: &[u8], max: usize) -> Vec<u8> {
        let mut out = Vec::new();
        while let Some(ReadAt { offset, len }) = cursor.next_read(max) {
            assert!((1..=max).contains(&len));
            let at = offset as usize;
            out.extend_from_slice(&src[at..at + len]);
            cursor.advance(len);
        }
        out
    }

    /// A source that counts how often it is seeked.
    struct Counting<'a> {
        inner: Cursor<&'a [u8]>,
        seeks: usize,
    }

    impl<'a> Counting<'a> {
        fn new(data: &'a [u8]) -> Self {
            Self {
                inner: Cursor::new(data),
                seeks: 0,
            }
        }
    }

    impl Read for Counting<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.inner.read(buf)
        }
    }

    impl Seek for Counting<'_> {
        fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
            self.seeks += 1;
            self.inner.seek(from)
        }
    }

    /// xorshift64: enough randomness for a model test, and the same every run.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    #[test]
    fn one_extent_is_cut_into_chunks() {
        let cursor = ItemCursor::new([r(10, 25)]).unwrap();
        assert_eq!(cursor.len(), 25);
        let mut reads = Vec::new();
        let mut cursor = cursor;
        while let Some(read) = cursor.next_read(10) {
            reads.push((read.offset, read.len));
            cursor.advance(read.len);
        }
        assert_eq!(reads, [(10, 10), (20, 10), (30, 5)]);
        assert_eq!(cursor.remaining(), 0);
        assert_eq!(cursor.next_read(10), None);
    }

    #[test]
    fn a_read_never_crosses_an_extent() {
        // Out of physical order, with an empty extent that is dropped.
        let mut cursor = ItemCursor::new([r(100, 5), r(7, 0), r(50, 7), r(300, 1)]).unwrap();
        assert_eq!(cursor.len(), 13);
        let mut reads = Vec::new();
        while let Some(read) = cursor.next_read(4) {
            reads.push((read.offset, read.len));
            cursor.advance(read.len);
        }
        assert_eq!(reads, [(100, 4), (104, 1), (50, 4), (54, 3), (300, 1)]);
    }

    #[test]
    fn any_chunk_size_gives_the_same_bytes() {
        let src = source();
        let ranges = [r(900, 100), r(0, 0), r(10, 1), r(500, 333), r(0, 40)];
        let expected = model(&src, &ranges);
        for max in (1..=40).chain([333, 334, 1000, usize::MAX]) {
            let mut cursor = ItemCursor::new(ranges).unwrap();
            assert_eq!(drain(&mut cursor, &src, max), expected, "max {max}");
        }
    }

    #[test]
    fn no_bytes_means_no_reads() {
        for ranges in [vec![], vec![r(5, 0)], vec![r(5, 0), r(9, 0)]] {
            let cursor = ItemCursor::new(ranges).unwrap();
            assert!(cursor.is_empty());
            assert_eq!(cursor.next_read(8), None);
        }
        let cursor = ItemCursor::new([r(0, 4)]).unwrap();
        assert_eq!(cursor.next_read(0), None);
    }

    #[test]
    fn seek_and_advance_stay_in_bounds() {
        let mut cursor = ItemCursor::new([r(0, 10)]).unwrap();
        cursor.seek(u64::MAX);
        assert_eq!(cursor.next_read(4), None);
        assert_eq!(cursor.remaining(), 0);
        cursor.advance(5);
        assert_eq!(
            cursor.pos(),
            u64::MAX,
            "advance does not move a cursor past the end"
        );
        cursor.seek(8);
        cursor.advance(usize::MAX);
        assert_eq!(cursor.pos(), 10);
    }

    #[test]
    fn seek_lands_in_the_right_extent() {
        let mut cursor = ItemCursor::new([r(100, 5), r(50, 7), r(300, 1)]).unwrap();
        for (pos, expected) in [(0, 100), (4, 104), (5, 50), (11, 56), (12, 300)] {
            cursor.seek(pos);
            let read = cursor.next_read(100).unwrap();
            assert_eq!(read.offset, expected, "position {pos}");
        }
        cursor.seek(13);
        assert_eq!(cursor.next_read(100), None);
    }

    #[test]
    fn a_length_that_overflows_is_an_error() {
        let all = ByteRange::new(0, u64::MAX).unwrap();
        assert!(matches!(
            ItemCursor::new([all, r(0, 1)]),
            Err(Error::LimitExceeded { .. })
        ));
        assert!(matches!(
            ItemCursor::new([r(0, 1), all]),
            Err(Error::LimitExceeded { .. })
        ));
        assert!(ItemCursor::new([all]).is_ok());
    }

    #[test]
    fn reader_gives_the_bytes_of_the_extents_only() {
        let src = source();
        let ranges = [r(700, 50), r(20, 0), r(10, 30), r(990, 10)];
        let mut input = Cursor::new(&src);
        let mut reader = ItemReader::new(&mut input, ItemCursor::new(ranges).unwrap());
        assert_eq!(reader.len(), 90);
        let mut out = Vec::new();
        reader.read_to_end(&mut out).unwrap();
        assert_eq!(out, model(&src, &ranges));
        // At the end, and past it.
        assert_eq!(reader.read(&mut [0u8; 4]).unwrap(), 0);
        assert_eq!(reader.seek(SeekFrom::Start(1_000_000)).unwrap(), 1_000_000);
        assert_eq!(reader.read(&mut [0u8; 4]).unwrap(), 0);
    }

    #[test]
    fn reader_and_seek_match_a_vec() {
        let src = source();
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        for _ in 0..200 {
            let ranges: Vec<_> = (0..rng.below(6))
                .map(|_| {
                    let offset = rng.below(900);
                    r(offset, rng.below(101))
                })
                .collect();
            let expected = model(&src, &ranges);
            let len = expected.len() as i64;
            let mut input = Cursor::new(&src);
            let mut reader = ItemReader::new(&mut input, ItemCursor::new(ranges.clone()).unwrap());
            let mut oracle = Cursor::new(expected);
            for _ in 0..40 {
                if rng.below(3) == 0 {
                    let span = 2 * len + 40;
                    let from = match rng.below(3) {
                        0 => SeekFrom::Start(rng.below(span as u64 + 1)),
                        1 => SeekFrom::Current(rng.below(span as u64 + 1) as i64 - len - 20),
                        _ => SeekFrom::End(rng.below(span as u64 + 1) as i64 - len - 20),
                    };
                    let (got, want) = (reader.seek(from), oracle.seek(from));
                    assert_eq!(got.is_ok(), want.is_ok(), "{ranges:?} {from:?}");
                    if let (Ok(got), Ok(want)) = (got, want) {
                        assert_eq!(got, want, "{ranges:?} {from:?}");
                    }
                } else {
                    let mut got = vec![0u8; rng.below(70) as usize];
                    let mut want = vec![0u8; got.len()];
                    let got_n = fill(&mut reader, &mut got);
                    let want_n = fill(&mut oracle, &mut want);
                    assert_eq!(got_n, want_n, "{ranges:?}");
                    assert_eq!(got[..got_n], want[..want_n], "{ranges:?}");
                }
            }
        }
    }

    /// Read until `buf` is full or the stream ends.
    fn fill<T: Read>(from: &mut T, buf: &mut [u8]) -> usize {
        let mut n = 0;
        while n < buf.len() {
            match from.read(&mut buf[n..]).unwrap() {
                0 => break,
                k => n += k,
            }
        }
        n
    }

    #[test]
    fn negative_seeks_are_errors_and_leave_the_position() {
        let src = source();
        let mut input = Cursor::new(&src);
        let mut reader = ItemReader::new(&mut input, ItemCursor::new([r(0, 10)]).unwrap());
        assert_eq!(reader.seek(SeekFrom::Start(4)).unwrap(), 4);
        assert!(reader.seek(SeekFrom::Current(-5)).is_err());
        assert!(reader.seek(SeekFrom::End(-11)).is_err());
        assert_eq!(reader.stream_position().unwrap(), 4);
        assert_eq!(reader.seek(SeekFrom::End(-1)).unwrap(), 9);
    }

    #[test]
    fn reads_that_continue_do_not_seek() {
        let src = source();
        // One extent, read in small pieces: one seek.
        let mut input = Counting::new(&src);
        let mut reader = ItemReader::new(&mut input, ItemCursor::new([r(100, 50)]).unwrap());
        let mut piece = [0u8; 7];
        while reader.read(&mut piece).unwrap() > 0 {}
        assert_eq!(input.seeks, 1);
        // Two extents with a gap: one seek for each.
        let mut input = Counting::new(&src);
        let mut reader = ItemReader::new(
            &mut input,
            ItemCursor::new([r(100, 50), r(200, 50)]).unwrap(),
        );
        let mut out = Vec::new();
        reader.read_to_end(&mut out).unwrap();
        assert_eq!(out.len(), 100);
        assert_eq!(input.seeks, 2);
        // Extents that are adjacent in the source read on without a seek.
        let mut input = Counting::new(&src);
        let mut reader = ItemReader::new(
            &mut input,
            ItemCursor::new([r(100, 50), r(150, 50)]).unwrap(),
        );
        reader.read_to_end(&mut out).unwrap();
        assert_eq!(input.seeks, 1);
    }

    #[test]
    fn a_short_source_is_an_error_not_an_early_end() {
        let src = source();
        // The extent runs to 1100; the source has 1000 bytes.
        let mut input = Cursor::new(&src);
        let mut reader = ItemReader::new(&mut input, ItemCursor::new([r(900, 200)]).unwrap());
        let mut out = Vec::new();
        let err = reader.read_to_end(&mut out).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(out.len(), 100, "the bytes that exist are delivered first");
    }

    #[test]
    fn a_reader_continues_from_the_cursor_position() {
        let src = source();
        let mut cursor = ItemCursor::new([r(0, 100), r(500, 100)]).unwrap();
        cursor.seek(150);
        let mut input = Cursor::new(&src);
        let mut reader = ItemReader::new(&mut input, cursor);
        let mut out = Vec::new();
        reader.read_to_end(&mut out).unwrap();
        assert_eq!(out, &src[550..600]);
    }
}
