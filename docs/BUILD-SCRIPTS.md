# Build & Install Scripts (mur)

The `mur` project has two build scripts for building the Rust binary with the embedded web dashboard.

---

## `build.sh` — Build with Embedded Web Dashboard

Builds `mur-web` (the web dashboard) first, then compiles the `mur` binary with the dashboard embedded via `MUR_WEB_DIST`.

### Usage

```bash
./build.sh [OPTIONS]
```

### Options

| Flag        | Description                                              |
|-------------|----------------------------------------------------------|
| `--release` | Build in release mode *(default if no flag is given)*    |
| `--install` | Copy the built binary to `/opt/homebrew/bin/mur`         |

### Environment Variables

| Variable      | Default                   | Description                      |
|---------------|---------------------------|----------------------------------|
| `MUR_WEB_DIR` | `~/Projects/mur-web`      | Path to the `mur-web` project   |

### What It Does

1. Runs `npm run build` in the `mur-web` directory
2. Sets `MUR_WEB_DIST` to `<mur-web>/dist` and runs `cargo build --release`
3. Prints binary path and size
4. If `--install`: copies the binary to `/opt/homebrew/bin/mur`

### Example

```bash
# Build release (default)
./build.sh

# Build and install
./build.sh --release --install

# Use custom mur-web location
MUR_WEB_DIR=/path/to/mur-web ./build.sh
```

---

## `install.sh` — Shortcut for Release + Install

A one-liner convenience script that runs:

```bash
./build.sh --release --install
```

### Usage

```bash
./install.sh
```

This builds the release binary with the embedded web dashboard and copies it to `/opt/homebrew/bin/mur`.
