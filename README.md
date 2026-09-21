# NCC CLI

[中文说明](README.zh-CN.md) · [Registry](https://ncc.ai) · [Issues](https://github.com/fusedmodel/ncc/issues)

![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue)
![Status: alpha](https://img.shields.io/badge/status-alpha-orange)
![Rust](https://img.shields.io/badge/rust-1.98%2B-orange)

The official command-line client for **NCC Registry** — a neutral, cross-protocol registry for *capability artifacts*: APIs, Skills (`SKILL.md`), MCP servers, Harnesses (incl. HUR), Plugins, Scaffolds, Docker images, benchmarks and live nodes.

`ncc` is a single Rust binary. No Node or Python runtime, no system OpenSSL. It covers the whole artifact lifecycle — register, publish, search, install, download — plus API keys for CI, device presence reporting, and the NCC Terminal console.

```bash
ncc publish --file ./hotel.SKILL.md --kind skill --name "Hotel Skill" --slug hotel-skill
ncc search skill --tag hotel
ncc install @you/hotel-skill
```

## Why this exists

- **Publish once, resolve from anywhere.** Every artifact gets a stable reference `@namespace/slug` that any agent, hub, CI job or teammate can resolve. NCC does not lock you into a vendor or an agent framework.
- **Open formats, not a proprietary blob.** Artifacts are plain files (for example `SKILL.md`) plus a manifest contract and a `sha256` digest — inspectable, diffable, mirrorable.
- **Machine-friendly by design.** Everything the CLI does goes through the public HTTP API (`/api/…`), so scripts, CI pipelines and other clients can talk to a registry directly without shelling out to `ncc`.
- **Self-hostable.** Point the client at any registry instance with `--base`.

## Status

> **Alpha (`0.1.0`) — nothing is distributed yet.** The registry is in invite-gated closed beta, and commands or data shapes may still change.
>
> - **Building from source is currently the only install path that works end to end.** `@fusedmodel/ncc-cli` is not published to npm and no GitHub Release has been cut, so both the install script and the npm launcher have no download source to reach yet.
> - **There is no public hosted registry.** `ncc.ai` is not live, and the client's built-in default points at a *local* instance (`http://localhost:8181`). Always pass `--base` explicitly for now.
> - **Human-facing CLI output is currently Chinese.** The programmatic surface (exit codes, stderr errors, JSON from `ncc info`) is stable and language-neutral; an English output layer is on the roadmap.

## Install

### Build from source (recommended today)

```bash
git clone https://github.com/fusedmodel/ncc.git
cd ncc/cli
cargo install --path .        # → ~/.cargo/bin/ncc
```

Or just build without installing:

```bash
cargo build --release         # → cli/target/release/ncc
```

Requires a Rust toolchain (edition 2021; tested with rustc 1.98). TLS is provided by `rustls`, so no OpenSSL dev package is needed.

### Install script (available once a registry is reachable)

A registry instance serves its own installer at `/install.sh`:

```bash
curl -fsSL https://<your-registry>/install.sh | sh   # → ~/.ncc/bin/ncc
```

The script picks the binary for your OS/arch and honours `NCC_RELEASE_BASE` for the download source. This path is implemented but **not usable against a public host yet** — see [Status](#status).

### npm wrapper (source ready, not yet published)

The wrapper lives in [`packages/ncc-cli`](packages/ncc-cli). Its source is complete and checked in, but the package has not been published to npm yet:

```bash
# once published:
npm install -g @fusedmodel/ncc-cli     # or: npx @fusedmodel/ncc-cli --help

# to publish it yourself from a checkout:
cd packages/ncc-cli && npm publish --access public
```

The wrapper is a thin launcher: it resolves a binary and forwards args, stdio and signals. Resolution order:

1. `NCC_BIN` (explicit path)
2. `vendor/ncc-<os>-<arch>` shipped inside the package
3. `~/.ncc/bin/ncc`
4. `cli/target/release/ncc` — in-repo build, for development
5. otherwise it downloads a release binary into `~/.ncc/bin/ncc`

A best-effort `postinstall` does the same download and never blocks installation on failure.

> The unscoped name `ncc` on npm belongs to an unrelated package, so the wrapper is published under the `@fusedmodel` scope as `@fusedmodel/ncc-cli`.

### Verify a prebuilt binary

Prebuilt binaries and their SHA-256 sums are checked in under [`release/bin`](release/bin):

```bash
cd release/bin && shasum -a 256 -c checksums.txt      # macOS
cd release/bin && sha256sum -c checksums.txt          # Linux
```

## Quick start

Because no public registry is live yet, run these against a local or self-hosted instance:

```bash
# 0) Point the client at an instance (persisted in the config file)
ncc --base http://localhost:8181 me

# 1) Create an account (auto-creates your personal namespace)
#    An invite code is required while the registry is in closed beta.
ncc register --email you@example.com --password 'a-strong-password' \
             --name You --invite NCC-2026-INVITE
ncc me

# 2) Publish a Skill from a local SKILL.md
ncc publish --file ./hotel.SKILL.md --kind skill --name "Hotel Skill" \
            --slug hotel-skill --tags hotel,travel --summary "Booking helper"

# 3) Find and consume capabilities
ncc search skill --tag hotel
ncc info     @you/hotel-skill
ncc download @you/hotel-skill -o hotel.md
ncc install  @you/hotel-skill          # → ~/.ncc/packages/@you/hotel-skill/

# 4) Automate publishing from CI
ncc key create --label ci              # secret is printed once — store it securely

# 5) Optional: presence and console
ncc living --name my-mac --capabilities mcp,api
ncc terminal
```

## Command reference

`<target>` is either a registry id (`R-…`) or a reference (`@namespace/slug`).

| Command | Description |
|---|---|
| `ncc register` | Create an account; auto-creates your personal namespace |
| `ncc login` / `ncc logout` | Start / end a session |
| `ncc me` | Show the signed-in user, plan and namespaces |
| `ncc ns list` | List the namespaces you own or belong to |
| `ncc ns create` | Create a namespace |
| `ncc publish` | Publish an artifact from a file upload or a BYO URL |
| `ncc search [query]` | Search the catalog |
| `ncc info <target>` | Print an artifact's full record as JSON |
| `ncc download <target>` | Download the artifact bytes |
| `ncc install <target>` | Install into the local package directory |
| `ncc key list` / `create` / `revoke` | Manage API keys for non-interactive use |
| `ncc living` | Report this machine as a device node in your namespace |
| `ncc terminal [status\|setup]` | Open the capability console / inspect the POSIX runtime |
| `ncc update` | Check for a newer CLI or official package |
| `ncc help <command>` | Show generated help for any command |

Global flags:

| Flag | Description |
|---|---|
| `--base <URL>` | Registry base URL. Overrides the config file and is written back to it. |
| `-h, --help` / `-V, --version` | Help / version |

### `ncc publish`

| Option | Description |
|---|---|
| `--kind <KIND>` | **Required.** `api`, `harness`, `hur`, `skill`, `mcp`, `plugin`, `scaffold`, `docker-image`, `benchmark`, `living` |
| `--name <NAME>` | **Required.** Human-readable display name |
| `--file <PATH>` | Upload bytes from a local file |
| `--url <URL>` | Bring your own storage: publish a direct link instead of uploading |
| `--slug <SLUG>` | URL-safe slug; the registry derives one from `--name` when omitted |
| `--version <VER>` | Defaults to `1.0.0` |
| `--summary <TEXT>` | One-line description |
| `--tags <a,b,c>` | Comma-separated tags |
| `--manifest <PATH>` | JSON wrapper contract for `--kind harness` (carries `harness.loader` / `harness.entry`) |
| `--namespace <SLUG>` | Target namespace; must be one you belong to. Defaults to your personal namespace |
| `--visibility <public\|private>` | Defaults to `public`. `private` requires a paid plan |
| `--draft` | Create the artifact in `draft` state instead of `published` |

Exactly one of `--file` or `--url` is required.

### `ncc search`

| Option | Description |
|---|---|
| `[query]` | Free-text keyword (optional positional) |
| `--kind <KIND>` | Filter by artifact kind |
| `--tag <TAG>` | Filter by tag |
| `--namespace <SLUG>` | Restrict to one namespace |
| `--mine` | Only your own artifacts (requires a session) |

### `ncc install` and `ncc download`

| Option | Description |
|---|---|
| `-d, --dir <DIR>` | Install root. Defaults to `~/.ncc/packages` (or `NCC_PACKAGES_DIR`) |
| `--force` | Overwrite an existing installation |
| `-o, --out <PATH>` | `download`: destination path |

`ncc install` lays artifacts out as `<root>/<namespace>/<slug>/` and writes a `package.json` alongside the artifact file recording the source reference, kind, version, `sha256`, size, install time — and the wrapper `manifest` / `harness` block when the artifact declares one.

### `ncc living`

| Option | Description |
|---|---|
| `--daemon` | Keep heartbeating on an interval instead of reporting once |
| `--interval <SEC>` | Daemon interval. Defaults to `15` |
| `--name <NAME>` | Device name. Defaults to `$HOSTNAME`, falling back to `<os>-<arch>` |
| `--slug <SLUG>` | Device slug; derived from the name when omitted |
| `--url <URL>` | Address others can reach this device at |
| `--capabilities <a,b>` | Kinds this device can serve, comma-separated |

`os`, `arch` and the CLI version are attached automatically. Only state and capability visibility are published — NCC never relays data on your behalf.

### `ncc terminal`

`ncc terminal` opens the capability console (official package `@ncc/terminal`). On a real TTY it renders a full-screen TUI with tab completion, history (`↑`/`↓`), output pane and `Ctrl+C` to exit; when stdin is not a TTY (pipes, CI) it falls back to a line-based REPL.

Inside the console:

| Input | Effect |
|---|---|
| `help` | Built-in help |
| `runtime status` / `runtime setup` | POSIX runtime state / assembly |
| `ncc <cmd…>` | Run a preset `ncc` subcommand (`publish`, `search`, `install`, `living`, …) |
| `! <cmd>` or any other line | Hand the line to the system POSIX shell |
| `exit` / `quit` | Leave |

`ncc terminal status` prints the resolved base URL, OS and POSIX runtime without entering the console. On Unix the runtime is native; on Windows the CLI detects WSL2 and falls back to MSYS2, guiding you through `ncc terminal setup` when neither is present.

## Core concepts

| Term | Meaning |
|---|---|
| **Artifact** | A versioned, publishable unit of capability — a Skill, MCP server, API description, Harness, … |
| **Kind** | The artifact's category (`skill`, `mcp`, `harness`, …). Determines how consumers interpret it |
| **Namespace** | A publishing scope, either personal (`@you`) or organizational (`@your-org`). Namespaces are addressable as `@slug` |
| **Reference** | `@namespace/slug` — the stable, portable way to name an artifact |
| **Visibility** | `public` is resolvable by anyone; `private` requires a paid plan |
| **Status** | `published` (resolvable), `draft` (only visible to you) or `archived` |
| **Manifest** | An optional JSON contract attached to an artifact. For `kind harness` it carries `harness.loader` / `harness.entry` |

## Configuration

| Path | Purpose |
|---|---|
| `~/.ncc/config.json` | Registry base URL plus your session token, email and name. Created on first login |
| `~/.ncc/bin/ncc` | Binary installed by the install script or npm launcher |
| `~/.ncc/packages/` | Default root for `ncc install` |

`--base` is the only flag the CLI persists: passing it rewrites `base_url` in the config file, and subsequent invocations use it without the flag.

### Environment variables

| Variable | Used by | Effect |
|---|---|---|
| `NCC_CONFIG` | CLI | Config file location. Defaults to `~/.ncc/config.json` |
| `NCC_PACKAGES_DIR` | CLI | Install root for `ncc install`. Defaults to `~/.ncc/packages` |
| `NCC_INVITE_CODE` | CLI | Invite code for `ncc register` when `--invite` is omitted |
| `NCC_BIN` | npm wrapper | Force a specific binary path (checked first) |
| `NCC_RELEASE_BASE` | install script, npm wrapper, `ncc update` | Base URL for downloading release binaries. Defaults to this repo's GitHub Releases |
| `NCC_UPDATE_URL` | `ncc update` | Endpoint used for the latest-release lookup. Defaults to the GitHub releases API |

`HOME`, `HOSTNAME` and `SHELL` are read for defaults (config location, device name, POSIX summary) and can be overridden as usual.

> **The config file is plain text and holds a bearer token.** It is written without hardening the file mode — `chmod 600 ~/.ncc/config.json` if your machine is shared. Prefer `NCC_PACKAGES_DIR` / `NCC_CONFIG` plus a short-lived config in CI over committing a credentials file.

## Using your own registry

Any NCC-compatible instance works as a backend:

```bash
ncc --base https://registry.internal.example me
```

A self-hosted instance also serves its own client distribution, so its users can install a binary that already knows the right base URL:

- `GET /install.sh` — installer script, rebased onto the serving host
- `GET /downloads/<file>` — release binaries

To distribute your own builds through either path, set `NCC_RELEASE_BASE` to the mirror you control.

## Scripting and CI

The CLI is designed to be driven by other programs:

- **Exit codes** — `0` on success, `1` on any failure.
- **Errors** — written to stderr as `✗ [error_code] message`, where the code and message come straight from the registry's JSON error body (`{"error":{"code":…,"message":…}}`). Network failures are reported separately as `网络错误: …`.
- **Structured data** — `ncc info <target>` prints the artifact record as pretty JSON on stdout, suitable for `jq`.
- **Non-interactive auth** — mint an API key once (`ncc key create --label ci`, printed exactly once) and use it in place of a login session.
- **Human-readable text** — `search`, `publish`, `install` and friends print human-readable Chinese output; use the HTTP API directly if you need machine-stable output. Note that `ncc terminal` detects a non-TTY and degrades to a REPL rather than failing.

Client-side network behaviour: the API client uses a 10 s connect timeout, a 60 s overall timeout and follows up to 10 redirects. Artifact bytes are fetched with a 60 s budget and buffered in memory before being written, so `download` / `install` are not yet suited to very large artifacts.

## Development

```bash
cd cli

cargo check                   # fast type check
cargo build --release         # → target/release/ncc
cargo fmt && cargo clippy     # if you have the rustup components
```

To exercise the CLI against a running registry without installing it:

```bash
NCC_CONFIG=/tmp/ncc-dev.json ./target/release/ncc --base http://localhost:8181 me
```

There is no automated test suite yet — a smoke test that drives the CLI against a live registry is the most valuable contribution here.

Source layout:

| File | Responsibility |
|---|---|
| `src/main.rs` | Argument parsing (clap) and every command implementation |
| `src/api.rs` | Thin HTTP client over `ureq`: JSON requests, raw uploads, error decoding |
| `src/config.rs` | `~/.ncc/config.json` load/save and session handling |
| `src/terminal.rs` | Command console, POSIX runtime detection, update check |
| `src/tui.rs` | Full-screen ratatui TUI (used when stdin is a real TTY) |

Design constraints worth preserving: keep the dependency list small; route every operation through the public HTTP API rather than inventing a private protocol; and never let the client become a data broker between machines.

## Releasing

```bash
bash scripts/build-release.sh          # current platform → release/bin/ncc-<os>-<arch>
bash scripts/build-release.sh --all    # cross-compile every target (needs `rustup target add …`)
```

The script writes `release/bin/ncc-<os>-<arch>[.exe]` plus a regenerated `checksums.txt`.

To cut a release:

1. Bump the version in `cli/Cargo.toml` (and refresh `cli/Cargo.lock`) and `packages/ncc-cli/package.json`.
2. Run `scripts/build-release.sh --all`.
3. Tag and publish a GitHub Release with those binaries attached — this is what the install script, the npm wrapper and `ncc update` all resolve against.
4. `npm publish --access public` from `packages/ncc-cli`.
5. Update the [Status](#status) section: once a release exists, the install script and npm paths above become live.

Prebuilt targets: `darwin` (x86_64, arm64), `linux` (x86_64, arm64), `windows` (x86_64). Only `darwin-arm64` is checked into `release/bin` today; the rest are produced by `--all`.

## Repository layout

```
cli/                 Rust crate (bin: ncc)
packages/ncc-cli/    npm wrapper (@fusedmodel/ncc-cli) — launcher + binary downloader
release/bin/         Checked-in prebuilt binaries + checksums.txt
scripts/             build-release.sh (cross-compile + checksums)
```

## Contributing

Contributions are welcome — bug reports, documentation fixes and platform support especially.

- Open an issue before starting on anything large, so the approach can be agreed on first.
- Keep pull requests focused; match the existing style and prefer the standard library plus the current dependencies over new crates.
- Note that the CLI is a *client* of the registry API. Changes to the wire format need a corresponding server-side change, so describe that in the issue.
- CLI output strings are currently Chinese and not yet centralized for translation. If you want to work on localization, open an issue first — it is a known gap, not an oversight.

## Security

Please do not open a public issue for a vulnerability. Report it privately through [GitHub Security Advisories](https://github.com/fusedmodel/ncc/security/advisories/new) for this repository, including reproduction steps and affected versions.

Remember that `~/.ncc/config.json` stores a bearer token in plain text, and that `ncc key create` prints an API key exactly once — treat both as secrets.

## License

Apache License 2.0 — see [LICENSE](LICENSE).
