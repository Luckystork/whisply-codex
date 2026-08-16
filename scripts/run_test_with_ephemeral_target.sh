#!/usr/bin/env bash
# Run the standard Rust test path in a disposable Cargo target directory.
#
# Whisply's Rust workspace is intentionally large; leaving its debug target in the checkout can
# consume tens of gigabytes. Tests do not need to retain those artifacts, so every invocation gets
# an isolated target directory that is removed whether the build passes, fails, or is interrupted.
set -euo pipefail

tmp_root="${TMPDIR:-/tmp}"
target_dir="$(mktemp -d "${tmp_root}/whisply-cargo-test-target.XXXXXXXX")"
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
workspace_root="$(cd -- "${script_dir}/.." && pwd -P)"
persistent_nextest_dir="${workspace_root}/whisply-rs/target/nextest"
persistent_junit_report="${persistent_nextest_dir}/local/junit.xml"
persistent_junit_existed=0
if [[ -e "${persistent_junit_report}" ]]; then
    persistent_junit_existed=1
fi

cleanup() {
    local exit_code=$?
    case "${target_dir}" in
        "${tmp_root}"/whisply-cargo-test-target.*)
            if ! rm -rf -- "${target_dir}"; then
                printf 'could not remove disposable Cargo target: %s\n' "${target_dir}" >&2
                exit_code=1
            fi
            ;;
        *)
            printf 'refusing to remove unexpected Cargo target: %s\n' "${target_dir}" >&2
            exit_code=1
            ;;
    esac

    # Nextest can still materialize its conventional workspace JUnit path even
    # when its target and JUnit paths are redirected. Delete that one report
    # only when this invocation observed it absent at startup; an existing
    # user report is never touched. Then remove only an empty directory chain.
    if [[ "${persistent_junit_existed}" == "0" && -e "${persistent_junit_report}" ]]; then
        if ! rm -f -- "${persistent_junit_report}"; then
            printf 'could not remove wrapper-owned Nextest JUnit report: %s\n' "${persistent_junit_report}" >&2
            exit_code=1
        fi
    fi
    rmdir "${persistent_nextest_dir}/local" 2>/dev/null || true
    rmdir "${persistent_nextest_dir}" 2>/dev/null || true
    rmdir "$(dirname -- "${persistent_nextest_dir}")" 2>/dev/null || true

    return "${exit_code}"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

export CARGO_TARGET_DIR="${target_dir}"
# The target disappears after this command, so Cargo's incremental cache can
# never speed a later run. Suppress it to keep the temporary peak lower.
export CARGO_INCREMENTAL=0

# Reuse the same disposable target for a narrowly scoped Cargo-backed maintenance command.
# This keeps generators from quietly recreating a persistent multi-gigabyte target directory.
if [[ "${1:-}" == "--run" ]]; then
    shift
    if [[ "$#" -eq 0 ]]; then
        printf '%s\n' 'expected a command after --run' >&2
        exit 2
    fi
    "$@"
    exit
fi

if [[ "${WHISPLY_TEST_SKIP_HELPERS:-}" != "1" ]]; then
    cargo build -p whisply-cli --bin whisply
    cargo build -p whisply-rmcp-client --bin test_stdio_server

    # Focused tests that do not exercise Code Mode can opt out of this independent helper build.
    # The default preserves the complete helper set; the opt-out only avoids an unavailable upstream
    # Rusty V8 archive while validating an unrelated package.
    if [[ "${WHISPLY_TEST_SKIP_CODE_MODE_HOST:-}" != "1" ]]; then
        cargo build -p whisply-code-mode-host --bin codex-code-mode-host
    fi
fi

if [[ "${1:-}" == "--helpers-only" ]]; then
    exit 0
fi

set +e
# Nextest resolves a relative JUnit report path against the workspace target,
# even when Cargo's target is redirected above. Feed it a disposable copy of
# the reviewed local profile with an absolute report destination, so no test
# artifact persists in the checkout.
nextest_config="${target_dir}/nextest.toml"
cp "${workspace_root}/whisply-rs/.config/nextest.toml" "${nextest_config}"
printf '\n[profile.local.junit]\npath = "%s/junit.xml"\n' "${target_dir}" >> "${nextest_config}"
RUST_MIN_STACK=67108864 NEXTEST_PROFILE=local cargo nextest run --config-file "${nextest_config}" --target-dir "${target_dir}" --no-fail-fast "$@"
test_status=$?
set -e

if [[ -n "${WHISPLY_RUST_TEST_STATUS_FILE:-}" ]]; then
    printf '%s\n' "${test_status}" > "${WHISPLY_RUST_TEST_STATUS_FILE}"
fi

exit "${test_status}"
