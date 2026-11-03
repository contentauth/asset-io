#![cfg(feature = "c2pa")]

use std::io::{self, Cursor, Read, Seek, SeekFrom};

use asset_io::{
    containers::c2pa::handler, plan::validate, testing::Conformance, Asset, Encoding, Error,
    HandlerRegistry, ItemKind, NullSink, Progress, SegmentKind, Structure, TransferEncoding,
    Updates, DEFAULT_WINDOW,
};

/// Parse with the sidecar handler directly, from any reader.
fn parse<R: Read + Seek>(source: &mut R) -> asset_io::Result<Structure> {
    handler().parse(source)
}

/// `bytes` as a sidecar. Detection is conservative, so the format is named.
fn open(bytes: &[u8]) -> Asset<Cursor<&[u8]>> {
    let registry = HandlerRegistry::new().with_handler(asset_io::containers::c2pa::C2paHandler);
    Asset::open_with(&registry, Cursor::new(bytes), Some("c2pa")).unwrap()
}

const STORE_TYPE: [u8; 16] = [
    0x63, 0x32, 0x70, 0x61, 0x00, 0x11, 0x00, 0x10, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71,
];

fn store(ty: [u8; 16]) -> Vec<u8> {
    let mut bytes = 38u32.to_be_bytes().to_vec();
    bytes.extend_from_slice(b"jumb");
    bytes.extend_from_slice(&30u32.to_be_bytes());
    bytes.extend_from_slice(b"jumd");
    bytes.extend_from_slice(&ty);
    bytes.push(3);
    bytes.extend_from_slice(b"c2pa\0");
    bytes
}

fn write(source: &[u8], updates: &Updates) -> (Vec<u8>, Structure) {
    let asset = open(source);
    let plan = asset.write_plan(updates).unwrap();
    validate(&plan, source.len() as u64).unwrap();
    let mut bytes = Vec::new();
    let written = asset.write(&mut bytes, updates, &mut NullSink).unwrap();
    let reparsed = open(&bytes);
    assert_eq!(plan.structure().segments(), written.segments());
    assert_eq!(written.segments(), reparsed.segments());
    assert_eq!(written.total_size(), bytes.len() as u64);
    assert_eq!(
        written.items().map(|item| item.info()).collect::<Vec<_>>(),
        reparsed.items().map(|item| item.info()).collect::<Vec<_>>()
    );
    (bytes, written)
}

#[test]
fn aliases_and_conservative_detection() {
    let mut registry = HandlerRegistry::new();
    registry.add_arc(handler());
    for format in [
        "c2pa",
        " .C2PA ",
        "APPLICATION/C2PA",
        "application/x-c2pa-manifest-store",
    ] {
        assert_eq!(registry.container_for(format), Some("c2pa"));
        assert_eq!(registry.media_type_for(format), Some("application/c2pa"));
        assert_eq!(registry.extension_for(format), Some("c2pa"));
    }
    let bytes = store(STORE_TYPE);
    assert_eq!(registry.detect(&bytes).unwrap().id(), "c2pa");
    assert_eq!(
        Asset::open_with(&registry, Cursor::new(&bytes), None)
            .unwrap()
            .container_id(),
        "c2pa"
    );
    for n in 0..32 {
        assert!(registry.detect(&bytes[..n]).is_none(), "{n} bytes");
    }
    assert!(registry.detect(&store([1; 16])).is_none());
    assert!(registry.detect(b"opaque manifest bytes").is_none());
    let mut wrong_box = bytes;
    wrong_box[4..8].copy_from_slice(b"free");
    assert!(registry.detect(&wrong_box).is_none());
}

#[test]
fn the_entire_file_is_the_stored_jumbf_item() {
    for bytes in [store(STORE_TYPE), b"opaque bytes".to_vec(), vec![0]] {
        let asset = open(&bytes);
        assert_eq!(asset.media_type(), "application/c2pa");
        assert_eq!(asset.total_size(), bytes.len() as u64);
        assert_eq!(asset.segments().len(), 1);
        let segment = &asset.segments()[0];
        assert_eq!(segment.kind(), SegmentKind::Metadata);
        assert_eq!(segment.header().len(), 0);
        assert_eq!(segment.trailer().len(), 0);
        assert_eq!(segment.payload().len(), bytes.len() as u64);
        let item = asset.require_item(ItemKind::Jumbf).unwrap();
        assert_eq!(item.encoding(), Encoding::Jumbf);
        assert_eq!(*item.transfer(), TransferEncoding::Identity);
        assert_eq!(item.extent_count(), 1);
        assert_eq!(item.extents().next().unwrap().range(), segment.range());
        assert_eq!(asset.store().unwrap(), Some(bytes.clone()));
        assert_eq!(asset.read_item(item).unwrap(), bytes);
        assert_eq!(asset.xmp().unwrap(), None);
    }
    let bytes = store(STORE_TYPE);
    assert_eq!(
        open(&bytes)
            .require_item(ItemKind::Jumbf)
            .unwrap()
            .jumbf_type(),
        Some(STORE_TYPE)
    );
}

#[test]
fn empty_sidecars_have_no_items_and_can_receive_a_store() {
    let empty = open(&[]);
    assert_eq!(empty.total_size(), 0);
    assert!(empty.segments().is_empty());
    assert!(empty.items().next().is_none());
    assert_eq!(empty.store().unwrap(), None);
    let bytes = store(STORE_TYPE);
    let (output, _) = write(&[], &Updates::new().set_jumbf(bytes.clone()));
    assert_eq!(output, bytes);
}

#[test]
fn keep_replace_empty_and_remove_have_no_added_framing() {
    for source in [Vec::new(), store(STORE_TYPE), b"opaque".to_vec()] {
        assert_eq!(write(&source, &Updates::new()).0, source);
        assert_eq!(write(&source, &Updates::new().remove_xmp()).0, source);
        for replacement in [Vec::new(), store([1; 16]), b"new store".to_vec()] {
            let (output, _) = write(&source, &Updates::new().set_jumbf(replacement.clone()));
            assert_eq!(output, replacement);
            assert_eq!(
                write(
                    &source,
                    &Updates::new().set_jumbf(replacement.clone()).as_update()
                )
                .0,
                replacement
            );
        }
        for updates in [Updates::new().remove_jumbf(), Updates::remove_all()] {
            let (output, structure) = write(&source, &updates);
            assert!(output.is_empty());
            assert!(structure.items().next().is_none());
            assert!(structure.segments().is_empty());
        }
    }
}

#[test]
fn typed_removal_preserves_other_and_unknown_types() {
    let bytes = store(STORE_TYPE);
    assert!(write(&bytes, &Updates::new().remove_jumbf_of(STORE_TYPE))
        .0
        .is_empty());
    assert_eq!(
        write(&bytes, &Updates::new().remove_jumbf_of([1; 16])).0,
        bytes
    );
    let opaque = b"untyped store";
    assert_eq!(
        write(opaque, &Updates::new().remove_jumbf_of(STORE_TYPE)).0,
        opaque
    );
}

#[test]
fn composing_is_identity_for_jumbf_only() {
    assert_eq!(
        handler().compose(ItemKind::Jumbf, b"store").unwrap(),
        b"store"
    );
    assert!(handler().compose(ItemKind::Jumbf, b"").unwrap().is_empty());
    assert!(matches!(
        handler().compose(ItemKind::Xmp, b"<x/>"),
        Err(Error::UnsupportedFormat)
    ));
}

#[test]
fn setting_xmp_is_rejected_before_output_is_changed() {
    let source = store(STORE_TYPE);
    let mut output = b"untouched".to_vec();
    let updates = Updates::new().set_xmp(b"<x/>".to_vec()).remove_jumbf();
    assert!(matches!(
        open(&source).write(&mut output, &updates, &mut NullSink),
        Err(Error::UnsupportedFeature(_))
    ));
    assert_eq!(output, b"untouched");
}

#[test]
fn patches_require_the_exact_length_and_never_pad() {
    let source = b"original store";
    // A writable copy, opened directly, so it can be patched in place.
    let registry = HandlerRegistry::new().with_handler(asset_io::containers::c2pa::C2paHandler);
    let output = Asset::open_with(&registry, Cursor::new(source.to_vec()), Some("c2pa")).unwrap();
    let item = output.require_item(ItemKind::Jumbf).unwrap();
    for replacement in [b"short".as_slice(), b"replacement too long".as_slice()] {
        assert!(output.patch(item, replacement).is_err());
        assert_eq!(output.read_item(item).unwrap(), source);
    }
    let replacement = b"replaced store";
    output.patch_exact(item, replacement).unwrap();
    assert_eq!(output.into_source().into_inner(), replacement);
}

struct Counted {
    source: Cursor<Vec<u8>>,
    read: usize,
}

impl Read for Counted {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let n = self.source.read(output)?;
        self.read += n;
        Ok(n)
    }
}

impl Seek for Counted {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.source.seek(position)
    }
}

#[test]
fn parsing_reads_only_a_bounded_prefix_and_planning_reads_nothing() {
    let mut bytes = store(STORE_TYPE);
    bytes.resize(2 * 1024 * 1024, 0);
    let mut source = Counted {
        source: Cursor::new(bytes),
        read: 0,
    };
    let structure = parse(&mut source).unwrap();
    assert_eq!(structure.total_size(), 2 * 1024 * 1024);
    assert!(
        (32..=DEFAULT_WINDOW).contains(&source.read),
        "{} bytes read",
        source.read
    );
    let updates = Updates::new().remove_jumbf();
    let mut task = structure.write_plan_task(&updates);
    assert!(matches!(task.resume().unwrap(), Progress::Done(_)));
}

#[test]
fn prefix_read_errors_are_propagated() {
    struct Broken;
    impl Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("read failed"))
        }
    }
    impl Seek for Broken {
        fn seek(&mut self, _: SeekFrom) -> io::Result<u64> {
            Ok(64)
        }
    }
    assert!(matches!(parse(&mut Broken), Err(Error::Io(_))));
}

#[test]
fn empty_opaque_and_structured_sidecars_conform() {
    for sample in [Vec::new(), b"opaque".to_vec(), store(STORE_TYPE)] {
        Conformance::new(handler())
            .xmp(None)
            .jumbf(Some(store(STORE_TYPE)))
            .check("sidecar", &sample);
    }
}
