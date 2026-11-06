#![cfg(feature = "cli")]

use std::{fs, process::Command};

use serde_json::Value;

mod pdf_support;

const XMP: &str = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?><x:xmpmeta xmlns:x="adobe:ns:meta/"/><?xpacket end="w"?>"#;
const SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg"><metadata></metadata></svg>"#;

fn aio(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_aio"))
        .args(args)
        .output()
        .unwrap()
}

fn json(output: &std::process::Output) -> Value {
    assert!(
        output.status.success(),
        "aio failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn inspect_glob_extract_update_and_embedded_are_machine_readable() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.svg");
    let xmp_file = dir.path().join("replacement.xmp");
    let output = dir.path().join("updated.svg");
    let extract_dir = dir.path().join("items");
    let bulk_output_dir = dir.path().join("bulk-updated");
    fs::write(&input, SVG).unwrap();
    fs::write(dir.path().join("second.svg"), SVG).unwrap();
    fs::write(&xmp_file, XMP).unwrap();

    let pattern = dir.path().join("*.svg").display().to_string();
    let inspected = json(&aio(&["inspect", &pattern, "--json"]));
    assert_eq!(inspected.as_array().unwrap().len(), 2);
    assert_eq!(inspected[0]["status"], "ok");
    assert_eq!(inspected[0]["container"]["id"], "svg");
    assert_eq!(inspected[0]["segments"][0]["kind"], "preamble");

    let bulk_updated = aio(&[
        "update",
        &pattern,
        "--xmp-file",
        xmp_file.to_str().unwrap(),
        "--output-dir",
        bulk_output_dir.to_str().unwrap(),
    ]);
    let bulk_updated = json(&bulk_updated);
    assert_eq!(bulk_updated.as_array().unwrap().len(), 2);
    for result in bulk_updated.as_array().unwrap() {
        assert!(std::path::Path::new(result["output"].as_str().unwrap()).exists());
    }

    let updated = aio(&[
        "update",
        input.to_str().unwrap(),
        "--xmp-file",
        xmp_file.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ]);
    assert_eq!(json(&updated)[0]["status"], "ok");

    let extracted = aio(&[
        "extract",
        output.to_str().unwrap(),
        "--output-dir",
        extract_dir.to_str().unwrap(),
    ]);
    let extracted = json(&extracted);
    let xmp_output = extracted[0]["extracted"][0]["output"].as_str().unwrap();
    assert_eq!(fs::read(xmp_output).unwrap(), XMP.as_bytes());

    let embedded = aio(&["embedded", output.to_str().unwrap(), "--json"]);
    assert_eq!(json(&embedded)[0]["embedded_count"], 0);
}

#[test]
fn unmatched_glob_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let pattern = dir.path().join("*.missing").display().to_string();
    let result = aio(&["inspect", &pattern]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("matched no paths"));
}

#[test]
fn inspection_defaults_to_readable_views_and_supports_json() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.svg");
    fs::write(&input, SVG).unwrap();
    let path = input.to_str().unwrap();

    let inspected = aio(&["inspect", path]);
    assert!(inspected.status.success());
    let text = String::from_utf8(inspected.stdout).unwrap();
    assert!(text.contains("Structure (parent sizes include children)"));
    assert!(text.contains("Items (stored sizes"));

    let segments = aio(&["inspect", path, "--segments"]);
    assert!(segments.status.success());
    let text = String::from_utf8(segments.stdout).unwrap();
    assert!(text.contains("Segments (non-overlapping"));
    assert!(text.contains("Header"));
    assert!(text.contains("Payload"));
    assert!(text.contains("Trailer"));

    let items = aio(&["items", path]);
    assert!(items.status.success());
    let text = String::from_utf8(items.stdout).unwrap();
    assert!(text.contains("Encoding"));
    assert!(!text.contains("Structure ("));

    let inspected = json(&aio(&["inspect", path, "--json"]));
    let total: u64 = inspected[0]["segments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|segment| {
            let size = segment["range"]["length"].as_u64().unwrap();
            assert_eq!(
                size,
                segment["header_bytes"].as_u64().unwrap()
                    + segment["payload_bytes"].as_u64().unwrap()
                    + segment["trailer_bytes"].as_u64().unwrap()
            );
            size
        })
        .sum();
    assert_eq!(total, fs::metadata(&input).unwrap().len());
    assert!(!inspected[0]["nodes"].as_array().unwrap().is_empty());
}

fn svg_with_xmp(dir: &std::path::Path) -> std::path::PathBuf {
    let input = dir.join("input.svg");
    let xmp_file = dir.join("metadata.xmp");
    let output = dir.join("updated.svg");
    fs::write(&input, SVG).unwrap();
    fs::write(&xmp_file, XMP).unwrap();
    json(&aio(&[
        "update",
        input.to_str().unwrap(),
        "--xmp-file",
        xmp_file.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ]));
    output
}

#[test]
fn readable_item_rows_align_with_their_headers() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("items.svg");
    fs::write(&input, format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:c2pa="http://c2pa.org/manifest"><metadata>{XMP}<c2pa:manifest>dGVzdA==</c2pa:manifest></metadata></svg>"#
    )).unwrap();
    for command in ["items", "inspect"] {
        let output = aio(&[command, input.to_str().unwrap()]);
        assert!(output.status.success());
        let output = String::from_utf8(output.stdout).unwrap();
        let mut lines = output
            .lines()
            .skip_while(|line| !line.trim_start().starts_with("Index"));
        let header = lines.next().unwrap();
        let rows = [
            (
                lines.next().unwrap(),
                "0",
                "xmp",
                "xml",
                format!("{} B", XMP.len()),
            ),
            (
                lines.next().unwrap(),
                "1",
                "jumbf",
                "jumbf",
                "8 B".to_owned(),
            ),
        ];
        for (row, index, kind, encoding, size) in rows {
            assert_eq!(
                row.find(index).unwrap() + index.len(),
                header.find("Index").unwrap() + 5
            );
            assert_eq!(row.find(kind), header.find("Kind"));
            let encoding_column = header.find("Encoding").unwrap();
            assert!(row[encoding_column..].starts_with(encoding));
            assert_eq!(
                row.find("base64").or_else(|| row.find("identity")),
                header.find("Transfer")
            );
            assert_eq!(
                row.find(&size).unwrap() + size.len(),
                header.find("Size").unwrap() + 4
            );
            let extents_end = header.find("Extents").unwrap() + "Extents".len();
            assert_eq!(&row[extents_end - 1..extents_end], "1");
            let native_id_end = header.find("Native ID").unwrap() + "Native ID".len();
            assert_eq!(&row[native_id_end - 1..native_id_end], "-");
            assert_eq!(&row[header.find("Label").unwrap()..], "-");
        }
    }
}

#[test]
fn item_filters_preserve_indexes_and_extract_selected_content() {
    let dir = tempfile::tempdir().unwrap();
    let input = svg_with_xmp(dir.path());
    let path = input.to_str().unwrap();
    let listed = json(&aio(&["items", path, "--kind", "xmp", "--json"]));
    assert_eq!(listed[0]["items"].as_array().unwrap().len(), 1);
    assert!(listed[0].get("segments").is_none());
    let item = &listed[0]["items"][0];
    let index = item["index"].as_u64().unwrap().to_string();
    assert_eq!(item["size_bytes"], XMP.len());
    let output = dir.path().join("selected.xmp");
    let extracted = json(&aio(&[
        "extract",
        path,
        "--item",
        &index,
        "--kind",
        "xmp",
        "--output",
        output.to_str().unwrap(),
    ]));
    assert_eq!(
        extracted[0]["extracted"][0]["item"],
        index.parse::<u64>().unwrap()
    );
    assert_eq!(fs::read(&output).unwrap(), XMP.as_bytes());
    let stored = dir.path().join("stored.xmp");
    json(&aio(&[
        "extract",
        path,
        "--kind",
        "xmp",
        "--stored",
        "--output",
        stored.to_str().unwrap(),
    ]));
    assert_eq!(fs::read(stored).unwrap(), XMP.as_bytes());
    let filtered = json(&aio(&["items", path, "--kind", "exif", "--json"]));
    assert_eq!(filtered[0]["item_count"], 0);
    let filtered = json(&aio(&["items", path, "--label", "missing", "--json"]));
    assert_eq!(filtered[0]["item_count"], 0);
    for selection in [
        ["--item", "999"],
        ["--kind", "exif"],
        ["--label", "missing"],
    ] {
        let failed = aio(&[
            "extract",
            path,
            selection[0],
            selection[1],
            "--output",
            output.to_str().unwrap(),
            "--force",
        ]);
        assert!(!failed.status.success());
        assert_eq!(fs::read(&output).unwrap(), XMP.as_bytes());
    }
    assert!(!aio(&["items", path, "--kind", "invalid"]).status.success());
}

#[test]
fn writes_require_force_and_never_overwrite_an_input() {
    let dir = tempfile::tempdir().unwrap();
    let input = svg_with_xmp(dir.path());
    let path = input.to_str().unwrap();
    let output = dir.path().join("output.svg");
    fs::write(&output, b"preserve existing output").unwrap();
    let result = aio(&[
        "update",
        path,
        "--remove-xmp",
        "--output",
        output.to_str().unwrap(),
    ]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stdout).contains("--force"));
    assert_eq!(fs::read(&output).unwrap(), b"preserve existing output");
    json(&aio(&[
        "update",
        path,
        "--remove-xmp",
        "--output",
        output.to_str().unwrap(),
        "--force",
    ]));
    let items = json(&aio(&[
        "items",
        output.to_str().unwrap(),
        "--kind",
        "xmp",
        "--json",
    ]));
    assert_eq!(items[0]["item_count"], 0);

    let original = fs::read(&input).unwrap();
    for command in ["update", "extract"] {
        let option = if command == "update" {
            "--remove-xmp"
        } else {
            "--stored"
        };
        let result = aio(&[command, path, option, "--output", path, "--force"]);
        assert!(!result.status.success());
        assert_eq!(fs::read(&input).unwrap(), original);
    }
    let extracted = dir.path().join("output.xmp");
    fs::write(&extracted, b"keep").unwrap();
    assert!(!aio(&[
        "extract",
        path,
        "--kind",
        "xmp",
        "--output",
        extracted.to_str().unwrap()
    ])
    .status
    .success());
    assert_eq!(fs::read(&extracted).unwrap(), b"keep");
    json(&aio(&[
        "extract",
        path,
        "--kind",
        "xmp",
        "--output",
        extracted.to_str().unwrap(),
        "--force",
    ]));
    assert_eq!(fs::read(&extracted).unwrap(), XMP.as_bytes());
    assert!(fs::read_dir(dir.path()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".tmp")
    }));
}

#[test]
fn failed_writes_leave_no_partial_output() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.jpg");
    // Detectable JPEG header, but truncated before the first marker's length.
    fs::write(&input, [0xff, 0xd8, 0xff, 0xe1]).unwrap();
    let output = dir.path().join("output.jpg");
    let result = aio(&[
        "update",
        input.to_str().unwrap(),
        "--remove-xmp",
        "--output",
        output.to_str().unwrap(),
    ]);
    assert!(!result.status.success());
    assert!(!output.exists());
    fs::write(&output, b"keep").unwrap();
    let result = aio(&[
        "update",
        input.to_str().unwrap(),
        "--remove-xmp",
        "--output",
        output.to_str().unwrap(),
        "--force",
    ]);
    assert!(!result.status.success());
    assert_eq!(fs::read(&output).unwrap(), b"keep");
}

#[test]
fn nested_hierarchy_sizes_and_depth_limit_are_exact() {
    fn boxed(name: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut bytes = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(name);
        bytes.extend_from_slice(body);
        bytes
    }
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("nested.mp4");
    let mut bytes = boxed(b"ftyp", b"isom\0\0\0\0isom");
    bytes.extend(boxed(b"moov", &boxed(b"trak", &boxed(b"mdia", b""))));
    fs::write(&input, &bytes).unwrap();
    let path = input.to_str().unwrap();
    let inspected = json(&aio(&["inspect", path, "--json"]));
    let nodes = inspected[0]["nodes"].as_array().unwrap();
    let moov = nodes.iter().find(|node| node["path"] == "/moov").unwrap();
    assert_eq!(moov["range"]["length"], 24);
    let mdia = nodes
        .iter()
        .find(|node| node["path"] == "/moov/trak/mdia")
        .unwrap();
    assert_eq!(mdia["depth"], 3);
    assert_eq!(mdia["range"]["offset"], 36);
    assert_eq!(mdia["range"]["length"], 8);
    let limited = json(&aio(&["inspect", path, "--json", "--max-depth", "2"]));
    assert_eq!(limited[0]["nodes"].as_array().unwrap().len(), 3);
    let human = aio(&["inspect", path]);
    assert!(human.status.success());
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("Path"));
    let mdia_row = text
        .lines()
        .find(|line| line.contains("/moov/trak/mdia"))
        .unwrap();
    assert!(mdia_row.contains("    mdia"));
    let segments = json(&aio(&["inspect", path, "--json", "--segments"]));
    assert!(segments[0].get("nodes").is_none());
    assert_eq!(segments[0]["segments"].as_array().unwrap().len(), 2);
}

#[test]
fn jpeg_element_names_are_shown_in_tree_and_segment_views() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("markers.jpg");
    let mut bytes = vec![0xff, 0xd8];
    for (marker, payload) in [
        (0xe0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0".as_slice()),
        (0xdb, &[0; 65]),
        (0xc0, &[8, 0, 1, 0, 1, 1, 1, 0x11, 0]),
        (0xc4, &[0; 20]),
        (0xda, &[1, 1, 0, 0, 0x3f, 0]),
    ] {
        bytes.extend_from_slice(&[0xff, marker]);
        bytes.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        bytes.extend_from_slice(payload);
    }
    bytes.extend_from_slice(&[0x12, 0x34, 0xff, 0x00, 0x56, 0xff, 0xd9]);
    fs::write(&input, bytes).unwrap();
    let path = input.to_str().unwrap();
    for args in [vec!["inspect", path], vec!["inspect", path, "--segments"]] {
        let output = aio(&args);
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        for name in [
            "SOI (0xFFD8)",
            "APP0 (0xFFE0)",
            "DQT (0xFFDB)",
            "SOF0 (0xFFC0)",
            "DHT (0xFFC4)",
            "SOS (0xFFDA)",
            "EOI (0xFFD9)",
        ] {
            assert!(text.contains(name), "missing {name} in:\n{text}");
        }
    }
    let inspected = json(&aio(&["inspect", path, "--json"]));
    assert_eq!(inspected[0]["nodes"][0]["name"], "0xFFD8");
    assert_eq!(inspected[0]["nodes"][0]["box_name"], "SOI");
    assert_eq!(inspected[0]["segments"][0]["box_name"], "SOI");
    let output = aio(&["inspect", path]);
    let text = String::from_utf8(output.stdout).unwrap();
    let soi_row = text
        .lines()
        .find(|line| line.contains("SOI (0xFFD8)"))
        .unwrap();
    assert!(soi_row.contains(inspected[0]["nodes"][0]["path"].as_str().unwrap()));
}

fn typed_jumbf(ty: [u8; 16], payload_len: usize) -> Vec<u8> {
    let mut bytes = ((41 + payload_len) as u32).to_be_bytes().to_vec();
    bytes.extend_from_slice(b"jumb");
    bytes.extend_from_slice(&25u32.to_be_bytes());
    bytes.extend_from_slice(b"jumd");
    bytes.extend_from_slice(&ty);
    bytes.push(0);
    bytes.extend_from_slice(&((8 + payload_len) as u32).to_be_bytes());
    bytes.extend_from_slice(b"json");
    bytes.resize(41 + payload_len, b' ');
    bytes
}

#[test]
fn c2pa_content_is_identified_without_changing_the_jumbf_kind() {
    use std::io::Cursor;

    let dir = tempfile::tempdir().unwrap();
    let mut bmff = 20u32.to_be_bytes().to_vec();
    bmff.extend_from_slice(b"ftypisom\0\0\0\0isom");
    let mut pdf = pdf_support::metadata_pdf();
    pdf.object(1, 0, b"<< /Type /Catalog /Metadata 2 0 R /AF [4 0 R] /Names << /EmbeddedFiles << /Names [(credentials) 4 0 R] >> >> >>");
    pdf.finish("/Root 1 0 R");
    let store = typed_jumbf(asset_io::jumbf::C2PA_STORE_TYPE, 70_000);
    for (format, source) in [
        ("jpg", vec![0xff, 0xd8, 0xff, 0xd9]),
        (
            "png",
            b"\x89PNG\r\n\x1a\n\0\0\0\0IEND\xae\x42\x60\x82".to_vec(),
        ),
        ("svg", SVG.as_bytes().to_vec()),
        ("mp4", bmff),
        ("tiff", b"II\x2a\0\x08\0\0\0\0\0\0\0\0\0".to_vec()),
        ("pdf", pdf.bytes),
        ("c2pa", typed_jumbf([1; 16], 0)),
    ] {
        let input = dir.path().join(format!("signed.{format}"));
        let asset = asset_io::Asset::open(Cursor::new(source), Some(format)).unwrap();
        let mut output = Cursor::new(Vec::new());
        asset
            .write(
                &mut output,
                &asset_io::Updates::new().set_jumbf(store.clone()),
                &mut asset_io::NullSink,
            )
            .unwrap();
        fs::write(&input, output.into_inner()).unwrap();
        let path = input.to_str().unwrap();
        for command in ["items", "inspect"] {
            let result = json(&aio(&[command, path, "--json"]));
            let item = result[0]["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["content_type"] == "c2pa_manifest_store")
                .unwrap_or_else(|| panic!("no C2PA content identified in {format}: {result}"));
            assert_eq!(item["kind"], "jumbf");
            assert_eq!(item["encoding"], "jumbf");
            assert_eq!(item["jumbf_type"], "6332706100110010800000aa00389b71");
            if format == "jpg" {
                assert!(item["extent_count"].as_u64().unwrap() > 1);
            }
            let human = aio(&[command, path]);
            assert!(human.status.success());
            let text = String::from_utf8(human.stdout).unwrap();
            assert!(text.contains("Content"));
            assert!(text.contains("C2PA"), "{format}: {text}");
        }
    }
}

#[test]
fn unrelated_jumbf_is_not_mislabeled_as_c2pa() {
    use std::io::Cursor;

    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("unrelated.jpg");
    let asset =
        asset_io::Asset::open(Cursor::new(vec![0xff, 0xd8, 0xff, 0xd9]), Some("jpg")).unwrap();
    let mut output = Cursor::new(Vec::new());
    asset
        .write(
            &mut output,
            &asset_io::Updates::new().set_jumbf(typed_jumbf([1; 16], 0)),
            &mut asset_io::NullSink,
        )
        .unwrap();
    fs::write(&input, output.into_inner()).unwrap();
    let result = json(&aio(&["items", input.to_str().unwrap(), "--json"]));
    assert_eq!(result[0]["items"][0]["kind"], "jumbf");
    assert!(result[0]["items"][0]["content_type"].is_null());
    let human = aio(&["items", input.to_str().unwrap()]);
    assert!(human.status.success());
    assert!(!String::from_utf8_lossy(&human.stdout).contains("C2PA"));
}

#[test]
fn inspect_automatically_reports_the_jpeg_inside_an_opaque_heif_item() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/elephant-ref-img.HEIC"
    );
    for args in [
        vec!["inspect", path],
        vec!["inspect", path, "--segments"],
        vec!["embedded", path],
    ] {
        let output = aio(&args);
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("Embedded [0] from root item [7] (depth 1)"));
        assert!(text.contains("native ID: 133"));
        assert!(text.contains("image/jpeg (jpeg)"));
        assert!(text.contains("4 items"));
        assert!(text.contains("iso_bmff"));
    }

    let inspected = json(&aio(&["inspect", path, "--json"]));
    assert_eq!(inspected[0]["items"][7]["kind"], "other");
    assert_eq!(inspected[0]["items"][7]["encoding"], "unknown");
    assert_eq!(inspected[0]["embedded_count"], 1);
    let child = &inspected[0]["embedded"][0];
    assert_eq!(child["source_item_index"], 7);
    assert_eq!(child["source_item"]["native_id"], 133);
    assert!(child["parent"].is_null());
    assert_eq!(child["depth"], 1);
    assert_eq!(child["asset"]["status"], "parsed");
    assert_eq!(child["asset"]["media_type"], "image/jpeg");
    assert_eq!(child["asset"]["item_count"], 4);
    assert_eq!(child["asset"]["items"][0]["kind"], "exif");
    let boxes = &child["asset"]["items"].as_array().unwrap()[1..];
    assert!(boxes.iter().all(|item| item["encoding"] == "iso_bmff"
        && item["kind"] == "other"
        && item["content_type"].is_null()));
    let embedded = json(&aio(&["embedded", path, "--json"]));
    assert_eq!(inspected[0]["embedded"], embedded[0]["embedded"]);
}

#[test]
fn embedded_scan_limits_are_reported_as_errors() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/elephant-ref-img.HEIC"
    );
    for option in ["--max-depth", "--max-assets", "--max-total-bytes"] {
        let output = aio(&["embedded", path, option, "0", "--json"]);
        assert!(!output.status.success());
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result[0]["status"], "error");
        assert!(result[0]["error"].as_str().unwrap().contains("embedded"));
    }
}

#[test]
fn extraction_distinguishes_decoded_content_from_stored_bytes_and_rejects_ambiguity() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("manifest.svg");
    fs::write(&input, r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:c2pa="http://c2pa.org/manifest"><metadata><c2pa:manifest>dGVzdA==</c2pa:manifest><c2pa:manifest>bW9yZQ==</c2pa:manifest></metadata></svg>"#).unwrap();
    let path = input.to_str().unwrap();
    let output = dir.path().join("manifest.jumbf");
    assert!(!aio(&[
        "extract",
        path,
        "--kind",
        "jumbf",
        "--output",
        output.to_str().unwrap()
    ])
    .status
    .success());
    assert!(!output.exists());
    json(&aio(&[
        "extract",
        path,
        "--item",
        "0",
        "--output",
        output.to_str().unwrap(),
    ]));
    assert_eq!(fs::read(&output).unwrap(), b"test");
    json(&aio(&[
        "extract",
        path,
        "--item",
        "0",
        "--stored",
        "--output",
        output.to_str().unwrap(),
        "--force",
    ]));
    assert_eq!(fs::read(&output).unwrap(), b"dGVzdA==");
}

#[cfg(unix)]
#[test]
fn outputs_reject_symlinks_hardlinks_and_other_matched_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let input = svg_with_xmp(dir.path());
    let path = input.to_str().unwrap();
    let original = fs::read(&input).unwrap();
    let symlink = dir.path().join("symlink.svg");
    let hardlink = dir.path().join("hardlink.svg");
    std::os::unix::fs::symlink(&input, &symlink).unwrap();
    fs::hard_link(&input, &hardlink).unwrap();
    for output in [&symlink, &hardlink] {
        assert!(!aio(&[
            "update",
            path,
            "--remove-xmp",
            "--output",
            output.to_str().unwrap(),
            "--force"
        ])
        .status
        .success());
        assert_eq!(fs::read(&input).unwrap(), original);
    }
    let batch = dir.path().join("z-batch");
    fs::create_dir(&batch).unwrap();
    let other_input = batch.join("000-updated.svg");
    fs::write(&other_input, SVG).unwrap();
    assert!(!aio(&[
        "update",
        path,
        other_input.to_str().unwrap(),
        "--remove-xmp",
        "--output-dir",
        batch.to_str().unwrap(),
        "--force"
    ])
    .status
    .success());
    assert_eq!(fs::read(&other_input).unwrap(), SVG.as_bytes());
}
