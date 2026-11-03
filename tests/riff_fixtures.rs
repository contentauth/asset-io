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

//! RIFF handler on the real files in tests/fixtures.

#![cfg(feature = "riff")]

use asset_io::{Asset, HandlerRegistry, ItemKind, NullSink, Updates};
use std::io::Cursor;

const FILES: &[&str] = &[
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample1.webp"),
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample1.wav"),
];

/// `bytes` as an asset, parsed by the RIFF handler alone.
fn open(bytes: &[u8]) -> asset_io::Result<Asset<Cursor<&[u8]>>> {
    let registry = HandlerRegistry::new().with_handler(asset_io::containers::riff::RiffHandler);
    Asset::open_with(&registry, Cursor::new(bytes), None)
}

fn write(bytes: &[u8], u: &Updates) -> asset_io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let dest = open(bytes)?.write(&mut out, u, &mut NullSink)?;
    let again = open(&out)?;
    assert_eq!(dest.segments(), again.segments());
    assert_eq!(dest.total_size(), out.len() as u64);
    Ok(out)
}

#[test]
fn real_files_round_trip_and_take_metadata() {
    let mut ran = 0;
    for path in FILES {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        ran += 1;
        let s = open(&bytes).unwrap_or_else(|e| panic!("{path}: {e}"));
        let covered: u64 = s.regions().map(|r| r.range.len()).sum();
        assert_eq!(covered, bytes.len() as u64, "{path}");

        assert_eq!(
            write(&bytes, &Updates::new()).unwrap(),
            bytes,
            "{path}: keep"
        );

        let u = Updates::new()
            .set_jumbf(vec![0xAB; 1001])
            .set_xmp(b"<x:xmpmeta/>".to_vec());
        let out = write(&bytes, &u).unwrap_or_else(|e| panic!("{path}: {e}"));
        let t = open(&out).unwrap();
        for (kind, want) in [
            (ItemKind::Jumbf, vec![0xAB; 1001]),
            (ItemKind::Xmp, b"<x:xmpmeta/>".to_vec()),
        ] {
            let e = t.require_item(kind).unwrap();
            assert_eq!(t.read_item(e).unwrap(), want, "{path}");
        }

        // Removing what was added gives back the media, whatever else changed
        // (a WebP gains a VP8X chunk when it takes XMP).
        let back = write(&out, &Updates::new().remove_jumbf().remove_xmp()).unwrap();
        let b = open(&back).unwrap();
        assert!(b.item(ItemKind::Jumbf).unwrap().is_none(), "{path}");
        assert!(b.item(ItemKind::Xmp).unwrap().is_none(), "{path}");
    }
    assert!(ran > 0, "no RIFF fixtures found");
}
