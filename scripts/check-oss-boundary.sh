#!/usr/bin/env bash
# check-oss-boundary.sh — CI guard: fail if Cloud/SaaS/cabinet terms or
# Cloud-specific endpoints leak into the OSS web viewer source.
#
# OSS-policy summary:
#   ALLOWED  generic relative endpoints: /v1/*, /.well-known/daruma-shell.json
#   FORBIDDEN Cloud/SaaS brand terms and any hardcoded Cloud-host URLs
#
# Run:  bash scripts/check-oss-boundary.sh
# Exit: 0 = clean, 1 = violation found

set -euo pipefail

SCAN_DIR="${1:-src}"
SCRIPT_NAME="check-oss-boundary.sh"

# ---------------------------------------------------------------------------
# Forbidden patterns (case-insensitive where noted)
# Each entry: <label>|<grep-pattern>
# ---------------------------------------------------------------------------
declare -a PATTERNS=(
    # Cloud/SaaS brand terms (case-insensitive)
    "cloud-term-ci|(?i)\bcloud\b"
    "saas-term-ci|(?i)\bsaas\b"
    "cabinet-term-ci|(?i)\bcabinet\b"
    # mcpbox.ru and any *.mcpbox.* references
    "mcpbox-domain|mcpbox"
    # Hardcoded Cloud host URLs (non-generic hostnames in string literals)
    "hardcoded-https-host|\"https://[a-zA-Z0-9._-]+\.[a-zA-Z]{2,}/[^\"]*\""
)

# ---------------------------------------------------------------------------
# Whitelist: lines matching these patterns are allowed even if a forbidden
# pattern also matches.  Used for OSS-policy documentation inside guard
# scripts and comments that explicitly name the policy.
# ---------------------------------------------------------------------------
declare -a WHITELIST=(
    # The guard script itself — skip its own comment text
    "${SCRIPT_NAME}"
    # Lines that are pure Rust doc-comments explaining the OSS boundary policy
    "^[[:space:]]*//"
    # Allow /.well-known/daruma-shell.json occurrences (generic endpoint)
    "well-known/daruma-shell"
)

found=0
violations=()

for entry in "${PATTERNS[@]}"; do
    label="${entry%%|*}"
    pattern="${entry#*|}"

    # grep -P for PCRE (enables (?i) and \b); -n for line numbers; -r recursive
    while IFS= read -r hit; do
        # Apply whitelist: skip if the hit line matches any whitelist pattern
        skip=0
        for wl in "${WHITELIST[@]}"; do
            if echo "${hit}" | grep -qE "${wl}"; then
                skip=1
                break
            fi
        done
        if [[ "${skip}" -eq 0 ]]; then
            violations+=("[${label}] ${hit}")
            found=1
        fi
    done < <(grep -rPn "${pattern}" "${SCAN_DIR}" 2>/dev/null || true)
done

if [[ "${found}" -eq 0 ]]; then
    echo "OSS boundary check passed — no Cloud/SaaS leaks detected in ${SCAN_DIR}/"
    exit 0
else
    echo "OSS boundary check FAILED — Cloud/SaaS leak(s) detected:" >&2
    for v in "${violations[@]}"; do
        echo "  ${v}" >&2
    done
    echo "" >&2
    echo "Only generic endpoints (/v1/*, /.well-known/daruma-shell.json) are" >&2
    echo "allowed in the OSS web viewer. Remove all Cloud/SaaS-specific terms." >&2
    exit 1
fi
