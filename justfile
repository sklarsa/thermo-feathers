# `just` lists these. Firmware recipes load secrets through scripts/env.sh: export them, or set
# THERMO_SECRETS to a sops file (see there).

set shell := ["bash", "-euo", "pipefail", "-c"]

qwp := env("QWP_EMBEDDED", "../qwp-embedded")
dummy := "WIFI_SSID=ci WIFI_PASSWORD=ci QUESTDB_ADDR=127.0.0.1:9000 THERMO_MASTER=" + sha256("")

default:
    @just --list

# Host tests for src/lib.rs, against a qwp-embedded checkout (QWP_EMBEDDED, default ../qwp-embedded)
test:
    #!/usr/bin/env bash
    set -euo pipefail
    t=$(mktemp -d); trap 'rm -rf "$t"' EXIT
    rustc +stable --edition 2024 --crate-type rlib --crate-name qwp_embedded {{qwp}}/src/lib.rs -o "$t/libqwp_embedded.rlib"
    rustc +stable --edition 2024 --test src/lib.rs --extern qwp_embedded="$t/libqwp_embedded.rlib" -o "$t/thermo-tests"
    "$t/thermo-tests"

# What CI runs: fmt, clippy (with dummy secrets), host tests
check: test
    cargo fmt --all -- --check
    [ ! -f ~/export-esp.sh ] || . ~/export-esp.sh; env {{dummy}} cargo clippy --release -- -D warnings

# Build node firmware and flash the next board on USB (press its RESET)
flash-node *args:
    . scripts/env.sh && scripts/flash-node.sh {{args}}

# Build and flash the gateway over USB, then watch its log
gateway:
    . scripts/env.sh && : "${QUESTDB_ADDR:?export QUESTDB_ADDR}" && cargo run --release --bin gateway

# Update over the air: `just ota` (all nodes), `just ota 610338,4a4d9c`, `just ota gateway`
ota to="all":
    . scripts/env.sh && : "${THERMO_GATEWAY:?export THERMO_GATEWAY, the gateway address}" && scripts/ota.sh "$THERMO_GATEWAY" {{to}}

# Rebuild every STL in enclosure/stl from the current model
stl:
    #!/usr/bin/env bash
    set -euo pipefail
    cd enclosure
    for f in stl/*.stl; do
        n=$(basename "$f" .stl)
        case $n in
            core)       d=(-D 'part="core"') ;;
            core_*)     d=(-D 'part="core"' -D "id=\"${n#core_}\"") ;;
            shell_*_c*) s=${n#shell_}; d=(-D 'part="shell"' -D "shell=\"${s%_c*}\"" -D "colour=${s##*_c}") ;;
            *)          d=(-D "part=\"$n\"") ;;
        esac
        openscad -q "${d[@]}" -o "$f" main.scad && echo "built enclosure/$f" &
    done
    wait

# A core with a new node's device ID cut into the back
core id:
    cd enclosure && openscad -q -D 'part="core"' -D 'id="{{id}}"' -o stl/core_{{id}}.stl main.scad && echo "built enclosure/stl/core_{{id}}.stl"

# Rebuild bootloader/bootloader.bin (Docker)
bootloader:
    bootloader/build.sh
