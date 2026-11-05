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

fn package_version(manifest: &str) -> Option<&str> {
    let mut in_package = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
        } else if in_package {
            if let Some(value) = line.strip_prefix("version") {
                let value = value.trim_start().strip_prefix('=')?.trim();
                return value.strip_prefix('"')?.strip_suffix('"');
            }
        }
    }
    None
}

fn main() {
    let root = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let sdk_manifest =
        std::fs::read_to_string(root.join("../Cargo.toml")).expect("read asset-io Cargo.toml");
    let sdk_version =
        package_version(&sdk_manifest).expect("asset-io Cargo.toml must declare a package version");
    let bindings_version = std::env::var("CARGO_PKG_VERSION").expect("Cargo sets package version");
    assert_eq!(
        bindings_version, sdk_version,
        "asset-io-c and asset-io package versions must match"
    );

    let bindings = cbindgen::Builder::new()
        .with_crate(&root)
        .with_config(cbindgen::Config::from_file(root.join("cbindgen.toml")).unwrap())
        .generate()
        .expect("generate asset_io.h");
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("asset_io.h");
    bindings.write_to_file(&out);
    if std::env::var_os("ASSET_IO_UPDATE_HEADER").is_some() {
        bindings.write_to_file(root.join("include/asset_io.h"));
    } else {
        let checked = std::fs::read(root.join("include/asset_io.h"))
            .expect("missing header: build with ASSET_IO_UPDATE_HEADER=1");
        assert_eq!(
            checked,
            std::fs::read(out).unwrap(),
            "stale header: build with ASSET_IO_UPDATE_HEADER=1"
        );
    }
    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-changed=../Cargo.toml");
    println!("cargo:rerun-if-changed=cbindgen.toml");
    println!("cargo:rerun-if-changed=include/asset_io.h");
    println!("cargo:rerun-if-env-changed=ASSET_IO_UPDATE_HEADER");
}
