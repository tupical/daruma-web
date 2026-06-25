#!/usr/bin/env sh
# link-oss.sh — Ensure vendor/oss is populated.
#
# vendor/oss is a git submodule (tupical/daruma.git), replacing the former
# local symlink. This initialises/updates that submodule so Cargo path deps
# under vendor/oss/crates/* resolve.
#
# Usage (from repo root):
#   sh scripts/link-oss.sh

set -eu

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${ROOT}"

git submodule update --init vendor/oss

if [ ! -f vendor/oss/Cargo.toml ]; then
  echo "ERROR: vendor/oss submodule did not populate (no Cargo.toml)." >&2
  exit 1
fi

echo "vendor/oss submodule ready -> $(cd vendor/oss && git rev-parse --short HEAD)"
