//! Workspace layout — where an agent instance keeps its private state.
//!
//! ADR-082 D1: memory nodes, the conversation index and session meta all live
//! in a single SQLite file inside the workspace. Every component that has to
//! find (or copy) that state — the memory backend, the conversation index,
//! session meta, package cloning — must agree on the path, so it is defined
//! once here instead of being re-spelled as a literal in each crate.

use std::path::{Path, PathBuf};

/// Directory holding agent-private storage inside a workspace.
pub const MEMORY_DIR: &str = "memory";

/// SQLite file inside [`MEMORY_DIR`] holding memory nodes, the conversation
/// index and session meta (ADR-082).
pub const STORE_FILE: &str = "private.sqlite";

/// Sidecar files SQLite may leave next to an open database.
pub const STORE_SIDECAR_SUFFIXES: [&str; 2] = ["-wal", "-shm"];

/// Path of the workspace store: `{work_dir}/memory/private.sqlite`.
pub fn store_path(work_dir: impl AsRef<Path>) -> PathBuf {
    work_dir.as_ref().join(MEMORY_DIR).join(STORE_FILE)
}

/// Every file that currently makes up the workspace store, in copy order: the
/// database itself followed by whichever sidecars exist.
///
/// Callers copying a store (e.g. full package clone) must copy all of them
/// while the source is closed; a `-wal` left behind is unreplayed WAL content.
pub fn store_files(work_dir: impl AsRef<Path>) -> Vec<PathBuf> {
    let db = store_path(work_dir);
    let mut files = vec![db.clone()];
    for suffix in STORE_SIDECAR_SUFFIXES {
        let sidecar = db.with_file_name(format!("{STORE_FILE}{suffix}"));
        if sidecar.exists() {
            files.push(sidecar);
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_path_is_memory_dir_plus_store_file() {
        let path = store_path("/tmp/agent/workspace");
        assert_eq!(
            path,
            Path::new("/tmp/agent/workspace")
                .join(MEMORY_DIR)
                .join(STORE_FILE)
        );
        assert!(path.ends_with("memory/private.sqlite"));
    }

    #[test]
    fn store_files_includes_existing_sidecars_only() {
        let dir = std::env::temp_dir().join(format!("acowork-ws-layout-{}", std::process::id()));
        let memory_dir = dir.join(MEMORY_DIR);
        std::fs::create_dir_all(&memory_dir).unwrap();
        std::fs::write(memory_dir.join(STORE_FILE), b"db").unwrap();
        std::fs::write(memory_dir.join(format!("{STORE_FILE}-wal")), b"wal").unwrap();

        let files = store_files(&dir);
        assert_eq!(files.len(), 2, "db + -wal, -shm absent: {files:?}");
        assert!(files[0].ends_with(STORE_FILE));
        assert!(files[1].ends_with(format!("{STORE_FILE}-wal")));

        std::fs::remove_dir_all(&dir).ok();
    }
}
