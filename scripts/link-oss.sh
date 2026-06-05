#!/usr/bin/env sh
# link-oss.sh — Create vendor/oss → local TaskAgent OSS checkout (symlink).
#
# taskagent-web consumes the OSS crates (shared/domain/events/api-dto) read-only
# via Cargo path deps under vendor/oss. OSS is developed in a sibling repo.
#
# Usage (from taskagent-web root):
#   sh scripts/link-oss.sh
#
# Override auto-detect:
#   TASKAGENT_OSS_ROOT=/path/to/taskagent sh scripts/link-oss.sh

set -eu

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WEB_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
LINK_PATH="${WEB_ROOT}/vendor/oss"

resolve_oss_root() {
  if [ -n "${TASKAGENT_OSS_ROOT:-}" ]; then
    printf '%s' "${TASKAGENT_OSS_ROOT}"
    return 0
  fi
  parent="$(cd "${WEB_ROOT}/.." && pwd)"
  for name in taskagent; do
    candidate="${parent}/${name}"
    if [ -f "${candidate}/Cargo.toml" ] && [ -d "${candidate}/crates" ]; then
      printf '%s' "${candidate}"
      return 0
    fi
  done
  return 1
}

oss_root="$(resolve_oss_root)" || {
  echo "ERROR: TaskAgent OSS checkout not found." >&2
  echo "       Clone it next to this repo (../taskagent) or set TASKAGENT_OSS_ROOT." >&2
  exit 1
}

mkdir -p "${WEB_ROOT}/vendor"

if [ -L "${LINK_PATH}" ]; then
  rm "${LINK_PATH}"
elif [ -e "${LINK_PATH}" ]; then
  echo "ERROR: '${LINK_PATH}' exists and is not a symlink — remove it manually." >&2
  exit 1
fi

rel_target="$(python3 -c "import os; print(os.path.relpath('${oss_root}', '${WEB_ROOT}/vendor'))" 2>/dev/null \
  || realpath --relative-to="${WEB_ROOT}/vendor" "${oss_root}" 2>/dev/null \
  || printf '%s' "${oss_root}")"

ln -s "${rel_target}" "${LINK_PATH}"
echo "Linked vendor/oss -> ${rel_target} ($(cd "${LINK_PATH}" && pwd))"
