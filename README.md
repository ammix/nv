# nv

Minimal Neovim version manager for the official Linux x86_64 stable and nightly
builds. Each channel retains one previous release for rollback.

nv is a single static Go binary. Downloading, JSON parsing, checksum
verification, and archive extraction use only the Go standard library, so it
needs no external modules or command-line tools.

## Install

```sh
go install github.com/ammix/nv@latest
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
just uninstall
```

The selected executable is linked at `~/.local/bin/nvim`. nv refuses to replace
an existing `~/.local/bin/nvim` it did not create.

## Usage

```text
nv install stable|nightly
nv use stable|nightly
nv update [stable|nightly]
nv remove [stable|nightly]
nv rollback stable|nightly
nv status
```

- `install` installs or updates a channel without changing the selected channel.
- `use` installs or updates a channel, then selects it.
- `update` updates all installed channels by default, or one selected channel.
- `remove` removes all installed channels by default, or one selected channel.
  Removing the active channel also removes nv's managed executable link.
- `rollback` swaps a channel's current and previous releases. Running it again
  swaps forward.
