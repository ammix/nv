# nv

Minimal Neovim version manager for Linux stable and nightly builds. Each channel retains one previous release for rollback.

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

- `install` installs a channel if it is missing, without changing the selected
  channel.
- `use` installs a channel if it is missing, then selects it. It never updates.
- `update` updates all installed channels by default, or one selected channel.
  It is the only command that replaces an installed release.
- `remove` removes all installed channels by default, or one selected channel.
  Removing the active channel also removes nv's managed executable link.
- `rollback` swaps a channel's current and previous releases. Running it again
  swaps forward.
