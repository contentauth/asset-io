//! The built-in containers are modules, so these checks keep the boundaries
//! separate crates would: the rest of the crate depends on no container, no
//! container depends on another, and containers use only the public API.

use std::{
    fs,
    path::{Path, PathBuf},
};

const CONTAINERS: &[&str] = &["bmff", "c2pa", "jpeg", "pdf", "png", "riff", "svg", "tiff"];

/// Public modules a container may reach through `crate::`; everything else it
/// uses comes from the crate root's re-exports.
const PUBLIC_MODULES: &[&str] = &[
    "containers",
    "jumbf",
    "labels",
    "plan",
    "testing",
    "transfer",
];

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
            for line in code(&path) {
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
