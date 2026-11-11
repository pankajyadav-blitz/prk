//! `prk clear`: remove everything prk keeps locally about past work (the queue and
//! history database, saved comments, review logs, leftover clones). The config
//! file is never touched. Only prk's own entries inside `workdir` are removed,
//! never the folder itself or anything else in it.

use std::path::{Path, PathBuf};

/// What prk writes into `workdir`.
const ENTRIES: [&str; 5] = [
    "state.sqlite3",
    "state.sqlite3-wal",
    "state.sqlite3-shm",
    "logs",
    "repos",
];

/// The entries of `workdir` that exist, with their size in bytes.
pub fn found(workdir: &Path) -> Vec<(PathBuf, u64)> {
    ENTRIES
        .iter()
        .map(|name| workdir.join(name))
        .filter(|p| p.symlink_metadata().is_ok())
        .map(|p| {
            let size = size_of(&p);
            (p, size)
        })
        .collect()
}

fn size_of(path: &Path) -> u64 {
    let Ok(meta) = path.symlink_metadata() else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path)
        .map(|entries| entries.flatten().map(|e| size_of(&e.path())).sum())
        .unwrap_or(0)
}

/// Remove the entries; returns the ones that could not be removed.
pub fn remove(entries: &[(PathBuf, u64)]) -> Vec<String> {
    let mut failed = Vec::new();
    for (path, _) in entries {
        let result = match path.symlink_metadata() {
            Ok(m) if m.is_dir() => std::fs::remove_dir_all(path),
            Ok(_) => std::fs::remove_file(path),
            Err(_) => Ok(()),
        };
        if let Err(e) = result {
            failed.push(format!("{}: {e}", path.display()));
        }
    }
    failed
}

pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_only_prks_own_entries() {
        let dir = std::env::temp_dir().join(format!("prk-clear-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        std::fs::create_dir_all(dir.join("repos/org/repo-pr1")).unwrap();
        std::fs::write(dir.join("state.sqlite3"), vec![0u8; 2048]).unwrap();
        std::fs::write(dir.join("logs/a.log"), "log").unwrap();
        std::fs::write(dir.join("repos/org/repo-pr1/f"), "x").unwrap();
        std::fs::write(dir.join("keep.txt"), "mine").unwrap();

        let entries = found(&dir);
        assert_eq!(entries.len(), 3);
        assert!(entries.iter().map(|e| e.1).sum::<u64>() >= 2048);
        assert!(remove(&entries).is_empty());

        assert!(found(&dir).is_empty());
        assert!(dir.join("keep.txt").exists(), "unrelated files stay");
        assert!(dir.exists(), "the folder itself stays");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_empty_workdir_has_nothing_to_clear() {
        let dir = std::env::temp_dir().join(format!("prk-clear-none-{}", std::process::id()));
        assert!(found(&dir).is_empty());
        assert!(remove(&[]).is_empty());
    }

    #[test]
    fn sizes_read_well() {
        assert_eq!(human(500), "500 B");
        assert_eq!(human(2048), "2.0 KB");
        assert_eq!(human(5 * 1024 * 1024), "5.0 MB");
    }
}
