//! `prk update`: replace the running binary with the latest GitHub release when
//! it is newer. Downloads with `curl` from the public releases page and checks
//! the archive against its published sha256 before swapping the binary in place.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const REPOSITORY: &str = "pankajyadav-blitz/prk";

type Result<T> = std::result::Result<T, String>;

pub fn self_update() -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let target = release_target()?;
    let archive = format!("prk-{target}.tar.gz");
    let staging = std::env::temp_dir().join(format!("prk-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("creating {}: {e}", staging.display()))?;
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
        println!("restart a running prk (or `systemctl --user restart prk`) to use it");
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&staging);
    outcome
}

fn download(asset: &str, dir: &Path) -> Result<()> {
    let url = format!("https://github.com/{REPOSITORY}/releases/latest/download/{asset}");
    let output = Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(dir.join(asset))
        .arg(&url)
        .output()
        .map_err(|e| format!("running curl: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "downloading {url} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

fn release_target() -> Result<&'static str> {
    Ok(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        (os, arch) => return Err(format!("there is no prk release for {os}/{arch}")),
    })
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
        .to_string();
    if sha256_of(&staging.join(archive))? != expected {
        return Err(format!("checksum mismatch for {archive}; nothing was changed"));
    }
    let extracted = Command::new("tar")
        .arg("-xzf")
        .arg(staging.join(archive))
        .arg("-C")
        .arg(staging)
        .status()
        .map_err(|e| format!("running tar: {e}"))?;
    if !extracted.success() {
        return Err(format!("tar could not extract {archive}"));
    }

    let current_exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|e| format!("locating the running prk binary: {e}"))?;
    let dir = current_exe.parent().ok_or("the prk binary has no parent directory")?;
    let replacement = dir.join(".prk-update");
    std::fs::copy(staging.join("prk"), &replacement).map_err(|e| {
        format!("writing to {} ({e}); rerun the install script instead", dir.display())
    })?;
    std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| e.to_string())?;
    std::fs::rename(&replacement, &current_exe)
        .map_err(|e| format!("replacing {}: {e}", current_exe.display()))?;
    Ok(current_exe)
}

fn sha256_of(path: &Path) -> Result<String> {
    for (program, args) in [("sha256sum", &[][..]), ("shasum", &["-a", "256"][..])] {
        if let Ok(output) = Command::new(program).args(args).arg(path).output()
            && output.status.success()
        {
            return Ok(String::from_utf8_lossy(&output.stdout)
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_string());
        }
    }
    Err("neither sha256sum nor shasum is available to verify the download".into())
}

#[cfg(test)]
mod tests {
    use super::is_newer;

    #[test]
    fn compares_versions_numerically() {
        assert!(is_newer("0.1.1", "0.1.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
    }
}
