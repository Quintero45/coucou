// The protected core, relative to the repository root. Shared with build.rs.
// A folder entry covers everything inside it.

pub const PROTECTED: &[&str] = &[
    "CLAUDE.md",
    "windows/core-directive.md",
    "windows/src-tauri/build.rs",
    "windows/src-tauri/tauri.conf.json",
    "windows/src-tauri/capabilities",
    "windows/src-tauri/src/policy.rs",
    "windows/src-tauri/src/secrets.rs",
    "windows/src-tauri/src/pipe.rs",
    "windows/src-tauri/src/selfmod",
    "windows/hook",
];
