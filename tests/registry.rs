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

use std::{io::Cursor, sync::Arc};

use asset_io::{
    container_api::{BoxFuture, ContainerHandler, FormatSpec},
    io::Io,
    Asset, Error, HandlerRegistry, Result, Structure,
};

#[derive(Debug)]
struct Custom;

impl ContainerHandler for Custom {
    fn id(&self) -> &'static str {
        "custom"
    }

    fn formats(&self) -> &[FormatSpec] {
        const FORMATS: &[FormatSpec] = &[FormatSpec::new("application/x-custom", "custom")];
        FORMATS
    }

    fn parse_async(self: Arc<Self>, _: Io) -> BoxFuture<'static, Result<Structure>> {
        Box::pin(async { Err(Error::UnsupportedFeature("test handler")) })
    }
}

#[test]
fn builtin_registry_contains_exactly_the_enabled_families() {
    let registry = HandlerRegistry::builtin();
    assert!(Arc::ptr_eq(&registry, &HandlerRegistry::builtin()));
    let families = [
        ("jpg", cfg!(feature = "jpeg"), "jpeg"),
        ("png", cfg!(feature = "png"), "png"),
        ("heic", cfg!(feature = "bmff"), "bmff"),
        ("webp", cfg!(feature = "riff"), "riff"),
        ("svg", cfg!(feature = "svg"), "svg"),
        ("pdf", cfg!(feature = "pdf"), "pdf"),
        ("tif", cfg!(feature = "tiff"), "tiff"),
        ("tiff", cfg!(feature = "tiff"), "tiff"),
        ("dng", cfg!(feature = "tiff"), "tiff"),
        ("c2pa", cfg!(feature = "c2pa"), "c2pa"),
        ("gif", cfg!(feature = "gif"), "gif"),
        ("mp3", cfg!(feature = "id3"), "id3"),
        ("flac", cfg!(feature = "id3"), "id3"),
    ];
    for (extension, enabled, id) in families {
        assert_eq!(
            registry.container_for(extension),
            enabled.then_some(id),
            "{extension}"
        );
    }
    if families.iter().all(|(_, enabled, _)| !enabled) {
        assert!(registry.formats().is_empty());
        assert!(registry.detect(b"\xff\xd8\xff\xe0").is_none());
        assert!(matches!(
            Asset::open_with(&registry, Cursor::new([]), Some("jpg")),
            Err(Error::UnsupportedFormat)
        ));
        assert!(matches!(
            Asset::open_with(&registry, Cursor::new(b"%PDF-1.7\n"), None),
            Err(Error::UnsupportedFormat)
        ));
    }
}

#[test]
fn new_and_default_are_empty_even_when_builtins_are_enabled() {
    assert!(HandlerRegistry::new().formats().is_empty());
    assert!(HandlerRegistry::default().formats().is_empty());
}

#[test]
fn custom_handlers_compose_with_the_builtins() {
    let builtin = HandlerRegistry::builtin();
    let registry = HandlerRegistry::with_builtin().with_handler(Custom);
    assert!(registry.handler("custom").is_some());
    assert!(builtin.handler("custom").is_none());
    for format in builtin.formats() {
        assert_eq!(
            registry.container_for(&format),
            builtin.container_for(&format)
        );
    }
    assert_eq!(registry.handlers().count(), builtin.handlers().count() + 1);
    assert!(matches!(
        Asset::open_with(&registry, Cursor::new([]), Some("custom")),
        Err(Error::UnsupportedFeature("test handler"))
    ));
    // The built-in registry itself is shared and unchanged.
    assert!(HandlerRegistry::builtin().handler("custom").is_none());
}

/// How a crate that provides several handlers hands them to an application.
fn third_party_handlers() -> Vec<Arc<dyn ContainerHandler>> {
    vec![Arc::new(Custom)]
}

#[test]
fn handler_lists_and_registries_extend_a_registry() {
    let mut registry = HandlerRegistry::new().with_handlers(third_party_handlers());
    assert_eq!(registry.container_for("custom"), Some("custom"));

    // Extending with another registry adds its handlers after the existing ones.
    registry.extend_from(&HandlerRegistry::builtin());
    assert_eq!(
        registry.handlers().count(),
        HandlerRegistry::builtin().handlers().count() + 1
    );
    assert_eq!(registry.container_for("custom"), Some("custom"));

    // `Extend` takes the same lists.
    let mut other = HandlerRegistry::new();
    other.extend(third_party_handlers());
    assert_eq!(
        other.formats(),
        HandlerRegistry::new().with_handler(Custom).formats()
    );
}

#[test]
fn a_registry_composed_from_another_is_independent_of_it() {
    let base = HandlerRegistry::new();
    let mut composed = HandlerRegistry::new();
    composed.extend_from(&base);
    composed.add_handler(Custom);
    assert!(composed.handler("custom").is_some());
    assert!(base.handler("custom").is_none());
}

#[cfg(feature = "c2pa")]
#[test]
fn sidecars_work_through_the_feature_selected_registry() {
    let registry = HandlerRegistry::builtin();
    let bytes = b"raw manifest-store bytes";
    let asset = Asset::open_with(
        &registry,
        Cursor::new(bytes),
        Some("application/x-c2pa-manifest-store"),
    )
    .unwrap();
    assert_eq!(asset.container_id(), asset_io::containers::c2pa::ID);
    assert_eq!(asset.c2pa_store().unwrap().unwrap(), bytes);
    assert_eq!(
        registry
            .handler("c2pa")
            .unwrap()
            .compose(asset_io::ItemKind::C2pa, b"replacement")
            .unwrap(),
        b"replacement"
    );
    let mut output = Vec::new();
    let result = asset
        .write(
            &mut output,
            &asset_io::Updates::new().remove_c2pa(),
            &mut asset_io::NullSink,
        )
        .unwrap();
    assert!(output.is_empty());
    assert!(result.items().next().is_none());
}
