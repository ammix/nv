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

    if [[ ( -e "$nvim_link" || -L "$nvim_link" ) && "$(readlink -- "$nvim_link")" != "../share/nv/active/bin/nvim" ]]; then
        printf 'refusing to remove unmanaged executable: %s\n' "$nvim_link" >&2
        exit 1
    fi

    cargo uninstall nv
    cargo clean
    gio trash --force -- "$state" "$nvim_link"

validate: fmt-check test clippy
