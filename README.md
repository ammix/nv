# nv

Minimal Neovim version manager for the official Linux x86_64 stable and nightly
builds. Each channel retains one previous release for rollback.

nv delegates downloading, JSON parsing, checksum verification, and archive
extraction to standard Linux command-line tools instead of extra Rust crates or
native implementations.

## Dependencies

- `curl`
- `jq`
- `sha256sum`
- `tar`

## Install

```sh
cargo install --locked --git https://github.com/ammix/nv.git
```

From a checkout:

```sh
just install
```

Repository workflows:

```sh
just build
just test
just validate
just clean
```

The selected executable is linked at `~/.local/bin/nvim`.

## Usage

```text
nv install stable|nightly
nv use stable|nightly
nv rollback stable|nightly
nv status
```

- `install` installs or updates a channel without changing the selected channel.
- `use` installs or updates a channel, then selects it.
- `rollback` swaps a channel's current and previous releases. Running it again
  swaps forward.
