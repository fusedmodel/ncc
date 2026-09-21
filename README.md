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

# 5) Optional: your profile card and this machine as a device node
ncc profile set --headline "Turning vague needs into shipped AI systems" \
                --roles fde,agent-engineer --availability open
ncc profile                            # your card + short link
ncc living --name my-agent --kind agent --capabilities mcp,api   # register a node
ncc nodes                              # my nodes + linked nodes
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
| `ncc key list` / `create` / `revoke` / `scopes` | Manage capability tokens (kind, scopes, namespace limits, expiry) |
| `ncc living --name X --kind service\|agent\|assigned` | Register a node (= heartbeat report); the node declares what it is |
| `ncc profile [show <username>]` | View your profile card, or someone else's |
| `ncc profile roles` | List the work-role catalog |
| `ncc profile set` | Update profile fields (reads first; only overwrites what you pass) |
| `ncc profile username <name>` | Change your username |
| `ncc profile work list` / `add` / `rm` | Manage the portfolio |
| `ncc nodes` / `kinds` / `discover` | My nodes, the kind catalog, connectable nodes on this instance |
| `ncc nodes link` / `label` / `unlink` | Link a node and give it a name label |
| `ncc nodes region` / `recommend` | Region coverage and recommendations (agent-facing) |
| `ncc grant list` / `set` / `rm` | Per-person access grants (`artifact` \| `share`) |
| `ncc terminal [status\|setup]` | Open the capability console / inspect the POSIX runtime |
| `ncc update` | Check for a newer CLI or official package |
| `ncc mcp` | Start as an **MCP server** over stdio, so any agent can drive NCC |
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

### `ncc living` — registering a node

Declaring **is** registering: the report both creates/updates the node and keeps its lease alive.

| Option | Description |
|---|---|
| `--kind <service\|agent\|assigned>` | What this node is. Defaults to `service` |
| `--name <NAME>` | Node name. Defaults to `$HOSTNAME`, falling back to `<os>-<arch>` |
| `--slug <SLUG>` | Node slug; derived from the name when omitted |
| `--url <URL>` | Address others can reach this node at |
| `--capabilities <a,b>` | Kinds this node can serve, comma-separated |
| `--daemon` / `--interval <SEC>` | Keep heartbeating instead of reporting once (default interval `15`) |

`os`, `arch` and the CLI version are attached automatically. Only state and capability visibility are
published — NCC never relays data on your behalf. A `visibility=public` node is discoverable by
other users on the same NCC instance and can be linked with `ncc nodes link`.

### `ncc profile`

Your profile card: positioning roles, a portfolio, and the capabilities you have published. It is served at the root of the registry, so your username *is* your address:

```
ncc.ai/aya          → your card      ncc profile
ncc.ai/ns/@aya      → your artifacts ncc profile roles
ncc install @aya/x  → your artifacts
```

| Command | Description |
|---|---|
| `ncc profile [show [<username>]]` | Print a card (defaults to your own) |
| `ncc profile roles [--group <id>]` | List the 20 work roles in 6 groups — these are the values for `--roles` |
| `ncc profile set` | Update fields (see below) |
| `ncc profile username <name>` | Change your username |
| `ncc profile work list` | List portfolio entries with their ids |
| `ncc profile work add --title …` | Add an entry |
| `ncc profile work rm <id>` | Delete an entry |

`ncc profile set` options:

| Option | Description |
|---|---|
| `--username <NAME>` | Username: 3–30 lowercase letters, digits and hyphens; reserved words rejected |
| `--name <TEXT>` | Display name |
| `--headline <TEXT>` | One-line positioning |
| `--bio <TEXT>` | Longer description |
| `--location <TEXT>` | Location |
| `--roles <a,b>` | Positioning roles; up to 5, first one is primary |
| `--skills <a,b>` | Free-form skill tags; up to 12 |
| `--availability <open\|collab\|hiring\|busy>` | Are you open to work / collaboration? |
| `--visibility <public\|unlisted>` | `public` is listed in the people directory; `unlisted` is link-only |
| `--email <ADDR>` | Contact email (publicly visible) |
| `--link <key=value>` | Social link, repeatable — e.g. `--link github=https://github.com/you`. Pass `key=` to clear |

`ncc profile work add` accepts `--title`, `--summary`, `--role`, `--tags`, `--year`, and one of `--url` (external link), `--share <S-…>` (an NCC Share page) or `--item <R-…>` (a registry artifact) — so a portfolio entry can point straight at work you published on NCC.

> **`set` never wipes fields you did not mention.** The API replaces the whole card in one `PUT`, so the CLI reads the current values first and only overwrites what you passed. Changing your username also moves your personal namespace (`@old` → `@new`), which invalidates references written as `@old/…` — the CLI warns you when that happens.

### `ncc nodes`, `ncc grant`

NCC is **not an address book** — it exists so that different agent nodes can connect. Two
independent layers, and mixing them up is the classic mistake:

- a **link** means *reachable*;
- a **grant** means *authorised*.

```
# register a node — declaring IS registering (heartbeat renews the lease)
ncc living --name my-agent --kind agent --capabilities mcp,api
ncc living --name delivery-svc --kind service --url https://svc.internal
ncc living --name client-a-bot --kind assigned --slug client-a

ncc nodes kinds                   # the kind catalog
ncc nodes                         # my nodes + the nodes I have linked
ncc nodes discover                # connectable nodes on this NCC instance
ncc nodes link @aya/my-agent --label "delivery helper" --note "drafts for client requests"
ncc nodes label NL-xxxx --label "delivery helper v2"
ncc nodes unlink NL-xxxx

ncc grant set --user @someone --kind artifact --ns @you   # may download my private artifacts
ncc grant set --user @someone --kind share                # may view my private share pages
ncc grant list --in                                       # what others granted me
```

| Concept | Question it answers | Grants data access? |
|---|---|---|
| Node | What this agent/service is, where it is, whether it can be reached | ❌ Identity and address only |
| Link (`ncc nodes link`) | Which nodes my agent should connect to, under the name label I give them | ❌ Reachability only |
| Grants (`artifact` / `share`) | Who may download my private artifacts / view my private share pages | ✅ Yes, per kind |
| API keys | As what identity, with what powers, may a program act | ✅ Yes, by scope + namespace |

**Node kinds.** A node declares what it is at registration: `service` (API / MCP server / gateway /
data source), `agent` (serves a person) or `assigned` (assigned to a task, team or client — not its
owner's private agent). The declaration belongs to the node, not to whoever links to it.

**Linking needs no approval.** The same NCC instance is the same trust domain, so public nodes are
mutually linkable; a link is *your* table entry, where you give the node a **name label** plus a
purpose note so your agent knows whom to connect to and why. Private nodes never show up in
discovery — that belongs to grants. Unlinking removes only your entry.

**Region coverage and recommendations are agent-facing** (`ncc nodes region` /
`ncc nodes recommend`, MCP: `ncc_region_profile` / `ncc_recommend_nodes`). A node's region comes
from its owner's profile location. `recommend` is sorted server-side by how many nodes you already
have in that region, so CLI, MCP and the web share one ordering instead of each inventing its own.
Neither is shown on the website.

### `ncc key`

API keys are **capability tokens**: two independent constraints — `scopes` (what it may do) and
`namespaces` (whose artifacts it may pull).

```bash
# read-only credential to hand to a customer or an agent
ncc key create --label "client-A agent" --kind distribution --ns @you --expires 30

ncc key list      # kind, scopes, namespaces, expiry, last use
ncc key scopes    # the full scope vocabulary
ncc key revoke <id>
```

| Option | Description |
|---|---|
| `--label <TEXT>` | Human-readable label |
| `--kind <user\|distribution>` | `user` acts as you; `distribution` is read-only for handing out |
| `--scopes <a,b,…>` | Explicit scopes; omit for the defaults of that kind |
| `--ns <@slug>` | Limit to a namespace; repeatable. Omit for unrestricted |
| `--note <TEXT>` | Who is it for, and what for |
| `--expires <DAYS>` | Expiry in days; omit for a non-expiring token |

The scope vocabulary is `registry:read`, `registry:download`, `registry:publish`, `profile:read`,
`profile:write`, `contacts:read`, `contacts:write`, `grants:read`, `grants:write`, `living:write`
and `keys:write`, with implications so older tokens keep working:
`registry:publish ⇒ registry:download ⇒ registry:read`, `contacts:write ⇒ contacts:read`,
`grants:write ⇒ grants:read`, `profile:write ⇒ profile:read`.

Two properties worth relying on:

- **No privilege escalation** — `keys:write` is never granted to a key, so a leaked token cannot mint
  more tokens (`POST /api/auth/keys` with a key returns 403).
- **Expiry is enforced** — with `--expires 30` the token stops working at the expiry instant.

Hand a token to an agent by setting `NCC_TOKEN`, or write it into `~/.ncc/config.json`.
A distribution key limited to `registry:read` + `registry:download` can search and download inside
its namespace, and gets `403` for anything else — including publishing.

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
| `src/profile.rs` | Profile card, portfolio, role catalog |
| `src/social.rs` | Contacts, friend requests, grants, region profile |
| `src/mcp.rs` | MCP server over stdio (tool schemas + dispatch) |
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
agent/               Agent integration pack (MCP config, SKILL.md, harness manifest)
release/bin/         Checked-in prebuilt binaries + checksums.txt
scripts/             build-release.sh (cross-compile + checksums)
```

## Use it from an agent

`ncc mcp` runs NCC as an **MCP server** over stdio, so any MCP-capable agent can search the catalog,
fetch artifacts, publish results and look up people — no extra service to run:

```jsonc
{ "mcpServers": { "ncc": { "command": "ncc", "args": ["mcp"] } } }
```

| Tool | Purpose |
|---|---|
| `ncc_list_kinds` | What kinds exist and how many |
| `ncc_search_catalog` | Search by keyword / kind / tag / namespace |
| `ncc_get_artifact` | Full metadata for one artifact |
| `ncc_fetch_artifact` | Fetch the artifact body (a SKILL.md can go straight into context) |
| `ncc_publish_artifact` | Publish an artifact (needs credentials) |
| `ncc_whoami` | Current account and namespaces |
| `ncc_list_roles` | Work-role catalog |
| `ncc_find_people` | Find people by role / skill |
| `ncc_get_profile` | Someone's card: roles + portfolio + published capabilities |
| `ncc_list_contacts` | Your address book, with effective regions |
| `ncc_region_profile` | Where your network clusters, and in which roles |
| `ncc_recommend_contacts` | Candidates by region / role, same-region-first |
| `ncc_list_grants` | Grant relationships (outgoing / incoming) |
| `ncc_list_friend_requests` | Friend requests (pending by default) |

The people-related tools are **read-only on purpose**. Anything that changes what another party
can obtain — adding contacts, granting access, accepting requests — stays in the CLI, where the
user performs it deliberately.

Search, fetch and the people directory need **no login**; only publishing does. `ncc mcp` writes only
protocol messages to stdout and all logs to stderr — required by MCP's stdio transport.

[`agent/`](agent) holds the distributable integration pack: the MCP setup, a `SKILL.md` for agents
without MCP, and a `kind=harness` manifest (`mcp/stdio` loader) so any runtime can load it.

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
