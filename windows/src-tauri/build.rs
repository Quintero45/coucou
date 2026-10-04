// Besides Tauri's own build step, this bakes the SHA-256 of every protected
// core file into the binary (selfmod/guard.rs checks them at startup).

#[path = "src/selfmod/sha256.rs"]
mod sha256;
#[path = "src/selfmod/core_list.rs"]
mod core_list;

use std::path::{Path, PathBuf};

fn collect(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(path)
            .map(|rd| rd.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        entries.sort();
        for entry in entries {
            collect(&entry, out);
        }
    } else if path.is_file() {
        out.push(path.to_path_buf());
    }
}

fn write_core_hashes() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest.parent().and_then(Path::parent).unwrap().to_path_buf();
    let mut lines = Vec::new();
    for rel in core_list::PROTECTED {
        let path = root.join(rel);
        println!("cargo:rerun-if-changed={}", path.display());
        let mut files = Vec::new();
        collect(&path, &mut files);
        for file in files {
            let Ok(bytes) = std::fs::read(&file) else { continue };
            let rel_file = file
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            lines.push(format!("    ({rel_file:?}, {:?}),", sha256::file_hash(&bytes)));
        }
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("core_hashes.rs");
    let body = format!("pub const CORE_HASHES: &[(&str, &str)] = &[\n{}\n];\n", lines.join("\n"));
    std::fs::write(out, body).unwrap();
}

fn main() {
    write_core_hashes();
    tauri_build::build()
}
