#!/bin/sh
# Install prk (prebuilt binary, no Rust needed). Linux, macOS, WSL and Git Bash:
#   curl -fsSL https://github.com/pankajyadav-blitz/prk/releases/latest/download/install.sh | sh
# PowerShell or cmd on Windows: use install.ps1 (see the README).
# Change the install dir with PRK_INSTALL_DIR=/usr/local/bin, pin a version with PRK_VERSION=v0.1.1
set -eu

repository="pankajyadav-blitz/prk"
install_dir="${PRK_INSTALL_DIR:-$HOME/.local/bin}"
if [ -n "${PRK_BASE_URL:-}" ]; then
  base_url="${PRK_BASE_URL%/}"
elif [ -n "${PRK_VERSION:-}" ]; then
  base_url="https://github.com/${repository}/releases/download/${PRK_VERSION}"
else
  base_url="https://github.com/${repository}/releases/latest/download"
fi

windows=""
case "$(uname -s)" in
  Linux)  os_part="unknown-linux-musl" ;;
  Darwin) os_part="apple-darwin" ;;
  MINGW* | MSYS* | CYGWIN*) os_part="pc-windows-msvc"; windows=1 ;;
  *) echo "prk: unsupported OS $(uname -s)" >&2; exit 1 ;;
esac
if [ -n "$windows" ]; then
  arch_part="x86_64" # ARM Windows runs the x64 build
  archive_name="prk-${arch_part}-${os_part}.zip"
  binary_name="prk.exe"
else
  case "$(uname -m)" in
    x86_64 | amd64)  arch_part="x86_64" ;;
    arm64 | aarch64) arch_part="aarch64" ;;
    *) echo "prk: unsupported CPU $(uname -m)" >&2; exit 1 ;;
  esac
  archive_name="prk-${arch_part}-${os_part}.tar.gz"
  binary_name="prk"
fi

temporary_dir="$(mktemp -d)"
trap 'rm -rf "$temporary_dir"' EXIT

version="$(curl -fsSL "${base_url}/VERSION" 2>/dev/null || echo unknown)"
echo "prk: downloading ${archive_name} (${version})"
curl -fsSL -o "${temporary_dir}/${archive_name}" "${base_url}/${archive_name}"
curl -fsSL -o "${temporary_dir}/${archive_name}.sha256" "${base_url}/${archive_name}.sha256"

expected_checksum="$(cut -d ' ' -f 1 "${temporary_dir}/${archive_name}.sha256" | tr -d '\r')"
if command -v sha256sum >/dev/null 2>&1; then
  actual_checksum="$(sha256sum "${temporary_dir}/${archive_name}" | cut -d ' ' -f 1)"
else
  actual_checksum="$(shasum -a 256 "${temporary_dir}/${archive_name}" | cut -d ' ' -f 1)"
fi
if [ "$expected_checksum" != "$actual_checksum" ]; then
  echo "prk: checksum mismatch, aborting" >&2
  exit 1
fi

mkdir -p "$install_dir"
if [ -n "$windows" ]; then
  if command -v unzip >/dev/null 2>&1; then
    unzip -q -o "${temporary_dir}/${archive_name}" -d "$temporary_dir"
  else
    powershell.exe -NoProfile -Command "Expand-Archive -Force -LiteralPath '$(cygpath -w "${temporary_dir}/${archive_name}")' -DestinationPath '$(cygpath -w "$temporary_dir")'"
  fi
  cp "${temporary_dir}/${binary_name}" "${install_dir}/${binary_name}"
else
  tar -xzf "${temporary_dir}/${archive_name}" -C "$temporary_dir"
  install -m 755 "${temporary_dir}/${binary_name}" "${install_dir}/${binary_name}"
fi

echo "prk: installed to ${install_dir}/${binary_name}"
if [ -n "$windows" ]; then
  # Also on the Windows user PATH, so PowerShell and cmd find it (new windows).
  windows_dir="$(cygpath -w "$install_dir")"
  powershell.exe -NoProfile -Command "\$d='${windows_dir}'; \$p=[Environment]::GetEnvironmentVariable('Path','User'); if (-not ((\$p -split ';') -contains \$d)) { [Environment]::SetEnvironmentVariable('Path', ((@(\$p, \$d) | Where-Object { \$_ }) -join ';'), 'User'); Write-Host \"prk: added \$d to your Windows PATH (open a new PowerShell or cmd window)\" }" || true
fi
case ":$PATH:" in
  *":${install_dir}:"*) ;;
  *)
    case "${SHELL:-}" in
      *zsh) rc="~/.zshrc" ;;
      *) rc="~/.bashrc" ;;
    esac
    echo "prk: add ${install_dir} to your PATH, e.g. echo 'export PATH=\"${install_dir}:\$PATH\"' >> ${rc}" ;;
esac
if [ -n "$windows" ]; then
  echo "prk: next, run 'winpty prk init' in Git Bash (or 'prk init' in PowerShell, cmd or Windows Terminal)"
else
  echo "prk: next, run 'prk init' to set it up; update later with 'prk update'"
fi
