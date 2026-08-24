set shell := ["bash", "-euo", "pipefail", "-c"]

alias b := build
alias c := clean
alias f := fmt
alias i := install
alias t := test
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

validate: fmt-check test clippy
