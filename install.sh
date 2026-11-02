#!/bin/sh
# Install prk (prebuilt binary, no Rust needed):
#   curl -fsSL https://github.com/pankajyadav-blitz/prk/releases/latest/download/install.sh | sh
# Change the install dir with PRK_INSTALL_DIR=/usr/local/bin, pin a version with PRK_VERSION=v0.1.1
set -eu

repository="pankajyadav-blitz/prk"
install_dir="${PRK_INSTALL_DIR:-$HOME/.local/bin}"
if [ -n "${PRK_VERSION:-}" ]; then
  base_url="https://github.com/${repository}/releases/download/${PRK_VERSION}"
else
  base_url="https://github.com/${repository}/releases/latest/download"
fi

case "$(uname -s)" in
  Linux)  os_part="unknown-linux-musl" ;;
  Darwin) os_part="apple-darwin" ;;
  *) echo "prk: unsupported OS $(uname -s) (on Windows, use WSL)" >&2; exit 1 ;;
esac
case "$(uname -m)" in
  x86_64 | amd64)  arch_part="x86_64" ;;
  arm64 | aarch64) arch_part="aarch64" ;;
  *) echo "prk: unsupported CPU $(uname -m)" >&2; exit 1 ;;
esac
archive_name="prk-${arch_part}-${os_part}.tar.gz"

temporary_dir="$(mktemp -d)"
trap 'rm -rf "$temporary_dir"' EXIT

version="$(curl -fsSL "${base_url}/VERSION" 2>/dev/null || echo unknown)"
echo "prk: downloading ${archive_name} (${version})"
curl -fsSL -o "${temporary_dir}/${archive_name}" "${base_url}/${archive_name}"
curl -fsSL -o "${temporary_dir}/${archive_name}.sha256" "${base_url}/${archive_name}.sha256"

expected_checksum="$(cut -d ' ' -f 1 "${temporary_dir}/${archive_name}.sha256")"
if command -v sha256sum >/dev/null 2>&1; then
  actual_checksum="$(sha256sum "${temporary_dir}/${archive_name}" | cut -d ' ' -f 1)"
else
  actual_checksum="$(shasum -a 256 "${temporary_dir}/${archive_name}" | cut -d ' ' -f 1)"
fi
if [ "$expected_checksum" != "$actual_checksum" ]; then
  echo "prk: checksum mismatch, aborting" >&2
  exit 1
fi

tar -xzf "${temporary_dir}/${archive_name}" -C "$temporary_dir"
mkdir -p "$install_dir"
install -m 755 "${temporary_dir}/prk" "${install_dir}/prk"

echo "prk: installed to ${install_dir}/prk"
case ":$PATH:" in
  *":${install_dir}:"*) ;;
  *) echo "prk: add ${install_dir} to your PATH, e.g. echo 'export PATH=\"${install_dir}:\$PATH\"' >> ~/.zshrc" ;;
esac
echo "prk: next, create ~/.config/prk/config.toml (see the README) and run 'prk check'; update later with 'prk update'"
