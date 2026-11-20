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

#![no_main]

//! Item reads: the fuzzer's bytes are a source, a list of extents and a list of
//! reads and seeks. Over extents inside the source, an `ItemReader` must read and
//! seek exactly like the concatenation of those extents. Over extents that may lie
//! past the end of the source, it may fail but must not panic, and must never give
//! more bytes than the item has.

use std::io::{Cursor, Read, Seek, SeekFrom};

use asset_io::{ByteRange, ItemCursor, ItemReader};
use libfuzzer_sys::fuzz_target;

/// Read until `buf` is full or the stream ends.
fn fill<T: Read>(from: &mut T, buf: &mut [u8]) -> usize {
    let mut n = 0;
    while n < buf.len() {
        match from.read(&mut buf[n..]).expect("a read inside the source") {
            0 => break,
            k => n += k,
        }
    }
    n
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    let src_len = (u16::from_le_bytes([data[0], data[1]]) as usize).min(data.len() - 2);
    let (src, rest) = data[2..].split_at(src_len);
    // Up to 16 extents of four bytes: offset and length, each a `u16`.
    let split = (rest.len() / 4).min(16) * 4;
    let (extent_bytes, op_bytes) = rest.split_at(split);

    let mut inside = Vec::new();
    let mut anywhere = Vec::new();
    for e in extent_bytes.chunks_exact(4) {
        let offset = u64::from(u16::from_le_bytes([e[0], e[1]]));
        let len = u64::from(u16::from_le_bytes([e[2], e[3]]));
        if let Ok(range) = ByteRange::new(offset, len) {
            anywhere.push(range);
        }
        let at = offset.min(src.len() as u64);
        let len = len.min(src.len() as u64 - at);
        if let Ok(range) = ByteRange::new(at, len) {
            inside.push(range);
        }
    }

    // Extents that may run past the source: no panic, and no extra bytes.
    if let Ok(cursor) = ItemCursor::new(anywhere.iter().copied()) {
        let mut input = Cursor::new(src);
        let mut reader = ItemReader::new(&mut input, cursor);
        let mut out = Vec::new();
        let _ = reader.read_to_end(&mut out);
        assert!(out.len() as u64 <= reader.len());
    }

    // Extents inside the source: the reader matches the concatenation.
    let expected: Vec<u8> = inside
        .iter()
        .flat_map(|r| src[r.offset() as usize..r.end() as usize].iter().copied())
        .collect();
    let Ok(cursor) = ItemCursor::new(inside.iter().copied()) else {
        return;
    };
    assert_eq!(cursor.len(), expected.len() as u64);
    let mut input = Cursor::new(src);
    let mut reader = ItemReader::new(&mut input, cursor);
    let mut oracle = Cursor::new(expected);
    for op in op_bytes.chunks_exact(3) {
        let (kind, a, b) = (op[0], i64::from(op[1]), usize::from(op[2]));
        if kind % 3 == 0 {
            let from = match kind / 3 % 3 {
                0 => SeekFrom::Start(a as u64 * 7),
                1 => SeekFrom::Current(a - 128),
                _ => SeekFrom::End(a - 128),
            };
            assert_eq!(reader.seek(from).ok(), oracle.seek(from).ok());
        } else {
            let mut got = vec![0u8; b];
            let mut want = vec![0u8; b];
            let got_n = fill(&mut reader, &mut got);
            let want_n = fill(&mut oracle, &mut want);
            assert_eq!(got_n, want_n);
            assert_eq!(got[..got_n], want[..want_n]);
        }
    }
});
