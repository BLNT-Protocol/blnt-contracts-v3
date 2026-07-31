#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
BASELINES_FILE="${ROOT_DIR}/wasm-size-v2-baseline.json"
WASM_DIR="${ROOT_DIR}/target/wasm32v1-none/optimized"

: "${MAX_WASM_BYTES:?MAX_WASM_BYTES must be provided by the build}"
if [[ ! "${MAX_WASM_BYTES}" =~ ^[0-9]+$ ]] || (( MAX_WASM_BYTES <= 0 )); then
    printf 'error: MAX_WASM_BYTES must be a positive integer\n' >&2
    exit 1
fi

command -v jq >/dev/null 2>&1 || {
    printf 'error: jq is required for the WASM size report\n' >&2
    exit 1
}
[[ -s "${BASELINES_FILE}" ]] || {
    printf 'error: missing WASM size baselines: %s\n' "${BASELINES_FILE}" >&2
    exit 1
}

file_bytes() {
    wc -c <"$1" | tr -d '[:space:]'
}

baseline_bytes() {
    local baseline="$1" contract="$2"
    jq -er --arg baseline "${baseline}" --arg contract "${contract}" \
        '.[$baseline].contracts[$contract].bytes' "${BASELINES_FILE}"
}

format_delta() {
    local current="$1" baseline="$2" delta
    if (( baseline == 0 )); then
        printf 'new'
        return
    fi
    delta=$((current - baseline))
    awk -v delta="${delta}" -v current="${current}" -v baseline="${baseline}" \
        'BEGIN { printf "%+d B (%+.1f%%)", delta, (current - baseline) * 100 / baseline }'
}

for artifact in backstop.wasm pool.wasm pool_factory.wasm; do
    [[ -s "${WASM_DIR}/${artifact}" ]] || {
        printf 'error: missing optimized artifact %s; run make build first\n' \
            "${WASM_DIR}/${artifact}" >&2
        exit 1
    }
done

current_commit="$(git -C "${ROOT_DIR}" rev-parse HEAD)"
current_dirty=false
if ! git -C "${ROOT_DIR}" diff --quiet --ignore-submodules -- ||
    ! git -C "${ROOT_DIR}" diff --cached --quiet --ignore-submodules --
then
    current_dirty=true
fi

current_backstop="$(file_bytes "${WASM_DIR}/backstop.wasm")"
current_pool="$(file_bytes "${WASM_DIR}/pool.wasm")"
current_factory="$(file_bytes "${WASM_DIR}/pool_factory.wasm")"

v2_backstop="$(baseline_bytes deployed_v2 backstop)"
v2_pool="$(baseline_bytes deployed_v2 pool)"
v2_factory="$(baseline_bytes deployed_v2 pool_factory)"
v2_total=$((v2_backstop + v2_pool + v2_factory))
current_total=$((current_backstop + current_pool + current_factory))

printf '# Optimized WASM size report\n\n'
printf -- "- Current commit: \`%s\`" "${current_commit}"
if [[ "${current_dirty}" == "true" ]]; then
    printf ' (dirty worktree)'
fi
printf '\n'
printf -- '- Per-contract ceiling: %d bytes\n\n' "${MAX_WASM_BYTES}"
printf '| Contract | Deployed v2 | Current v3 | Change |\n'
printf '|---|---:|---:|---:|\n'
printf '| Backstop | %d B | %d B | %s |\n' \
    "${v2_backstop}" "${current_backstop}" \
    "$(format_delta "${current_backstop}" "${v2_backstop}")"
printf '| Pool | %d B | %d B | %s |\n' \
    "${v2_pool}" "${current_pool}" \
    "$(format_delta "${current_pool}" "${v2_pool}")"
printf '| Pool factory | %d B | %d B | %s |\n' \
    "${v2_factory}" "${current_factory}" \
    "$(format_delta "${current_factory}" "${v2_factory}")"
printf '| **Total** | **%d B** | **%d B** | **%s** |\n\n' \
    "${v2_total}" "${current_total}" \
    "$(format_delta "${current_total}" "${v2_total}")"
printf 'Totals compare stored code size only; each contract is uploaded separately.\n'
