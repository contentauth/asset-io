// Copyright 2026 Adobe. All rights reserved.
// This file is licensed under the Apache License, Version 2.0 or the MIT license,
// at your option.

//! Reading an item through `ItemCursor` drivers: a `Read` + `Seek` reader, a
//! callback over a caller's buffer, and the same through an `Io`.

#![cfg(feature = "jpeg")]

use std::io::{Cursor, Read, Seek, SeekFrom};

use asset_io::{
    container_api::{ItemInfo, StructureBuilder},
    task::block_on,
    ByteRange, Encoding, Error, HandlerRegistry, ItemKind, Name, SegmentKind, Structure,
};

/// A stream whose bytes tell their position apart, a structure with one item over
/// three of its extents (stored out of order, with other bytes between them), and
/// the item's bytes.
fn fixture() -> (Vec<u8>, Structure, Vec<u8>) {
    let data: Vec<u8> = (0..6000u32).map(|i| (i * 7 % 251) as u8).collect();
    let extents = [(4000u64, 500u64), (100, 3000), (3500, 1)];
    let registry = HandlerRegistry::builtin();
    let mut builder = StructureBuilder::new(
        registry.handler("jpg").unwrap().clone(),
        "image/jpeg",
        data.len() as u64,
    );
    let id = builder
        .push_span(SegmentKind::Other, Name::None, 0, data.len() as u64, 0)
        .unwrap();
    let ranges: Vec<_> = extents
        .iter()
        .map(|&(at, len)| (id, ByteRange::new(at, len).unwrap()))
        .collect();
    builder
        .item(
            ItemInfo::new(ItemKind::EmbeddedAsset, Encoding::Asset),
            &ranges,
        )
        .unwrap();
    let expected = extents
        .iter()
        .flat_map(|&(at, len)| data[at as usize..(at + len) as usize].iter().copied())
        .collect();
    (data, builder.finish().unwrap(), expected)
}

#[test]
fn item_reader_reads_and_seeks_like_read_item() {
    let (data, structure, expected) = fixture();
    let item = structure.embedded_assets().next().unwrap();
    assert_eq!(
        structure.read_item(&mut Cursor::new(&data), item).unwrap(),
        expected
    );

    let mut src = Cursor::new(&data);
    let mut reader = structure.item_reader(&mut src, item).unwrap();
    assert_eq!(reader.len(), expected.len() as u64);
    let mut all = Vec::new();
    reader.read_to_end(&mut all).unwrap();
    assert_eq!(all, expected);

    // From inside the second extent, across into the third.
    reader.seek(SeekFrom::Start(700)).unwrap();
    let mut middle = vec![0u8; 1000];
    reader.read_exact(&mut middle).unwrap();
    assert_eq!(middle, &expected[700..1700]);
    reader.seek(SeekFrom::End(-1)).unwrap();
    let mut last = [0u8; 1];
    reader.read_exact(&mut last).unwrap();
    assert_eq!(last[0], expected[expected.len() - 1]);
}

#[test]
fn item_cursor_checks_extents_against_the_asset() {
    let (data, structure, _) = fixture();
    let item = structure.embedded_assets().next().unwrap();
    let cursor = structure.item_cursor(item).unwrap();
    assert_eq!(cursor.len(), item.len());
    // A source shorter than the structure is an error when read, not a short item.
    let mut short = Cursor::new(&data[..4200]);
    let mut reader = structure.item_reader(&mut short, item).unwrap();
    assert!(reader.read_to_end(&mut Vec::new()).is_err());
}

#[test]
fn stream_item_visits_the_bytes_in_chunks() {
    let (data, structure, expected) = fixture();
    let item = structure.embedded_assets().next().unwrap();
    for size in [1usize, 7, 64, 500, 501, 4096] {
        let mut buf = vec![0u8; size];
        let mut out = Vec::new();
        let mut chunks = 0;
        structure
            .stream_item(&mut Cursor::new(&data), item, &mut buf, |chunk| {
                assert!(!chunk.is_empty() && chunk.len() <= size);
                out.extend_from_slice(chunk);
                chunks += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(out, expected, "buffer {size}");
        assert!(chunks >= expected.len().div_ceil(size), "buffer {size}");
    }
    assert!(structure
        .stream_item(&mut Cursor::new(&data), item, &mut [], |_| Ok(()))
        .is_err());
}

#[test]
fn stream_item_stops_when_the_callback_fails() {
    let (data, structure, _) = fixture();
    let item = structure.embedded_assets().next().unwrap();
    let mut seen = 0;
    let err = structure
        .stream_item(&mut Cursor::new(&data), item, &mut [0u8; 64], |_| {
            seen += 1;
            Err(Error::UserCanceled)
        })
        .unwrap_err();
    assert!(matches!(err, Error::UserCanceled));
    assert_eq!(seen, 1);
}

#[test]
fn stream_item_async_gives_the_same_bytes_under_block_on() {
    let (data, structure, expected) = fixture();
    let item = structure.embedded_assets().next().unwrap();
    for window in [1usize, 5, 100, 4096] {
        let mut out = Vec::new();
        let sink = &mut out;
        let structure = &structure;
        block_on(
            data.len() as u64,
            &mut Cursor::new(&data),
            |mut io| async move {
                structure
                    .stream_item_async(&mut io, item, window, |chunk| {
                        assert!(chunk.len() <= window);
                        sink.extend_from_slice(chunk);
                        Ok(())
                    })
                    .await
            },
        )
        .unwrap();
        assert_eq!(out, expected, "window {window}");
    }
}

#[test]
fn stream_item_async_rejects_an_empty_window_and_a_short_stream() {
    let (data, structure, _) = fixture();
    let item = structure.embedded_assets().next().unwrap();
    // A reference, so that each future takes a copy of it, not the structure.
    let structure = &structure;
    let empty = block_on(
        data.len() as u64,
        &mut Cursor::new(&data),
        |mut io| async move {
            structure
                .stream_item_async(&mut io, item, 0, |_| Ok(()))
                .await
        },
    );
    assert!(empty.is_err());
    let short = block_on(
        data.len() as u64,
        &mut Cursor::new(&data[..4200]),
        |mut io| async move {
            structure
                .stream_item_async(&mut io, item, 64, |_| Ok(()))
                .await
        },
    );
    assert!(short.is_err());
}
