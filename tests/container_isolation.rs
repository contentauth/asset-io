//! The built-in containers are modules, so these checks keep the boundaries
//! separate crates would: the rest of the crate depends on no container, no
//! container depends on another, and containers use only the public API.

use std::{
    fs,
    path::{Path, PathBuf},
};

const CONTAINERS: &[&str] = &["bmff", "c2pa", "jpeg", "pdf", "png", "riff", "svg", "tiff"];

/// Public modules a container may reach through `crate::`; everything else it
/// uses comes from the crate root's client re-exports. Advanced client modules
/// are available to tests; production handlers use `container_api`.
const PUBLIC_MODULES: &[&str] = &[
    "container_api",
    "containers",
    "execute",
    "io",
    "jumbf",
    "labels",
    "plan",
    "task",
    "testing",
    "transfer",
];

#[test]
fn root_exports_only_client_types() {
    let removed = [
        "ContainerHandler",
        "FormatSpec",
        "StructureBuilder",
        "ItemInfo",
        "PlanBuilder",
        "MetadataUpdate",
        "MetadataSlot",
        "Existing",
        "BoxFuture",
        "ReadSeek",
        "ReadWriteSeek",
        "DETECT_LEN",
        "Crc32",
        "MAX_SEGMENTS",
        "MAX_SEGMENT_SIZE",
        "Io",
        "Reader",
        "DEFAULT_WINDOW",
        "Task",
        "Progress",
        "ReadRequest",
        "block_on",
        "Action",
        "Execution",
        "Step",
        "WritePlan",
    ];
    let root = fs::read_to_string(src().join("lib.rs")).unwrap();
    for statement in root.split(';') {
        let Some((_, exports)) = statement.split_once("pub use ") else {
            continue;
        };
        assert!(!exports.contains('*'), "wildcard root export: {exports}");
        for name in exports.split(|c: char| !c.is_ascii_alphanumeric() && c != '_') {
            assert!(
                !removed.contains(&name),
                "author/advanced root export: {name}"
            );
        }
    }
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn src() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Code lines of a file, without comments (which may name anything).
fn code(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_rest_of_the_crate_depends_on_no_container() {
    let mut files = Vec::new();
    rust_files(&src(), &mut files);
    for path in files {
        let rel = path.strip_prefix(src()).unwrap();
        if rel.starts_with("containers") {
            continue;
        }
        for line in code(&path) {
            if !line.contains("containers") {
                continue;
            }
            let allowed = (rel == Path::new("lib.rs") && line.trim() == "pub mod containers;")
                || (rel == Path::new("registry.rs")
                    && line.contains("crate::containers::register_builtin"));
            assert!(allowed, "{}: {line}", rel.display());
        }
    }
}

#[test]
fn containers_use_the_public_api_and_not_each_other() {
    for name in CONTAINERS {
        let mut files = Vec::new();
        rust_files(&src().join("containers").join(name), &mut files);
        assert!(!files.is_empty(), "{name}: no sources");
        for path in files {
            let rel = path.strip_prefix(src()).unwrap().display().to_string();
            let mut in_tests = path.file_name().is_some_and(|n| n == "tests.rs");
            for line in fs::read_to_string(&path).unwrap().lines() {
                if line.trim() == "#[cfg(test)]" {
                    in_tests = true;
                }
                if line.trim_start().starts_with("//") {
                    continue;
                }
                for (at, _) in line.match_indices("crate::") {
                    let rest = &line[at + "crate::".len()..];
                    let module: String = rest
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                        .collect();
                    // `crate::Item`, `crate::{..}`, `crate::C2paHash::Bmff`: the crate
                    // root's re-exports. Only a lower-case name is a module.
                    if !rest[module.len()..].starts_with("::")
                        || !module.starts_with(|c: char| c.is_ascii_lowercase())
                    {
                        continue;
                    }
                    assert!(
                        PUBLIC_MODULES.contains(&module.as_str()),
                        "{rel}: reaches into crate::{module}: {line}"
                    );
                    assert!(
                        in_tests || !["io", "task", "execute", "plan"].contains(&module.as_str()),
                        "{rel}: production handler bypasses container_api: {line}"
                    );
                    if module == "containers" {
                        let own = format!("containers::{name}::");
                        assert!(
                            rest.starts_with(&own),
                            "{rel}: uses another container: {line}"
                        );
                    }
                }
            }
        }
    }
}
