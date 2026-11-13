//! Embeds the built Browser UI (`web/dist`) so one
//! binary can serve it. Every file is embedded, dot-prefixed ones included;
//! the static handler refuses to serve hidden assets.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"),
    );
    let dist = manifest.join("../../web/dist");
    println!("cargo:rerun-if-changed={}", dist.display());
    let mut files = Vec::new();
    collect(&dist, &dist, &mut files);
    files.sort();
    let mut table = String::from("&[\n");
    for (name, path) in &files {
        println!("cargo:rerun-if-changed={}", path.display());
        let absolute = path.to_string_lossy();
        let _ = writeln!(
            table,
            "    ({name:?}, include_bytes!({absolute:?}).as_slice()),"
        );
    }
    table.push(']');
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    std::fs::write(out.join("webassets.rs"), table).expect("write the embedded asset table");
}

/// `(relative "/"-separated name, absolute path)` for every file below `root`.
fn collect(root: &Path, directory: &Path, files: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            println!("cargo:rerun-if-changed={}", path.display());
            collect(root, &path, files);
        } else if let Ok(relative) = path.strip_prefix(root) {
            let name = relative
                .components()
                .map(|component| component.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            let absolute = std::fs::canonicalize(&path).unwrap_or(path);
            files.push((name, absolute));
        }
    }
}
