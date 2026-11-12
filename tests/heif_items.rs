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

//! HEIF items (EXIF, thumbnails) on the real files in tests/fixtures.

#![cfg(feature = "bmff")]

use std::io::{Cursor, Read, Seek};

use asset_io::{
    container_api, container_api::ItemInfo, Asset, HandlerRegistry, ItemKind, NullSink,
    RelationKind, Structure, Updates,
};

const FILES: &[&str] = &[
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample1.heic"),
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample1.heif"),
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample1.avif"),
];

/// `bytes` as an asset, parsed by the BMFF handler alone.
fn open(bytes: &[u8]) -> asset_io::Result<Asset<Cursor<&[u8]>>> {
    let registry = HandlerRegistry::new().with_handler(asset_io::containers::bmff::BmffHandler);
    Asset::open_with(&registry, Cursor::new(bytes), None)
}

/// Every HEIF item (those with an item id): what it is and its bytes.
fn items<S: Read + Seek>(asset: &Asset<S>) -> Vec<(ItemInfo, Vec<u8>)> {
    asset
        .items()
        .filter(|e| e.native_id().is_some())
        .map(|e| {
            (
                container_api::item_info(e).clone(),
                asset.read_item(e).unwrap(),
            )
        })
        .collect()
}

fn is_thumbnail(i: &(ItemInfo, Vec<u8>), s: &Structure) -> bool {
    s.items().any(|e| {
        container_api::item_info(e) == &i.0
            && e.relations()
                .iter()
                .any(|r| r.kind() == RelationKind::Thumbnail)
    })
}

#[test]
fn items_survive_a_write_and_stay_readable() {
    let mut ran = 0;
    for path in FILES {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        ran += 1;
        let s = open(&bytes).unwrap_or_else(|e| panic!("{path}: {e}"));
        let before = items(&s);
        eprintln!(
            "{path}: {} exif, {} thumbnail",
            s.items_of(ItemKind::Exif).count(),
            before.iter().filter(|i| is_thumbnail(i, &s)).count()
        );

        // Writing a manifest moves the data; the items must move with it, in both
        // the structure the write returns and a fresh parse of the output.
        let mut out = Vec::new();
        let dest = s
            .write(
                &mut out,
                &Updates::new().set_c2pa(vec![0u8; 100]),
                &mut NullSink,
            )
            .unwrap();
        let dest = Asset::from_parts(dest, Cursor::new(&out[..]));
        let again = open(&out).unwrap();
        assert_eq!(items(&dest), before, "{path}: returned structure");
        assert_eq!(items(&again), before, "{path}: reparsed output");

        #[cfg(feature = "exif")]
        {
            // Real EXIF must parse, or the item was not the TIFF data.
            if s.items_of(ItemKind::Exif).next().is_some() {
                assert!(s.exif_info().is_ok(), "{path}");
            }
        }
    }
    assert!(ran > 0, "no HEIF fixtures found");
}
