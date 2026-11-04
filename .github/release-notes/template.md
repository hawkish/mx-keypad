## Install

Releases are source only: build mx-keypad with Nix or Cargo.

### Nix flake

```nix
# Pin to a release tag (recommended); drop the ref to track the default branch.
inputs.mx-keypad.url = "github:hawkish/mx-keypad/{{TAG}}";
```

Or install it into your profile:

```sh
nix profile install github:hawkish/mx-keypad/{{TAG}}
```

### Cargo

```sh
cargo install --git https://github.com/hawkish/mx-keypad --tag {{TAG}}
```

On macOS, run `mx-keypad install` afterward to restart the LaunchAgent on the new build. It signs the agent with your Apple Development identity when you have one, so macOS keeps its Accessibility permission across upgrades. See the [README](https://github.com/hawkish/mx-keypad#install-and-check-a-configuration).
