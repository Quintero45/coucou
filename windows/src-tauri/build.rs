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

/// Unit-test executables get no application manifest (tauri-build embeds one
/// in the app binary only), so Windows resolves comctl32 to v5, which lacks
/// TaskDialogIndirect, and the test exe dies at load with
/// STATUS_ENTRYPOINT_NOT_FOUND. Cargo has no link-arg instruction for a lib's
/// unit tests (`rustc-link-arg-tests` is for `tests/` targets only), and a
/// second embedded manifest would clash with tauri-build's resource in the app.
/// So, in debug builds only: delay-load comctl32. Tests never call into it and
/// load fine; the app still gets v6 (its manifest's activation context applies
/// when the DLL loads on first use). Release builds are unchanged.
fn delay_load_comctl32_in_debug() {
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    let debug = std::env::var("PROFILE").as_deref() == Ok("debug");
    if windows && msvc && debug {
        println!("cargo:rustc-link-arg=/DELAYLOAD:comctl32.dll");
        println!("cargo:rustc-link-arg=delayimp.lib");
    }
}

fn main() {
    write_core_hashes();
    delay_load_comctl32_in_debug();
    tauri_build::build()
}
