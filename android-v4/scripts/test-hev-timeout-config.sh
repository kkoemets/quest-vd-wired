#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
revision="${1:-c6e4c72246fb0f20bda299f0efc7814bb3098d57}"
hev="$root/.deps/hev-socks5-tunnel"
build="$(mktemp -d)"
trap 'rm -rf "$build"' EXIT

"$root/scripts/fetch-hev.sh" "$revision"

yaml_sources=("$hev"/third-part/yaml/src/*.c)
cc \
    -std=c11 \
    -Wall \
    -Wextra \
    -Werror \
    -DYAML_VERSION_MAJOR=0 \
    -DYAML_VERSION_MINOR=2 \
    -DYAML_VERSION_PATCH=5 \
    '-DYAML_VERSION_STRING="0.2.5"' \
    -I"$hev/src" \
    -I"$hev/src/misc" \
    -I"$hev/third-part/yaml/src" \
    -I"$hev/third-part/lwip/src/include" \
    -I"$hev/third-part/lwip/src/ports/include" \
    "$root/tests/hev_timeout_config_test.c" \
    "$hev/src/hev-config.c" \
    "${yaml_sources[@]}" \
    -o "$build/hev-timeout-config-test"

"$build/hev-timeout-config-test"

cc \
    -std=c11 \
    -Wall \
    -Wextra \
    -Werror \
    -Wno-unused-parameter \
    -I"$hev/src" \
    -I"$hev/src/misc" \
    -I"$hev/src/core/include" \
    -I"$hev/src/core/src" \
    -I"$hev/third-part/lwip/src/include" \
    -I"$hev/third-part/lwip/src/ports/include" \
    -I"$hev/third-part/hev-task-system/include" \
    -I"$hev/third-part/hev-task-system/src/kern/core" \
    -I"$hev/third-part/hev-task-system/src/kern/task" \
    -I"$hev/third-part/hev-task-system/src/lib/object" \
    "$root/tests/hev_timeout_phases_test.c" \
    -o "$build/hev-timeout-phases-test"

"$build/hev-timeout-phases-test"

make -C "$hev" --no-print-directory static CC="${CC:-cc}" AR="${AR:-ar}" >/dev/null

cc \
    -std=c11 \
    -Wall \
    -Wextra \
    -Werror \
    -Wno-unused-parameter \
    -I"$hev/src" \
    -I"$hev/src/misc" \
    -I"$hev/src/core/src" \
    -I"$hev/third-part/lwip/src/include" \
    -I"$hev/third-part/lwip/src/ports/include" \
    -I"$hev/third-part/hev-task-system/include" \
    "$root/tests/hev_timeout_behavior_test.c" \
    "$hev/bin/libhev-socks5-tunnel.a" \
    "$hev/third-part/yaml/bin/libyaml.a" \
    "$hev/third-part/lwip/bin/liblwip.a" \
    "$hev/third-part/hev-task-system/bin/libhev-task-system.a" \
    -pthread \
    -o "$build/hev-timeout-behavior-test"

"$build/hev-timeout-behavior-test"

atomic_source="$hev/src/hev-socks5-tunnel.c"
grep -Fq 'static atomic_size_t stat_tx_packets;' "$atomic_source"
grep -Fq 'atomic_fetch_add_explicit (&stat_tx_bytes' "$atomic_source"
grep -Fq 'atomic_load_explicit (&stat_rx_bytes' "$atomic_source"
grep -Fq 'atomic_store_explicit (&stat_rx_packets' "$atomic_source"
