set shell := ["bash", "-euo", "pipefail", "-c"]

alias b := build
alias c := clean
alias f := fmt
alias i := install
alias t := test
alias u := uninstall
alias v := validate

[default]
default:
    @just --list

build:
    cargo build --release --locked

check:
    cargo check --locked

clippy:
    cargo clippy --locked --all-targets --all-features -- -D warnings

clean:
    cargo clean

fmt:
    cargo fmt

fmt-check:
    cargo fmt --check

install:
    cargo install --locked --path .

run *args:
    cargo run --locked -- {{ args }}

test:
    cargo test --locked

uninstall:
    #!/usr/bin/env bash
    set -euo pipefail

    state="${HOME:?HOME is not set}/.local/share/nv"
    nvim_link="$HOME/.local/bin/nvim"
    lock="$state/operation.lock"
    expected_link="../share/nv/active/bin/nvim"

    if [[ -e "$lock" || -L "$lock" ]]; then
        printf 'refusing to uninstall while operation lock exists: %s\n' "$lock" >&2
        exit 1
    fi

    if [[ -L "$state" ]]; then
        printf 'refusing to remove symlinked state directory: %s\n' "$state" >&2
        exit 1
    fi

    if [[ -e "$nvim_link" || -L "$nvim_link" ]]; then
        if [[ ! -L "$nvim_link" ]]; then
            printf 'refusing to remove unmanaged executable: %s\n' "$nvim_link" >&2
            exit 1
        fi

        actual_link="$(readlink -- "$nvim_link")"
        if [[ "$actual_link" != "$expected_link" ]]; then
            printf 'refusing to remove unmanaged symlink: %s -> %s\n' "$nvim_link" "$actual_link" >&2
            exit 1
        fi
    fi

    cargo uninstall nv
    cargo clean

    targets=()
    if [[ -e "$state" ]]; then
        targets+=("$state")
    fi
    if [[ -e "$nvim_link" || -L "$nvim_link" ]]; then
        targets+=("$nvim_link")
    fi

    if (( ${#targets[@]} > 0 )); then
        gio trash -- "${targets[@]}"
    fi

validate: fmt-check test clippy check
