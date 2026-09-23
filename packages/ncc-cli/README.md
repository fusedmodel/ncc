# @fusedmodel/ncc-cli

> npm wrapper for the NCC command line. It locates the platform Rust binary —
> or downloads it once — and hands over stdin/stdout/signals unchanged.

```bash
npx @fusedmodel/ncc-cli --help
```

Or install it globally:

```bash
npm i -g @fusedmodel/ncc-cli
ncc search --kind skill
```

The installed command is **`ncc`**.

## What it does

NCC is a registry for capability artifacts (API / Skill / MCP / Harness / Plugin / Scaffold).
The CLI talks to the registry at [ncc.ai](https://ncc.ai) or to your own deployment.

| Command | Purpose |
| --- | --- |
| `ncc register` · `ncc login` · `ncc logout` · `ncc me` | account and session |
| `ncc ns list` · `ncc ns create --slug …` | namespaces |
| `ncc publish` | publish an artifact (`--kind skill --name X --file ./x.SKILL.md`) |
| `ncc search [query] [--kind api]` | browse the catalog |
| `ncc info <id \| @org/slug>` | artifact details |
| `ncc download` · `ncc install` | fetch bytes, or install into `~/.ncc/packages` |
| `ncc key create\|list\|revoke` | API keys |
| `ncc living` | device heartbeat (Living); `--daemon` to report periodically |
| `ncc p2p probe` · `p2p check <node>` · `p2p ticket …` | hole-punch preflight (local) / two-sided connectivity check / P2P tickets |
| `ncc terminal` · `ncc upgrade` | terminal console, in-place self-upgrade |

## Where the binary comes from

This package **ships a launcher, not the binary.** `bin/ncc.js` resolves in this order and
never silently switches source:

1. `NCC_BIN` (explicit path)
2. `vendor/ncc-<os>-<arch>` shipped inside the package
3. `~/.ncc/bin/ncc` (previously installed)
4. `cli/target/release/ncc` — the single-repo development case

If none exist it downloads once from `NCC_RELEASE_BASE`
(default: this repo's GitHub Releases) into `~/.ncc/bin/ncc`. `postinstall` attempts the same
download, but **fails soft** — a broken or offline install still leaves you with a working
launcher that retries on first run.

Asset names follow `ncc-<os>-<arch>[.exe]`, with `os ∈ {darwin, linux, windows}` and
`arch ∈ {x86_64, arm64}`. That naming is a contract shared with `.github/workflows/release.yml`
and `scripts/build-release.sh` — change one, change all three.

## Just want the source

```bash
git clone https://github.com/fusedmodel/ncc.git && cd ncc/cli
cargo build --release
./target/release/ncc --help
```

## License

Apache-2.0.
