#!/usr/bin/env bash
set -euo pipefail

revision="${1:?HEV revision is required}"
root="$(cd "$(dirname "$0")/.." && pwd)"
target="$root/.deps/hev-socks5-tunnel"
lifecycle_patch="$root/patches/hev-lifecycle.patch"
split_udp_patch="$root/patches/hev-split-udp-port.patch"
patches=("$lifecycle_patch" "$split_udp_patch")
expected_patch_files=$'src/hev-config.c\nsrc/hev-config.h\nsrc/hev-jni.c\nsrc/hev-main.c\nsrc/hev-main.h\nsrc/hev-socks5-session.c\nsrc/hev-socks5-tunnel.c'

verify_exact_patch_content() (
    expected="$(mktemp -d)"
    trap 'rm -rf "$expected"' EXIT
    git -C "$target" archive "$revision" | tar -x -C "$expected"
    for patch in "${patches[@]}"; do
        git -C "$expected" apply "$patch"
    done
    while IFS= read -r file; do
        if ! cmp -s "$target/$file" "$expected/$file"; then
            echo "HEV checkout differs from the exact pinned patch content: $file" >&2
            exit 1
        fi
    done <<< "$expected_patch_files"
)

verify_pinned_patch() {
    git -C "$target" diff --cached --quiet
    git -C "$target" submodule foreach --quiet --recursive \
        'git diff --quiet && git diff --cached --quiet'
    git -C "$target" diff --check

    for patch in "${patches[@]}"; do
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
    actual_patch_files="$(git -C "$target" diff --name-only | LC_ALL=C sort)"
    if [[ "$actual_patch_files" != "$expected_patch_files" ]]; then
        echo "HEV checkout contains changes outside the pinned project patches" >&2
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
