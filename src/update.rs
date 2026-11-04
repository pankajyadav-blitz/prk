//! `prk update`: replace the running binary with the latest GitHub release when
//! it is newer. Downloads from the public releases page and checks the archive
//! against its published sha256 before swapping the binary in place.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use sha2::{Digest, Sha256};

pub const REPOSITORY: &str = "pankajyadav-blitz/prk";

type Result<T> = std::result::Result<T, String>;

/// What the previous binary is renamed to on Windows, where a running .exe
/// can be renamed but not overwritten. Removed on the next start.
const OLD_EXE: &str = "prk.old.exe";

pub fn self_update() -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let target = release_target()?;
    let archive = archive_name(target);
    let staging = std::env::temp_dir().join(format!("prk-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .map_err(|e| format!("creating {}: {e}", staging.display()))?;
    let outcome = (|| {
        download("VERSION", &staging)?;
        let tag = std::fs::read_to_string(staging.join("VERSION")).map_err(|e| e.to_string())?;
        let latest = tag.trim().trim_start_matches('v').to_string();
        if !is_newer(&latest, current) {
            println!("prk {current} is already the latest version");
            return Ok(());
        }
        println!("updating prk {current} -> {latest} ({target})");
        download(&archive, &staging)?;
        download(&format!("{archive}.sha256"), &staging)?;
        let path = install_from_archive(&staging, &archive)?;
        println!("updated {} to {latest}", path.display());
        if cfg!(windows) {
            println!("restart a running prk to use it");
        } else {
            println!("restart a running prk (or `systemctl --user restart prk`) to use it");
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&staging);
    outcome
}

/// Delete the binary a Windows update renamed aside. Harmless elsewhere.
pub fn remove_old_binary() {
    if cfg!(windows)
        && let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let _ = std::fs::remove_file(dir.join(OLD_EXE));
    }
}

fn base_url() -> String {
    std::env::var("PRK_BASE_URL")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| format!("https://github.com/{REPOSITORY}/releases/latest/download"))
}

fn download(asset: &str, dir: &Path) -> Result<()> {
    let url = format!("{}/{asset}", base_url().trim_end_matches('/'));
    let http = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|e| e.to_string())?;
    let bytes = http
        .get(&url)
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.bytes())
        .map_err(|e| format!("downloading {url} failed: {e}"))?;
    std::fs::write(dir.join(asset), &bytes).map_err(|e| format!("saving {asset}: {e}"))
}

fn release_target() -> Result<&'static str> {
    Ok(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        // ARM Windows runs the x64 build.
        ("windows", "x86_64" | "aarch64") => "x86_64-pc-windows-msvc",
        (os, arch) => return Err(format!("there is no prk release for {os}/{arch}")),
    })
}

fn archive_name(target: &str) -> String {
    if target.contains("windows") {
        format!("prk-{target}.zip")
    } else {
        format!("prk-{target}.tar.gz")
    }
}

fn is_newer(candidate: &str, current: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> { v.split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    parts(candidate) > parts(current)
}

fn install_from_archive(staging: &Path, archive: &str) -> Result<PathBuf> {
    let checksum_file = staging.join(format!("{archive}.sha256"));
    let expected = std::fs::read_to_string(&checksum_file)
        .map_err(|e| format!("reading {}: {e}", checksum_file.display()))?
        .split_whitespace()
        .next()
        .ok_or("the release's checksum file is empty")?
        .to_lowercase();
    if sha256_of(&staging.join(archive))? != expected {
        return Err(format!(
            "checksum mismatch for {archive}; nothing was changed"
        ));
    }
    extract(&staging.join(archive), staging)?;

    let binary = format!("prk{}", std::env::consts::EXE_SUFFIX);
    let current_exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|e| format!("locating the running prk binary: {e}"))?;
    let dir = current_exe
        .parent()
        .ok_or("the prk binary has no parent directory")?;
    let replacement = dir.join(format!(".prk-update{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(staging.join(&binary), &replacement).map_err(|e| {
        format!(
            "writing to {} ({e}); rerun the install script instead",
            dir.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }
    if cfg!(windows) {
        let old = dir.join(OLD_EXE);
        let _ = std::fs::remove_file(&old);
        std::fs::rename(&current_exe, &old)
            .map_err(|e| format!("moving {} aside: {e}", current_exe.display()))?;
        if let Err(e) = std::fs::rename(&replacement, &current_exe) {
            let _ = std::fs::rename(&old, &current_exe);
            return Err(format!("replacing {}: {e}", current_exe.display()));
        }
    } else {
        std::fs::rename(&replacement, &current_exe)
            .map_err(|e| format!("replacing {}: {e}", current_exe.display()))?;
    }
    Ok(current_exe)
}

/// Unpack the release archive into `dir`. Windows 10 and later ship a tar.exe
/// (bsdtar) that reads zip; it is called by its full path because Git Bash puts
/// a GNU tar, which cannot, first on PATH.
fn extract(archive: &Path, dir: &Path) -> Result<()> {
    let tar = if cfg!(windows) {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
        Path::new(&root)
            .join("System32")
            .join("tar.exe")
            .to_string_lossy()
            .into_owned()
    } else {
        "tar".to_string()
    };
    let flags = if cfg!(windows) { "-xf" } else { "-xzf" };
    let status = Command::new(&tar)
        .arg(flags)
        .arg(archive)
        .arg("-C")
        .arg(dir)
        .status()
        .map_err(|e| format!("running {tar}: {e}"))?;
    if !status.success() {
        return Err(format!("{tar} could not extract {}", archive.display()));
    }
    Ok(())
}

fn sha256_of(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    Ok(Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_like_sha256sum() {
        let dir = std::env::temp_dir().join(format!("prk-sha-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("x");
        std::fs::write(&file, "abc").unwrap();
        assert_eq!(
            sha256_of(&file).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn every_platform_has_an_archive() {
        let target = release_target().unwrap();
        let archive = archive_name(target);
        assert!(archive.ends_with(if cfg!(windows) { ".zip" } else { ".tar.gz" }));
        assert_eq!(
            archive_name("x86_64-pc-windows-msvc"),
            "prk-x86_64-pc-windows-msvc.zip"
        );
    }

    #[test]
    fn compares_versions_numerically() {
        assert!(is_newer("0.1.1", "0.1.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
    }
}
