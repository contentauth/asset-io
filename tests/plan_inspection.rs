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

//! Inspecting a write plan before running it: reading the planned output,
//! finding the items it writes and the bytes it relocates.

#![cfg(feature = "all-formats")]

use std::io::{Cursor, Read, Seek, SeekFrom};

use asset_io::{
    plan::Step, testing::c2pa_store_of_len, ByteRange, Chunk, Error, HandlerRegistry, ItemKind,
    Structure, Updates,
};

const FIXTURES: &[(&str, &str)] = &[
    ("jpg", "tests/fixtures/FireflyTrain.jpg"),
    ("png", "tests/fixtures/GreenCat.png"),
    ("gif", "tests/fixtures/sample1.gif"),
    ("webp", "tests/fixtures/sample1.webp"),
    ("wav", "tests/fixtures/sample1.wav"),
    ("mp3", "tests/fixtures/sample1.mp3"),
    ("svg", "tests/fixtures/sample1.svg"),
    ("tif", "tests/fixtures/tiff_multipage.tif"),
    ("heic", "tests/fixtures/sample1.heic"),
    ("avif", "tests/fixtures/sample1.avif"),
    ("m4a", "tests/fixtures/sample1.m4a"),
];

fn parse(format: &str, bytes: &[u8]) -> Structure {
    HandlerRegistry::builtin()
        .handler(format)
        .unwrap()
        .clone()
        .parse(&mut Cursor::new(bytes))
        .unwrap()
}

#[test]
fn reader_matches_execute_and_written_items_find_the_update() {
    for (format, path) in FIXTURES {
        let bytes = std::fs::read(path).unwrap();
        let s = parse(format, &bytes);
        let jumbf = c2pa_store_of_len(256);
        let updates = Updates::new().set_c2pa(jumbf.clone());
        let plan = s.write_plan(&mut Cursor::new(&bytes), &updates).unwrap();

        let mut executed = Vec::new();
        plan.execute(
            &mut Cursor::new(&bytes),
            &mut executed,
            &mut asset_io::NullSink,
        )
        .unwrap();
        let mut read = Vec::new();
        plan.reader(Cursor::new(&bytes))
            .read_to_end(&mut read)
            .unwrap();
        assert_eq!(read, executed, "{format}: reader differs from execute");

        // Small reads at odd offsets agree too.
        let mut reader = plan.reader(Cursor::new(&bytes));
        let at = executed.len() as u64 / 3;
        reader.seek(SeekFrom::Start(at)).unwrap();
        let mut buf = [0u8; 7];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(
            &buf[..n],
            &executed[at as usize..at as usize + n],
            "{format}"
        );
        assert_eq!(
            reader.seek(SeekFrom::End(0)).unwrap(),
            executed.len() as u64
        );
        assert_eq!(reader.read(&mut buf).unwrap(), 0, "{format}: read past end");

        let written: Vec<_> = plan.written_items(ItemKind::C2pa).collect();
        assert_eq!(written.len(), 1, "{format}: one written C2PA item");
        let found = plan.structure().c2pa_item().unwrap();
        assert!(
            found.is_some_and(|f| std::ptr::eq(f, written[0])),
            "{format}: written item is the planned store"
        );
        assert_eq!(plan.written_items(ItemKind::Xmp).count(), 0, "{format}");

        // Keeping everything writes no items.
        let keep = Updates::new();
        let plan = s.write_plan(&mut Cursor::new(&bytes), &keep).unwrap();
        assert_eq!(plan.written_items(ItemKind::C2pa).count(), 0, "{format}");
    }
}

#[test]
fn relocated_offset_tables_are_reported_and_read_rewritten() {
    let bytes = std::fs::read("tests/fixtures/sample1.m4a").unwrap();
    let s = parse("m4a", &bytes);
    let updates = Updates::new().set_c2pa(c2pa_store_of_len(4096));
    let plan = s.write_plan(&mut Cursor::new(&bytes), &updates).unwrap();

    let mut at = 0;
    let mut relocated = None;
    for step in plan.steps() {
        if matches!(step, Step::Relocate { .. }) {
            relocated = Some(ByteRange::new(at, step.len()).unwrap());
        }
        at += step.len();
    }
    let relocated = relocated.expect("inserting before mdat relocates chunk offsets");
    assert!(plan.is_relocated(relocated));
    assert!(plan.is_relocated(ByteRange::new(relocated.offset(), 1).unwrap()));
    assert!(!plan.is_relocated(ByteRange::new(0, 8).unwrap()));

    let mut executed = Vec::new();
    plan.execute(
        &mut Cursor::new(&bytes),
        &mut executed,
        &mut asset_io::NullSink,
    )
    .unwrap();
    // Read the table one byte at a time, so every read starts mid-entry.
    let mut reader = plan.reader(Cursor::new(&bytes));
    reader.seek(SeekFrom::Start(relocated.offset())).unwrap();
    let mut table = vec![0; relocated.len() as usize];
    for byte in &mut table {
        reader.read_exact(std::slice::from_mut(byte)).unwrap();
    }
    let range = relocated.offset() as usize..relocated.end() as usize;
    assert_eq!(table, executed[range.clone()]);
    assert_ne!(table, bytes[range], "the table moved");
}

#[derive(Debug)]
struct Stop(u32);

impl std::fmt::Display for Stop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "stopped at chunk {}", self.0)
    }
}

impl std::error::Error for Stop {}

#[test]
fn sink_errors_are_returned_unchanged() {
    let bytes = std::fs::read("tests/fixtures/GreenCat.png").unwrap();
    let s = parse("png", &bytes);
    let updates = Updates::new();
    let plan = s.write_plan(&mut Cursor::new(&bytes), &updates).unwrap();
    let mut seen = 0;
    let error = plan
        .execute(
            &mut Cursor::new(&bytes),
            &mut Vec::new(),
            &mut |_: Chunk<'_>| {
                seen += 1;
                if seen == 2 {
                    return Err(Error::callback(Stop(seen)));
                }
                Ok(())
            },
        )
        .unwrap_err();
    let Error::Callback(source) = error else {
        panic!("expected a callback error, got {error:?}");
    };
    assert_eq!(source.downcast::<Stop>().unwrap().0, 2);
}

#[test]
fn an_update_the_container_cannot_place_is_unsupported_item() {
    // A C2PA sidecar is a manifest store; it has nowhere to put XMP.
    let bytes = c2pa_store_of_len(64);
    let s = parse("c2pa", &bytes);
    let mut src = Cursor::new(&bytes);
    assert!(matches!(
        s.write_plan(&mut src, &Updates::new().set_xmp(b"<x/>".to_vec())),
        Err(Error::UnsupportedItem(ItemKind::Xmp))
    ));
    assert!(matches!(
        s.write(
            &mut src,
            &mut Vec::new(),
            &Updates::new().set_xmp(b"<x/>".to_vec()),
            &mut asset_io::NullSink
        ),
        Err(Error::UnsupportedItem(ItemKind::Xmp))
    ));
    assert!(s
        .write_plan(&mut src, &Updates::new().set_c2pa(c2pa_store_of_len(96)))
        .is_ok());
}
