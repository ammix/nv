# nv

Minimal Neovim version manager for the official Linux x86_64 stable and nightly
builds. Each channel retains one previous release for rollback.

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
just uninstall
```

Requires `curl`, `jq`, `sha256sum`, and `tar`. The selected executable is linked
at `~/.local/bin/nvim`.

## Usage

```text
nv install stable|nightly
nv use stable|nightly
nv update stable|nightly|all
nv rollback stable|nightly
nv status
```

- `install` installs or updates a channel without changing the selected channel.
- `use` installs or updates a channel, then selects it.
- `update` updates installed channels only.
- `rollback` swaps a channel's current and previous releases. Running it again
  swaps forward.

Updates and rollbacks to the selected channel take effect immediately.

## State

State lives under `~/.local/share/nv`. Installations are immutable; channel and
active pointers are atomically replaced symlinks.

Archives are selected through the GitHub Releases API, size-checked, and verified
against the asset's GitHub-provided SHA-256 digest before extraction. Publication
occurs only after the staged `nvim --version` succeeds.

Mutating operations hold `~/.local/share/nv/operation.lock`. An existing marker
is treated as active or stale state and must be resolved manually. After a stale
marker is removed, the next command completes any interrupted pointer transaction.
The state and executable directories must not be writable by group or others.
