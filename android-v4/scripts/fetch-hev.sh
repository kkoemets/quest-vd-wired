#!/usr/bin/env bash
set -euo pipefail

revision="${1:?HEV revision is required}"
root="$(cd "$(dirname "$0")/.." && pwd)"
target="$root/.deps/hev-socks5-tunnel"
lifecycle_patch="$root/patches/hev-lifecycle.patch"
split_udp_patch="$root/patches/hev-split-udp-port.patch"
timeout_phases_patch="$root/patches/hev-timeout-phases.patch"
lwip_window_patch="$root/patches/hev-lwip-window.patch"
root_patches=("$lifecycle_patch" "$split_udp_patch" "$timeout_phases_patch")
lwip_patches=("$lwip_window_patch")
expected_patch_files=$'src/hev-config.c\nsrc/hev-config.h\nsrc/hev-jni.c\nsrc/hev-main.c\nsrc/hev-main.h\nsrc/hev-socks5-session.c\nsrc/hev-socks5-tunnel.c'
expected_lwip_patch_files=$'src/ports/include/lwipopts.h'

verify_exact_patch_content() (
    expected="$(mktemp -d)"
    trap 'rm -rf "$expected"' EXIT
    git -C "$target" archive "$revision" | tar -x -C "$expected"
    for patch in "${root_patches[@]}"; do
        git -C "$expected" apply "$patch"
    done
    while IFS= read -r file; do
        if ! cmp -s "$target/$file" "$expected/$file"; then
            echo "HEV checkout differs from the exact pinned patch content: $file" >&2
            exit 1
        fi
    done <<< "$expected_patch_files"

    lwip_expected="$(mktemp -d)"
    git -C "$target/third-part/lwip" archive HEAD | tar -x -C "$lwip_expected"
    for patch in "${lwip_patches[@]}"; do
        git -C "$lwip_expected" apply "$patch"
    done
    while IFS= read -r file; do
        if ! cmp -s "$target/third-part/lwip/$file" "$lwip_expected/$file"; then
            echo "HEV lwIP checkout differs from the exact pinned patch content: $file" >&2
            rm -rf "$lwip_expected"
            exit 1
        fi
    done <<< "$expected_lwip_patch_files"
    rm -rf "$lwip_expected"
)

verify_pinned_patch() {
    git -C "$target" diff --cached --quiet
    git -C "$target/src/core" diff --quiet
    git -C "$target/src/core" diff --cached --quiet
    git -C "$target/third-part/hev-task-system" diff --quiet
    git -C "$target/third-part/hev-task-system" diff --cached --quiet
    git -C "$target/third-part/yaml" diff --quiet
    git -C "$target/third-part/yaml" diff --cached --quiet
    git -C "$target" diff --ignore-submodules=all --check
    git -C "$target/third-part/lwip" diff --cached --quiet
    git -C "$target/third-part/lwip" diff --check

    for patch in "${root_patches[@]}"; do
        if git -C "$target" apply --reverse --check "$patch"; then
            continue
        fi
        if ! git -C "$target" apply --check "$patch"; then
            echo "HEV checkout cannot apply project patch: $patch" >&2
            exit 1
        fi
        git -C "$target" apply "$patch"
        if ! git -C "$target" apply --reverse --check "$patch"; then
            echo "HEV checkout does not contain project patch: $patch" >&2
            exit 1
        fi
    done
    for patch in "${lwip_patches[@]}"; do
        if git -C "$target/third-part/lwip" apply --reverse --check "$patch"; then
            continue
        fi
        if ! git -C "$target/third-part/lwip" apply --check "$patch"; then
            echo "HEV lwIP checkout cannot apply project patch: $patch" >&2
            exit 1
        fi
        git -C "$target/third-part/lwip" apply "$patch"
        if ! git -C "$target/third-part/lwip" apply --reverse --check "$patch"; then
            echo "HEV lwIP checkout does not contain project patch: $patch" >&2
            exit 1
        fi
    done
    actual_patch_files="$(git -C "$target" diff --ignore-submodules=all --name-only | LC_ALL=C sort)"
    if [[ "$actual_patch_files" != "$expected_patch_files" ]]; then
        echo "HEV checkout contains changes outside the pinned project patches" >&2
        exit 1
    fi
    actual_lwip_patch_files="$(git -C "$target/third-part/lwip" diff --name-only | LC_ALL=C sort)"
    if [[ "$actual_lwip_patch_files" != "$expected_lwip_patch_files" ]]; then
        echo "HEV lwIP checkout contains changes outside the pinned project patches" >&2
        exit 1
    fi
    verify_exact_patch_content
}

if [[ -d "$target/.git" ]] && [[ "$(git -C "$target" rev-parse HEAD)" == "$revision" ]]; then
    git -C "$target" submodule update --init --recursive --depth 1
    verify_pinned_patch
    exit 0
fi

rm -rf "$target"
mkdir -p "$(dirname "$target")"
git clone --no-checkout https://github.com/heiher/hev-socks5-tunnel.git "$target"
git -C "$target" checkout --detach "$revision"
git -C "$target" submodule update --init --recursive --depth 1
verify_pinned_patch
