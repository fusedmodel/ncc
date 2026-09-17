# NCC CLI

**The official command-line client for [NCC Registry](https://ncc.ai)** — a neutral, cross-protocol registry for *capability artifacts*: APIs, Skills (SKILL.md), MCP servers, Harnesses, Plugins, Scaffolds, Docker images and benchmarks.

One Rust binary (`ncc`, no runtime dependencies) that lets you register, publish, search, install and download capabilities, mint API keys for CI, attach devices to your namespace, and open the NCC Terminal capability console.

- **Publish once, distribute everywhere.** An artifact gets a stable reference `@your-namespace/slug` that any agent, hub or teammate can resolve — NCC does not lock you into a vendor.
- **Machine-friendly by design.** Everything the CLI does goes through the public HTTP API (`/api/…`), so scripts and CI can talk to the registry directly.
- **Single static binary.** `ncc` is a small Rust binary for macOS / Linux / Windows (x86_64 & arm64).

> **Status: 0.1.0 (early).** The registry is in invite-gated closed beta; commands and data shapes may still change. CLI output is currently Chinese — an English/localized output layer is on the roadmap.

## Features

- **Accounts & sessions** — `register` / `login` / `logout` / `me`, session persisted in `~/.ncc/config.json`.
- **Publishing** — upload a file (`--file`) or point at your own URL (`--url`, BYO storage), with `kind`, version, summary, tags, manifest, namespace, visibility (`public` / `private`) and `draft` state.
- **Discovery** — full-text `search` with `--kind` / `--tag` / `--namespace` / `--mine` filters, plus `info` for a single artifact.
- **Distribution** — `download` to a file, or `install` into the local package directory (`~/.ncc/packages`).
- **Namespaces** — `ncc ns list` / `ncc ns create --slug …` (org namespaces are part of the paid tier).
- **API keys** — `ncc key create --label ci` for non-interactive publishing from CI.
- **NCC Living** — `ncc living` reports this machine as a device node in your namespace (`--daemon` keeps heartbeating), so others can discover what you can serve.
- **NCC Terminal** — `ncc terminal` opens the capability console (official package `@ncc/terminal`), including a POSIX runtime check (`terminal status` / `terminal setup`).

## Install

### 1. Install script (recommended)

```bash
curl -fsSL https://ncc.ai/install.sh | sh
# → installs to ~/.ncc/bin/ncc
```

The script picks the binary for your OS/arch. Self-hosted registries serve it from their own `/downloads`; any release mirror works through `NCC_RELEASE_BASE`.

### 2. npm wrapper

```bash
npm install -g @ncc/cli     # or: npx @ncc/cli --help
```

`@ncc/cli` is a thin launcher: it locates the platform binary in the order below and forwards args / stdio / signals to it.

1. `NCC_BIN` (explicit path)
2. `vendor/ncc-<os>-<arch>` shipped inside the package
3. `~/.ncc/bin/ncc`
4. `cli/target/release/ncc` (in-repo build, for development)
5. otherwise it downloads a release binary into `~/.ncc/bin/ncc`

> The wrapper lives in [`packages/ncc-cli`](packages/ncc-cli) and is **not published to npm yet** — publish with `npm publish --access public` from that directory.

### 3. Build from source

```bash
git clone https://github.com/fusedmodel/ncc.git
cd ncc/cli
cargo install --path .          # → ~/.cargo/bin/ncc
# or a plain build:
cargo build --release           # → cli/target/release/ncc
```

Requires a Rust toolchain (edition 2021; tested with 1.98). No system TLS library is needed — `ureq` uses rustls, so there is no OpenSSL dependency.

### Verify a downloaded binary

Prebuilt binaries and their SHA-256 sums are checked in under [`release/bin`](release/bin):

```bash
cd release/bin && shasum -a 256 -c checksums.txt      # macOS
cd release/bin && sha256sum -c checksums.txt          # Linux
```

## Quick start

```bash
# 1) Get an account (an invite code is required while the registry is in closed beta)
ncc register --email you@example.com --password 'a-strong-password' --name You --invite NCC-2026-INVITE
ncc me

# 2) Publish a Skill from a local SKILL.md
ncc publish --file ./hotel.SKILL.md --kind skill --name "Hotel Skill" \
            --slug hotel-skill --tags hotel,travel --summary "Booking helper"

# 3) Find and consume capabilities
ncc search skill --tag hotel
ncc info     @you/hotel-skill
ncc download @you/hotel-skill -o hotel.md
ncc install  @you/hotel-skill                 # → ~/.ncc/packages/@you/hotel-skill/

# 4) Automate publishing from CI
ncc key create --label ci                     # printed once — keep it secret

# 5) Join the live fabric (optional)
ncc living --name my-mac --capabilities mcp,api
ncc terminal                                  # capability console
```

Point the CLI at any registry instance with the global `--base` flag (it is remembered in the config file):

```bash
ncc --base http://localhost:8181 me           # self-hosted instance
```

## Command reference

| Command | What it does | Notable options |
|---|---|---|
| `ncc register` | Create an account (auto-creates your personal namespace) | `--email` `--password` `--name` `--invite` |
| `ncc login` / `ncc logout` | Start / end a session | `--email` `--password` |
| `ncc me` | Show the signed-in user and namespaces | |
| `ncc ns list` / `ncc ns create` | List namespaces / create an org namespace | `--slug` |
| `ncc publish` | Publish an artifact (file upload or BYO URL) | `--kind` `--name` `--slug` `--version` `--summary` `--tags` `--file` `--url` `--manifest` `--namespace` `--visibility` `--draft` |
| `ncc search [query]` | Search the catalog | `--kind` `--tag` `--namespace` `--mine` |
| `ncc info <target>` | Artifact detail | |
| `ncc download <target>` | Download artifact bytes | `-o, --out` |
| `ncc install <target>` | Install into the local package dir | `-d, --dir` `--force` |
| `ncc key list` / `create` / `revoke` | Manage API keys | `--label` |
| `ncc living` | Report this device / keep heartbeating | `--daemon` `--interval` `--name` `--slug` `--url` `--capabilities` |
| `ncc terminal [status\|setup]` | Capability console / POSIX runtime helpers | |
| `ncc update` | Check for newer CLI / `@ncc/terminal` versions | |

`<target>` accepts either a registry id (`R-…`) or a reference (`@namespace/slug`).

Global flags: `--base <URL>` (registry base URL, persisted), `-h/--help`, `-V/--version`.

## Configuration

| Where | What |
|---|---|
| `~/.ncc/config.json` | Registry base URL + session token (plain file). Override the location with `NCC_CONFIG`. |
| `~/.ncc/bin/ncc` | Binary installed by the install script / npm launcher. |
| `~/.ncc/packages/` | Default install root for `ncc install`. |
| `NCC_BIN` | Force a specific binary path (used by the npm launcher). |
| `NCC_INVITE_CODE` | Invite code used by `ncc register` when `--invite` is omitted. |
| `NCC_RELEASE_BASE` | Base URL for downloading release binaries (default: this repo's GitHub Releases). |
| `NCC_UPDATE_URL` | Base URL used by `ncc update` to look up the latest release. |

## Repository layout

```
cli/                 Rust CLI crate (bin: ncc)
packages/ncc-cli/    npm wrapper (@ncc/cli) — launcher + binary downloader
release/bin/         Prebuilt binaries + checksums.txt
scripts/             build-release.sh (cross-compile + checksums)
```

## Development & release

```bash
# build / run locally
cd cli && cargo build --release && ./target/release/ncc --help

# cross-compile release binaries for every platform + regenerate checksums
bash scripts/build-release.sh            # current platform
bash scripts/build-release.sh --all      # all targets (needs `rustup target add …`)
```

`build-release.sh` writes `release/bin/ncc-<os>-<arch>[.exe]` plus `checksums.txt`. To cut a release, attach those files to a GitHub Release tag (the npm wrapper and `install.sh` download from there), and bump the version in `cli/Cargo.toml` and `packages/ncc-cli/package.json`.

## Related

- Product & registry: <https://ncc.ai> · CLI page: <https://ncc.ai/cli>
- Issues / ideas: <https://github.com/fusedmodel/ncc/issues>

## License

Apache License 2.0 — see [LICENSE](LICENSE).

---

## 中文说明

**NCC CLI 是 [NCC Registry](https://ncc.ai)（中立、跨协议的能力制品注册中心）官方命令行客户端。** 一个静态 Rust 二进制，覆盖注册 / 发布 / 检索 / 安装 / 下载，以及 API-Key、设备接入与 NCC Terminal 能力命令台。

**为什么用它**

- **一次发布，处处可分发**：制品以 `@命名空间/slug` 稳定引用，任何 Agent、Hub、同事都能解析，不绑定任何平台。
- **机器友好**：CLI 全部走公开 HTTP API（`/api/…`），脚本与 CI 可直接对接。
- **单一二进制**：macOS / Linux / Windows（x86_64 与 arm64），无需系统 TLS 库（rustls）。

**安装**

```bash
curl -fsSL https://ncc.ai/install.sh | sh     # → ~/.ncc/bin/ncc（推荐）
npm install -g @ncc/cli                       # npm 包装（自动定位 / 下载平台二进制）
cd cli && cargo install --path .              # 从源码安装 → ~/.cargo/bin/ncc
```

> `@ncc/cli` 目前**尚未发布到 npm**（源码在 `packages/ncc-cli`，在该目录执行 `npm publish --access public` 即可发布）。
> 二进制校验：`cd release/bin && shasum -a 256 -c checksums.txt`。

**快速开始**

```bash
ncc register --email you@example.com --password '…' --name You --invite NCC-2026-INVITE  # 闭测期需邀请码
ncc publish --file ./hotel.SKILL.md --kind skill --name "Hotel Skill" --slug hotel-skill
ncc search skill --tag hotel
ncc install @you/hotel-skill                  # → ~/.ncc/packages
ncc key create --label ci                     # CI 用 API-Key（仅显示一次）
ncc living --name my-mac --capabilities mcp,api
ncc terminal                                  # 能力命令台
ncc --base http://localhost:8181 me           # 指向自托管实例
```

**命令一览**：`register` `login` `logout` `me` `ns list|create` `publish` `search` `info` `download` `install` `key list|create|revoke` `living` `terminal [status|setup]` `update`；全局参数 `--base <URL>`。
制品引用支持注册中心 id（`R-…`）与 `@命名空间/slug` 两种写法。

**配置**：`~/.ncc/config.json`（服务地址 + 登录态，可用 `NCC_CONFIG` 改位置）、`~/.ncc/bin/ncc`、`~/.ncc/packages`；
环境变量 `NCC_BIN`、`NCC_INVITE_CODE`、`NCC_RELEASE_BASE`、`NCC_UPDATE_URL`。

**仓库结构**：`cli/`（Rust 客户端）、`packages/ncc-cli/`（npm 包装）、`release/bin/`（预编译二进制 + SHA-256）、`scripts/build-release.sh`（交叉编译与校验和）。

**当前状态**：0.1.0 早期版本；注册中心处于邀请制闭测，命令与数据结构可能调整；CLI 输出现为中文，英文 / 多语言输出在计划中。

**许可**：Apache-2.0（见 [LICENSE](LICENSE)）。

