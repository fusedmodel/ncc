# NCC Registry

> [中文说明](README.zh-CN.md) · [Registry](https://ncc.ai) · [Issues](https://github.com/fusedmodel/ncc/issues) · [Changelog](CHANGELOG.md)

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

#### `ncc: command not found` right after installing

The script does two things, each with a precondition, and a miss looks like "installed but unrunnable":

1. Appends `~/.ncc/bin` to any rc file that **already exists** (`~/.zshrc` / `~/.bashrc` / `~/.profile`), creating `~/.zshrc` when none does.
   This only affects **new** terminals — the script runs in a subshell and cannot change your current shell.
2. If a directory already on PATH is writable (e.g. `~/.local/bin`), it symlinks `ncc` there so the **current** terminal works immediately.

Stopgap: `export PATH="$PATH:$HOME/.ncc/bin"`. The decision to write an rc file is based on the **rc file's contents**, never on the current `$PATH` — an inherited PATH makes "already set up" a lie.

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

> With a **global** install, whether `ncc` is typable depends on the **npm global bin directory** being on your PATH — the binary goes to `~/.ncc/bin`, the shim goes to the npm bin dir, and those are two different places. When the npm prefix is `/usr/local/lib/npm` (a common default), that bin dir is *not* on PATH and you get `command not found`. Check with `npx @fusedmodel/ncc-cli --version` (PATH-independent), then `export PATH="$PATH:$(npm prefix -g)/bin"`.

### Verify a prebuilt binary

Prebuilt binaries and their SHA-256 sums are checked in under [`release/bin`](release/bin):

```bash
cd release/bin && shasum -a 256 -c checksums.txt      # macOS
cd release/bin && sha256sum -c checksums.txt          # Linux
```

## Quick start

Because no public registry is live yet, run these against a local or self-hosted instance:

```bash
# 0) Point the client at an instance: an existing target with that URL is reused,
#    otherwise a new target is created and selected (your old target is never silently rewritten)
ncc --base http://localhost:8181 me

# 1) Create an account (auto-creates your personal namespace)
#    An invite code is required while the registry is in closed beta.
#    The first account of an instance also becomes its admin and gets a
#    one-time admin key/secret printed (see `ncc registry admin`).
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
ncc p2p probe                                                   # local hole-punch preflight (no server)
ncc p2p check @you/mac                                          # real connectivity check (run on both sides)
ncc nodes                              # my nodes + linked nodes
ncc agent share ./my-agent             # hand my agent to a specific person (point-to-point link)
ncc agent add 'https://ncc.ai/a/AC-…'  # accept someone's agent: install the package + link the node
ncc sandbox init --host 10.0.0.5 --port 8282 --key <key>   # cloud computer: register a machine that runs work
ncc sandbox run --on office --cmd "docker build -t me/app . && docker push me/app" --reason "release"
ncc conn open --on office --name deploy --ttl 7200         # connection channel: open a session on that machine
ncc conn open --ssh deploy@10.0.0.7:2222 --name web        # SSH works too: the target only needs sshd (no server change)
ncc conn run deploy --file ./app.tar.gz --script ./deploy.sh --reason "release: v0.3.0"
ncc conn exec deploy "tar -xzf app.tar.gz && ./app --check" --reason "verify"
ncc conn pull deploy logs/app.log --to ./app.log            # pull artifacts; close --purge to hang up
ncc auth pkg init ./team-auth --id @me/team-auth --project web --passphrase-file ./pass.txt
ncc auth pkg run ./team-auth --project web --reason "release" --passphrase-file ./pass.txt -- ./deploy.sh
# ↑ auth package: only metadata is plaintext; unlock lasts exactly one command, then it re-locks and wipes
ncc rsi init --preset unattended-safe    # a decision gate before the action: project-level .ncc-rsi/
ncc rsi goal set --statement "ship v0.3.0" --accept "deploy,release" --reject "refactor,rewrite"
ncc rsi check --command "git push --force"          # -> blocked (exit code 20); --json for machines
ncc rsi guard --reason "run tests" -- npm test      # check first; when blocked it really does not run
ncc rsi hook install --host claude                 # wire it into a host's PreToolUse hook
ncc rsi report --since 24h                         # the local ledger tally (no upload channel)
ncc feedback send --about 'artifact:@me/tool' --kind report --body 'run reports ENOENT' --as-agent claude-code
ncc feedback inbox                                 # what others said about my items / services / profile
ncc feedback relay --all-mine                      # carry **public** node feedback up to the hub (private never leaves)
ncc rsi learn consent set --source feedback:mine --source 'mem:@me' --source log:ledger
ncc rsi learn consent on                           # off by default: nothing is read until you say so
ncc rsi learn plan                                 # read once -> **proposals** (nothing applied); apply by name
ncc terminal
```

Connecting to an intranet node:

```bash
ncc target list                        # who am I talking to, and what does each side declare?
ncc target add office --base http://10.0.0.5:8282 --use
ncc --base http://127.0.0.1:8282 login --email you@corp.com --password '***'
ncc --target hub me                    # one-shot: use the cloud for this command
ncc hub publish --file ./x.SKILL.md --kind skill --name X --slug x
```

## Targets: the cloud ncc.ai vs an intranet node

`ncc` is one client, but ncc has **two worlds**:

| World | Default target name | What it is | What you do on it |
|---|---|---|---|
| **Cloud** | `hub` | the public registry at ncc.ai | services marketplace, profiles, share pages, billing, ops console |
| **Intranet node** | your pick (`local` / `office` …) | self-hosted `ncc-registry` (single binary) | artifacts, nodes, config hosting, share links, node governance, cluster |

Both are called “registry”, and some endpoint names even collide (`/api/nodes`,
`/api/grants`, `/api/admin` …) with **different semantics** — so the CLI makes
“who am I talking to” explicit with **targets**:

```bash
ncc target list                  # current target, addresses, sessions, declared capabilities
ncc target use office            # switch (each target keeps its own credentials)
ncc target show                  # details of the current target
ncc target add lab --base http://10.0.0.9:8282
ncc target rm lab
ncc hub                          # quick look at the cloud target
```

Three ways to point somewhere for **one command only** (the default target stays put):

| Form | Meaning |
|---|---|
| `ncc --target <name> <cmd>` | run this command against that target |
| `ncc hub <cmd>` | run it against the cloud (same as `--target hub`) |
| `ncc --base <URL>` | use that URL: reuse an existing target, else create one and switch (it tells you) |

### Capabilities: a command runs if the target declares it

Every node declares what it supports in `GET /api/meta` (`registry` / `services` / `profile` /
`config` / `nodes` / `grants` / `share` / `access` / `cluster` / `admin` / `living` …). The CLI
routes by that list:

```bash
ncc target use office && ncc services match "book a hotel in Hangzhou"
#   ✗ 目标 office（ncc-registry · 内网节点）没有声明 `services` 能力
#     它声明的能力：registry · config · share · nodes · grants · access · cluster · admin
#     `services` 目前由云端（ncc.ai）提供。切过去：ncc target use hub
```

This is deliberate: **no hard-coded “cloud-only” / “local-only” split**, just “what did this
node declare”. When the intranet node grows `services` / `profile`, the very same command
works on it with no client change. Older servers without `/api/meta` are treated as
“unknown → unrestricted” so nothing gets locked out.

## Command reference

`<target>` is either a registry id (`R-…`) or a reference (`@namespace/slug`).

| Command | Description |
|---|---|
| `ncc target list` / `use` / `add` / `rm` / `show` | Targets: which ncc instances am I connected to (cloud / intranet nodes) |
| `ncc hub <cmd>` | Run a command against the cloud target (one-shot) |
| `ncc register` | Create an account; auto-creates your personal namespace |
| `ncc login` / `ncc logout` | Start / end a session |
| `ncc me` | Show the signed-in user, plan and namespaces |
| `ncc ns list` | List the namespaces you own or belong to (organizations show a **plan** column) |
| `ncc ns plans` | List the **organization plan** catalog (unavailable tiers are shown but cannot be picked) |
| `ncc ns create --slug <slug> --name <name> [--plan free]` | Create an org namespace (`plan` defaults to `free`) |
| `ncc publish` | Publish an artifact from a file upload or a BYO URL |
| `ncc search [query]` | Search the catalog |
| `ncc index publish <channel>` | Index an offering / artifact / need (platform first, then intranet nodes; failed nodes are reported one by one) |
| `ncc index list` / `show` / `rm` / `push` | What I registered / details + pushed nodes / withdraw (original untouched) / push again |
| `ncc list users` / `needs` / `channels` | Who is in the index / what people need / which search spaces exist |
| `ncc match "…"` | My need → who can do it (`--want need` finds demand; `--from <target>` matches against an intranet node) |
| `ncc info <target>` | Print an artifact's full record as JSON |
| `ncc download <target>` | Download the artifact bytes |
| `ncc install <target>` | Install into the local package directory |
| `ncc sign <file>` | **Sign the bytes you publish** (any kind: skill / mcp / …). Minisign/ed25519; the key never leaves the machine; `--attach <ref>` is the only networked step (uploads the `.minisig` + public key, never the artifact bytes) |
| `ncc verify <file \| @ns/slug>` | Verify a signature: local file runs entirely offline; a reference downloads the bytes, re-checks the digest, then verifies. `--require-signature` for CI; a **tampered** artifact always exits non-zero |
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
| `ncc services` / `catalog` / `match` / `show` | Services offered: catalog / match by intent / full details of one |
| `ncc services add` / `rm` | Declare or take down your own services (provider side) |
| `ncc grant list` / `set` / `rm` | Per-person access grants (`artifact` \| `service` \| `share` \| `p2p`) |
| `ncc auth key new` / `ls` / `rm` | Bind a **proof-of-possession credential** (`CR-…`): Ed25519 key under `~/.ncc/cred/` (0600), only the public key is registered. Deliberately **separate** from the publishing key in `~/.harnessuse/keys`. Binding a credential ≠ being allowed to use anything |
| `ncc auth login --client <id>` | **Device flow** (RFC 8628, like `gh auth login`): the terminal prints `verification_uri` + `user_code`, you approve in a browser, the CLI polls. The token is stored per target and stays on the data plane (account commands still use `ncc login`) |
| `ncc auth consents` / `revoke` / `status` | What I granted to which platform (scopes + bound credential) / revoke **per platform** (immediate, and it never touches other platforms) |
| `ncc auth pkg init <dir> --id @ns/slug --project web --passphrase-file <0600 file> [--identity] [--allow <fpr>…]` | **Auth package** (`profile=auth`): seal a set of credentials into a signable HUR package. **Only metadata is plaintext**; values live in `auth/vault.enc` (ChaCha20-Poly1305) and the ciphertext is pinned by the manifest |
| `ncc auth pkg show <dir>` / `get <dir> --project p --entry N [--reveal]` / `set … --value-stdin --reason "…"` / `rm …` | Read metadata (**no passphrase**, no values) / read one entry (masked by default) / write one / delete one. Values only come from `--value-stdin` / `--value-file` (argv is visible in ps) |
| `ncc auth pkg run <dir> --project p --reason "…" -- <cmd>` | **Unlock → inject → run → wipe → lock**: values arrive as `NCC_AUTH_<entry>` and `NCC_AUTH_BUNDLE` (0600), wiped on exit; the exit code follows the child |
| `ncc auth pkg rotate <dir> [--project p \| --new-passphrase-file <new>] --reason "…"` | **Rotation**: change a project key (gen+1 and re-encrypt) or the master key. Old material stops working; every update is lock-protected |
| `ncc auth pkg lock <dir> [--force]` / `status <dir>` | Inspect the lock / clear leftover sessions (`--force` clears the lock too) / local state and the audit tail (**entry names only, never values**) |
| `ncc --auth <cmd>` | Run that command as an **outward access token** (data plane only); when the token is key-bound the CLI attaches the `NCC-Proof` holder proof automatically |
| `ncc p2p probe` | Hole-punch preflight — local NAT profile, no server needed |
| `ncc p2p check <node>` | Real connectivity check against a peer node (both sides run it; ≈ ICE connectivity check) |
| `ncc p2p ticket create` / `list` / `verify` / `revoke` | P2P tickets: which node may pull which resource from you |
| `ncc registry p2p self` / `check --peer <ip:port>` | NAT profile **on the node machine** / real UDP punch against a peer mapping (0 bytes) |
| `ncc registry p2p serve` `[--on] [--peer <ip:port>]` | Node's punchable UDP entry (STUN replies only); `--peer` sets the reverse-punch peer |
| `ncc gateway init` / `check` / `run` / `status` / `audit` | Gateway (S2a): a fixed-route allow-listed proxy — provide egress (`accept`) or borrow a peer's (`forward`). Callers can never choose the target; outbound credentials come from config `inject`; local JSONL audit records metadata only |
| `ncc gateway bind` / `heartbeat` | Attach to the **control plane** (= ncc.ai): register the gateway → token written to `~/.ncc/gateway.json` (0600, shown once) / periodic heartbeat (default meaning is `online`; `draining` may be self-reported) |
| `ncc gateway report` / `usage` | Aggregate local audit into **window summaries** and report them signed (**persist before sending**: unreachable control plane ⇒ queue, backfill when it returns) / usage rollup (explicitly **self-reported counters**) |
| `ncc gateway audit --remote` / `unbind` | View/export summaries retained by the control plane (`--csv` = compliance export) / deregister and clear the local binding (works even when the control plane is unreachable) |
| `ncc registry add` | Join a self-hosted `ncc-registry` with a one-click intranet link, or key/secret |
| `ncc registry login` / `join` | Sign in to that node, or host this machine as a node (register + heartbeat) |
| `ncc registry status` / `nodes` | That node and its cluster (master/worker) / discover nodes on the instance |
| `ncc registry catalog` / `route` | Aggregated directory across the cluster / which node can serve an artifact |
| `ncc registry ticket create` / `list` / `rm` | Issue and manage join tickets (key + secret + intranet link) |
| `ncc registry config` / `kinds` / `get` / `set` | Config hosting: catalog / fetch (masked by default) / write (new revision) |
| `ncc registry config` `history` / `rollback` / `bundle` | Version history / rollback / per-environment bundle |
| `ncc registry share create` / `list` / `rm` / `info` | Share: turn an artifact into an **expiring download link** (no login required) |
| `ncc registry admin overview` / `users` / `nodes` / `services` / `audit` | Node governance: users / nodes / services / audit (admin account or admin key/secret) |
| `ncc registry admin disable` / `enable` / `passwd` / `rm-node` / `rm-service` | Disable/enable accounts / reset passwords / remove nodes / archive service entries |
| `ncc registry admin login` / `status` / `rotate` | Store machine credentials / check your admin status / rotate the admin key+secret |
| `ncc registry replicate` | Push a copy of an artifact to workers (`--to all` or names) |
| `ncc registry rm` | Take an artifact down and reclaim every replica (`--yes`) |
| `ncc registry leave` | Take my node offline (the next heartbeat re-registers it) |
| `ncc trace add --file <f.jsonl>` | Collect run traces — native `ncc-trace/v1` documents, HUR run traces, or an event stream (`{type,name,ms,in,out}` per line). **Local only**, no network |
| `ncc trace ls` / `show` / `stats` / `export` / `label` / `rm` | Inspect, aggregate, export (JSONL dataset) or label traces locally; add `--remote` to do it against the target node |
| `ncc trace push` | Upload collected traces to the node (idempotent, 50 per batch). Requires the target to declare the `trace` capability |
| `ncc trace kinds` / `status` | Vocabularies and limits / local spool state (how many collected, how many uploaded) |
| `ncc kb set <slug> --title … [--file f]` | Write a **hosted knowledge base** document (exists → new revision). `--public` makes it anonymously readable |
| `ncc kb ls` / `get` / `search` / `history` / `bundle` | List (public ∪ mine ∪ granted) / read one, `--revision N` for a historical version / **keyword-weighted** search / revision history / group fetch a whole namespace |
| `ncc kb archive` / `restore` / `rm` | Archive (hidden from listings and search) / restore / delete |
| `ncc kb pull --package <dir>` | **Read a package's `state.kb` declaration and pull those corpora** into `~/.ncc/kb` (incremental by `checksum`). Refuses to guess when the package declares nothing |
| `ncc kb kinds` | Knowledge-base kinds / formats / limits (works offline) |
| `ncc mem set <key> <value>` | Write **memory** (upsert on `(namespace, subject, key)`, `--ttl-days`, `--kind`, `--source`, `--confidence`, `--pin`) |
| `ncc mem get <key>` / `ls` / `rm` / `gc` | Read one by key (the path an agent takes) / list (expired excluded) / delete / really remove expired entries |
| `ncc mem kinds` | Memory kinds and limits (memory has **no public tier**) |
| `ncc ckpt save --name … --file …` | Take a **checkpoint**: computes `sha256`, creates the metadata, uploads the bytes (`--parent-last` links lineage, `--meta k=v` adds free metadata) |
| `ncc ckpt ls` / `show` / `lineage` | List / inspect incl. a short-lived signed `bytesUrl` / walk the `parent` chain back to the start |
| `ncc ckpt pull <id> --out <file>` | Fetch the bytes, **verifying the digest before writing to disk** |
| `ncc ckpt prune --ref <@ns/slug> --keep N` / `rm` | Keep the newest N (rest marked `pruned`, bytes deleted, metadata kept) / delete one outright |
| `ncc ckpt kinds` | Checkpoint labels and limits (immutable — there is no "edit" action) |
| `ncc store declare <collection>` | **Declare a collection — this is the entire cost of a new kind of content** (issues, run logs, retros, notes…). The server does not change: `--field 'title:string!'` / `'status:enum:open\|closed'` / `'labels:string[]'` / `'body:text?search'` / `'owner:ref'`, `--index` for the filterable ones, `--immutable` / `--append-only`, `--public`, `--max-bytes`, `--ttl-days` |
| `ncc store declare --file <f>` / `--dir <d>` | Declarations are **configuration**: apply them from a file (single object, array, `{"stores":[…]}` or a whole `hur.json`), or from a directory of `*.json` (one per collection). Declaring states the whole truth — anything absent from the file is absent |
| `ncc store declare … --check` | Show **what would change** (which keys) without changing anything; exits 1 when there is drift, so it works as a CI gate |
| `ncc store export --dir <d>` / `--file <f>` | Write the declarations out (one file per collection + a README) so they can live in git, be reviewed, and be re-applied. This is the **declaration**, not the records — it is not a backup |
| `ncc store ls` | Which collections this node has (records, shape, visibility, declared fields) |
| `ncc store put <collection> <key>` | Write one record: `--body` / `--file` / stdin, `--field k=v` (typed and validated **locally against the declaration**), `--meta k=v` (free JSON, **not filterable**), `--tag`, `--ttl-days`, `--revision N` (optimistic concurrency — a mismatch is a 409, never a silent overwrite), `--note` (recorded in history) |
| `ncc store list <collection>` | Query: `--where field=value` (must be a declared **and indexed** field, otherwise it fails with the list of filterable fields rather than returning nothing), `--q` (keyword matching: every word must appear; hits are weighted key 3 / tags and `?search` fields 2 / body 1 — **not** indexed retrieval, and ranking runs over at most 500 candidates), `--tag`, `--prefix`, `--archived`, `--expired`, paging |
| `ncc store get <collection> <key>` | Fetch one record with its body |
| `ncc store history <collection> <key>` | Revision history — metadata only (who, when, digest, note). Notes travel with **their own** revision, so the note you gave at creation is never lost |
| `ncc store rm <collection> <key>` | Archive (not listed, still there). `--hard` really deletes |
| `ncc store kinds` | Type whitelist / caps / the three invariants — **works offline** |
| `ncc terminal [status\|setup]` | Open the capability console / inspect the POSIX runtime |
| `ncc upgrade` | Upgrade the CLI binary in place (`--check` only reports, `--force` reinstalls) |
| `ncc mcp` | Start as an **MCP server** over stdio, so any agent can drive NCC. `--package <dir\|hur.json>` narrows the face to the collections that package declares (**the model face = the declared face**: read tools follow `mode`, a write tool exists only if the package declares a write, and a collection that is not declared is not even visible — nor callable) |
| `ncc app init` / `doctor` / `up` / `status` / `export` | **NCC pod**: make "deploy your own personal assistant" one command — the product is yours (`app.json`), the engine is ncc, the app logic is a HUR package (`hur.json`), content lives on your node, interconnection on the platform; `up` runs a loopback console, `export` produces a hand-off directory |
| `ncc rsi init` / `policy` / `check` | **A decision gate before the action**: project-level `.ncc-rsi/` (policy / goal / preferences / ledger); `check` takes a decision request (`--file` or stdin, and it accepts host shapes like `tool_name` / `tool_input.command`) and returns a verdict — **exit code 0 allow / 10 warn / 20 block / 1 config error** (kept separate from "blocked"); `--unattended` **only upgrades warn to block** |
| `ncc rsi guard -- <cmd>` | **Guarded execution**: check first, and when blocked it **really does not run** (the smoke asserts the marker file is absent); otherwise the child's exit code is returned and a non-zero one is booked as an incident; `--explain` checks without running |
| `ncc rsi goal set` / `status` / `done` | **No drift**: both `--accept a,b` and `--accept a --accept b` work; a `--reject` hit is drift, no `--accept` hit is reported as **"cannot tell how this relates"** (it neither hides that nor counts it as a pass) |
| `ncc rsi pref add` / `ls` / `rm` / `suggest` | **User preferences**: `kind=avoid` takes part in the verdict (upgraded to block when unattended), `prefer` is display-only; `ls` shows **hit counts**; `suggest` surfaces repeated reasons from the ledger for a human to pick — it does **not** auto-write preferences |
| `ncc rsi report --since 24h` / `--json` | The tally after unattended work, from the **local** ledger (**there is no upload channel**): decisions / blocks / incidents / drifts / preference hits |
| `ncc feedback send` / `ls` / `get` / `reply` / `status` | **Cross-agent, cross-user feedback**: four identities kept apart (author / agent / about / owner, with **owner resolved by the server**); **append-only** (content cannot be edited — only the **disposition**, and only by the target owner); **private by default** (`--public` to open it up); `--about` forms: `service:@alice/stay` / `artifact:@alice/tool` / `@alice` (profile) / a bare sentence (topic) |
| `ncc feedback inbox` / `summary` / `spool` | Inbox (what others said about my things) / summary (**not a score**: never feeds ranking, matching or trust) / the local spool (**spool first, send second**: on failure the command exits non-zero and the entry stays queued) |
| `ncc feedback relay --about \| --all-mine` / `--flush` | Carry **public** feedback from one target to another (hub by default): private ones are **never** carried (and the server rejects them again), the relayer is the author of record (the original author is quoted as `originAuthor`), idempotent by `(origin, originId)`; `--flush` first drains the local spool |
| `ncc rsi learn consent` / `plan` / `apply` / `digest` / `export` | **Learn from feedback and state**: nothing is learned by default; `consent set --source feedback:mine|mem:@me|kb:@ns/doc|ckpt:@ns/app|log:ledger` declares sources (**declaration is permission**), `consent on` switches it on; `plan` only produces **proposals** (pref / guard / lesson, each with a why and its evidence), `apply` is by name, and **policy is never written automatically**; `export --dir` emits a dataset (`manifest.json` + `items.jsonl` with sources, consent and redaction) |
| `ncc rsi hook install --host claude\|cursor\|generic` | Writes a 30-line sh shim for a host's `PreToolUse` hook: JSON on stdin, exit code `20` (`NCC_RSI_BLOCK_CODE`, default 2, when unattended) means blocked; **⚠️ it is `--host`, not `--target`** (the top-level `--target` is the global target selector and would swallow it first) |
| `ncc hur profile <path \| @ns/slug>` | Read what a package **is**: what it wants, what it gives, **how to hook it up** — plus a graded check (`structure / self-consistent / signature` reported separately, never smeared into one ✅). `--list` prints the whole profile table |
| `ncc hur match --profile kb-seed` | Read-only search: find packages by profile / integration host / capability |
| `ncc hur data import --package <dir>` | Pour a **data snapshot package** into the node (kb-seed / mem-seed / ckpt-set / trace-set). Prints a plan by default; `--apply` actually writes |
| `ncc kb bundle --as-package <dir>` | Export a knowledge base as an immutable **snapshot package** (source / snapshotAt / privacy / license), signable & publishable |
| `ncc mem export --as-package <dir>` | Memory snapshot (private by default — a memory that can be handed out is no longer a memory) |
| `ncc ckpt export --as-package <dir>` | Checkpoint set: bytes + lineage, digest verified before it enters the package |
| `ncc trace export --as-package <dir>` | Trace dataset snapshot; `--payload digest\|preview\|full` must be declared (`full` + `public` is refused) |
| `ncc help <command>` | Show generated help for any command |

Global flags:

| Flag | Description |
|---|---|
| `--base <URL>` | Use that URL for this command: reuse an existing target or create one and switch |
| `--target <name>` | Use that target for this command (the default target is unchanged; to change it: `ncc target use <name>`) |
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

### `ncc sign` and `ncc verify` — signing any artifact

A signature is only worth anything if the person checking it does not have to trust you. So
`ncc sign` signs **the exact bytes you publish**, and anyone holding the public key can check
them with stock tooling — no NCC, no `ncc-cli`:

```sh
ncc hur key gen                                  # once: key lives in ~/.harnessuse/keys
ncc sign SKILL.md --kind skill --reference @me/release-notes --version 0.1.0
#   ⇢ writes SKILL.md.minisig (offline; the private key never leaves the machine)

ncc verify SKILL.md                              # ✔ verified
ncc verify SKILL.md --require-signature          # CI gate (non-zero when there is no verifiable signature)
minisign -V -p ~/.harnessuse/keys/hur.pub -m SKILL.md   # the third-party check
```

The signature object carries `format`, `keynum`, `signer`, `sha256` (the artifact digest),
the `.minisig` text and a public-key hint. Put it in the publish manifest and the registry
re-checks that the digest matches the bytes you just uploaded:

```sh
ncc sign SKILL.md --kind skill --reference @me/release-notes --version 0.1.0 --json \
  | python3 -c 'import json,sys; json.dump({"signature": json.load(sys.stdin)["signature"]}, open("manifest.json","w"))'
ncc publish --file SKILL.md --kind skill --name release-notes --manifest manifest.json
```

Signing after the fact is `--attach` (the only networked path — it uploads the `.minisig` and
the public key, never the artifact bytes):

```sh
ncc sign SKILL.md --kind skill --attach @me/release-notes
```

Three things this deliberately does **not** do:

* **A package is not one file.** Point `ncc sign` at a HUR package directory (or a `.hur` /
  `.hur.gz` artifact) and it hands over to `ncc hur sign`, which signs the canonical packed bytes
  together with the package identity, version and `hur.lock`. Two shapes, one implementation each.
* **An unknown key is not "verified".** `ncc verify` reports `⚠️ has a signature, but this
  machine does not know the key` and exits non-zero for `--require-signature`.
* **A bundled public key is not trust.** The `pubkey` captured next to a signature is the
  publisher's own claim. `ncc verify` will tell you the signature is *self-consistent* with it,
  nothing more — accepting it means `--pubkey <the key you confirmed yourself>` or
  `ncc hur key trust`.

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
ncc grant set --user @someone --kind service              # may call my non-public services
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

### `ncc services`

A provider (a company, or a chain group) can declare **each of its businesses as its own
service**, open to other agents. Other agents match on the provider's policy to learn *who to
call, how to connect and whether a grant is needed*.

```bash
ncc services catalog                      # categories (6 groups / 28), protocols, access modes
ncc services                              # browse the public catalog (or --mine)
ncc services match "book me a hotel in Hangzhou"   # server-side scoring + reasons + steps
ncc services show @aya/hotel-booking      # full connection details for one service
ncc services match --json "…"             # raw JSON (score / reasons / howToUse)

# provider side
ncc services add --name "Hotel booking hub" --category booking --slug hotel-booking \
  --summary "East-China room availability" --tag booking --intent "book hotel" \
  --match "Call me for East-China hotel bookings" --region "Hangzhou · Shanghai" \
  --protocol openapi --endpoint https://api.example.com/openapi/hotel.json \
  --access open --publish
ncc services rm SV-xxxx
```

| Concept | Question it answers | Grants data access? |
|---|---|---|
| Service | Who to call for this business, how to connect, is a grant needed | ❌ declaration; details follow the grant |
| Node | Where it runs (a service may bind one node) | ❌ address only |
| Grant | Who may see the connection details of non-public services | ✅ `--kind service` |

Worth knowing:

- New services are `draft` (visible to you only) until published; up to 30 per profile.
- `--protocol`: `http` / `openapi` / `mcp` / `artifact` (install the package first) / `human`.
- `--access`: `open` / `grant` (details need a grant) / `invite` (not listed at all).
- **Non-public services expose a summary only**: without a grant, results carry name, category,
  region and policy — endpoint, node, package and steps stay hidden.
- **Any region** means `countrywide` or empty: it counts as covering every region query.

### `ncc index` / `ncc list` / `ncc match`

A service declaration answers "what can I do"; the **index** answers "how do people find me" —
register something into a **channel** (a free name like `booking/hotel`) so one sentence of need
can retrieve it. The same service can be indexed into different channels with different wording.

```bash
# Publish: platform first (authoritative), then best-effort push to joined intranet nodes
ncc index publish booking/hotel --service @aya/hotel-booking
ncc index publish pptx/deck --item @aya/html-deck-to-pptx --intent "HTML to PPTX"
ncc index publish booking/hotel --need "two king rooms in Hangzhou for Oct 1"   # needs are indexed too

ncc index list --mine              # what I registered
ncc index list --channel booking   # what is in this channel (prefix: booking → booking/hotel)
ncc index show @aya/aya-hotel      # details: how to connect + which nodes were pushed
ncc index push @aya/aya-hotel      # push to intranet nodes again
ncc index rm @aya/aya-hotel        # withdraw the index (**the original is untouched**)

ncc list users                     # who is in the index (supply / needs / channels)
ncc list needs                     # who is looking for help (for suppliers)
ncc list channels                  # which search spaces exist, and how big

ncc match "help me book a hotel in Hangzhou" --channel booking   # my need → who can do it
ncc match "杭州酒店" --want need                                 # I want work → who needs it
ncc match "…" --from office                                      # match against an intranet node
```

Essentials:

- **Three kinds**: `--service` (index one of my offerings; category / keywords / summary are inherited),
  `--item` (index an artifact), `--need` (register a need).
- **Channels are free-form but not unruled**: `Booking/Hotel` == `booking/hotel`; `Food____RES` → `food-res`.
- **An index is not a grant**: being indexed only means findable — private artifacts still need
  `ncc grant`, non-public services still need a `service` grant. Indexing a non-`open` service
  **carries no endpoint** (the entry points at `ncc services show`).
- **Platform authoritative, nodes are copies**: `publish` writes the platform first and then pushes to
  joined intranet nodes; **a failed node push never rolls back the platform, but is reported per node**
  (`✓/✗`), and successfully pushed nodes show up in `index show`.
- **Ratings are never shown publicly**: ranking includes an internal reputation weight, but neither the
  API nor the CLI ever prints a score — access still requires a grant.

### `ncc registry config` (config hosting)

Besides artifacts, the intranet registry also **hosts team configuration** (network, gateway,
infra, agents, CI…): private by default, one revision per write, rollback, secrets encrypted at
rest — and agents can manage them with a scoped credential.

```bash
ncc registry config kinds                      # kinds (network/gateway/infra/agent/ci/security…) + formats + envs
ncc registry config set @team/network --file ./network.yaml --kind network --env prod \
    --summary "subnets/DNS/VLAN" --tags network,dns --note "initial"
ncc registry config list --mine                # my configs (private included; content masked)
ncc registry config get @team/network --reveal --out ./network.yaml    # plaintext to a file
ncc registry config history @team/network      # who changed what, when
ncc registry config rollback @team/network --to 2                      # rollback (written back as a new revision)
ncc registry config bundle --ns @team --env prod --out ./conf          # fetch the whole set (prod + any)
ncc registry config rm @team/network --yes
```

| Action | Requires |
|---|---|
| Read a public config | `visibility=public` and `status=active` → anyone |
| Read a non-public config | scope `config:read` **and** (namespace member **or** `ncc grant set --kind config`) |
| Write / rollback / delete | scope `config:write` **and** namespace membership (outsiders only get read) |

A long-lived credential for an agent is a scoped join ticket (the token's `Sub` is the issuer,
so it acts on your behalf inside your team namespace):

```bash
ncc registry ticket create --label agent-conf --scopes config:read,config:write,nodes:write
```

Worth knowing:

- **Not an artifact**: artifacts are distributable files (public, fan-out to workers); configs are
  authoritative team data (private, kept on the node you point at, never fanned out).
- **Content is masked by default**: without `--reveal` you only get `sha256` and size.
- **Secrets**: with `--secret`, content is AES-256-GCM encrypted before it touches the database
  (key derived from `jwt-secret`). Backing up the DB without that secret is safe; moving machines
  means the ciphertext can no longer be opened.
- **Bundles skip `secret` configs by default**: use `--secrets --reveal` when you really want them.

### `ncc registry share` (expiring links)

Turn an artifact into a **temporary download link** — the receiver does not need an account or the CLI.

```bash
ncc registry share create @team/report --label "for partner" --uses 1 --expires 7
#  → page  http://<node>/s/<token>        landing page with a download button
#    raw   http://<node>/s/<token>/raw    curl -OJ (only this endpoint counts a use)
ncc registry share list                        # mine (--all needs admin)
ncc registry share info "<link>"                # check a link (public, does not consume a use)
ncc registry share rm <SH-…|link>               # revoke — effective immediately
```

- **Share ≠ grant**: a share is a temporary, link-scoped allowance (limited uses / expiry / revocable);
  for lasting access to a person use `ncc grant`.
- **Creating a share is not privilege escalation**: only someone who can already read the artifact can share it.
- The token is stored as sha256 only and returned once; revoked / expired / exhausted links return `410`.

### `ncc registry admin` (node governance)

Manage the **users / nodes / services** of one intranet registry. Every action is audited.

Two equivalent identities: the **first account registered on the node** (becomes admin automatically —
just sign in), or a **machine credential** `AK-…` + secret (issued when the first admin appears, rotatable):

```bash
# The first registration prints the admin credential once (secret shown a single time)
ncc --base http://<node>:8282 register --email you@corp.com --password '***'
ncc registry admin login --key AK-XXXXXX --secret ****   # stored in ~/.ncc/config.json (0600)
ncc registry admin status                                # am I an admin? are local creds usable?
ncc registry admin rotate --label ops                    # rotate: new secret works, old one dies

ncc registry admin overview                              # users / nodes / services / assets / audit counts
ncc registry admin users --q bob                         # who registered here (disabled included)
ncc registry admin disable bob@corp.com --note "abuse"   # disable: existing tokens die immediately
ncc registry admin enable  bob@corp.com
ncc registry admin passwd  bob@corp.com                  # reset password (server-generated, shown once)
ncc registry admin nodes --kind service                  # all hosted nodes (private + offline included)
ncc registry admin rm-node ND-…                           # remove a node (its links are cleaned up too)
ncc registry admin services                              # node-side kind=service + artifact-side kind=api
ncc registry admin rm-service ND-…  |  @team/hotel-api    # remove node / archive artifact (bytes kept)
ncc registry admin audit --limit 20                      # who did what to whom, when
```

Two rules the server enforces: **you cannot disable your own account**, and
**you cannot disable the last usable admin**.

### `ncc hur profile` and data snapshot packages

What a package **is** used to be carried by a three-valued `kind` (`agent|harness|repo`). It now has a
name: **profile** — 12 of them (`agent` `harness` `plugin` `mcp` `app` `scaffold` `skill` `kb-seed`
`mem-seed` `ckpt-set` `trace-set`, plus `auth` — an **authorization package is not a snapshot**: it can
be updated and needs unlocking to read). The **envelope is unchanged** (deterministic bytes + `hur.json` +
`hur.lock` + signature); what the profile decides is **what it wants, whether it runs, how it hooks up,
and what it matches on**.

```sh
ncc hur profile --list                 # the whole profile table
ncc hur profile ./my-plugin            # what / wants / gives / how to hook up, plus a graded check
ncc hur match --profile plugin --host cursor
ncc hur init --profile mcp --name my-mcp        # the template generates the first 7 (agent…skill)
```

**A profile is a constraint, not a label.** Data profiles explicitly forbid `entry` and
`permissions.network` — a knowledge-base snapshot must not run code or reach the network on its own
(rule R12; the constraint also binds the **generator**: the generic template cannot produce data or
`auth` packages, so `ncc hur init --profile kb-seed` now **refuses and points at the right path**
(export a real snapshot from a node: `ncc kb bundle --as-package`, …; for authorization packages use
`ncc auth pkg init`) instead of writing a package that fails its own `verify`; a mistyped profile name
is an error too, never a silent substitute).

The artifact file name carries the same segment: `dist/…-0.1.0.kb-seed.hur.gz`, `…-0.1.0.plugin.hur.gz` —
so a `dist/` full of artifacts is scannable at a glance. **The name is a hint, the manifest is the
authority**: renaming a file does not change what it is (it only adds a warning), and a name we do not
recognise is never treated as a mistake.

**`hur` is only a spec; the file itself ends with the archive format**: `.hur` answers “this is a HUR
artifact”, the trailing `.gz` answers “this is a gzip container” — `file x.hur.gz` reports gzip, and
`gunzip -c x.hur.gz > x.zip` yields a plain zip anyone can list. The outer layer does the compressing,
the inner one only carries structure and traversal-safe unpacking, and **identical content still
produces an identical sha256**. Packages built before the switch (`.hur`, bare zip) keep verifying and
installing. The suffix follows the container format — swapping containers changes one constant.

All four data kinds (`kb` / `mem` / `ckpt` / `trace`) export as immutable **snapshot packages** that can
be poured back into any node:

```sh
ncc kb bundle --namespace @me --as-package ./kbseed --privacy internal --license CC-BY-4.0
ncc hur verify ./kbseed && ncc hur sign ./kbseed && ncc hur publish ./kbseed
ncc hur data import --package ./kbseed            # plan only — writes nothing
ncc hur data import --package ./kbseed --apply    # actually writes
```

Four boundaries: **live state never ships** (kb/mem/ckpt are rewritten, growing, private by default —
what travels is a snapshot); a snapshot **must say** where it came from, when it was taken, who may see
it, and under what licence; **the privacy level decides** (`privacy != public` ⇒ everything lands
`private`); **payloads are declared honestly** (`full` + `public` is refused). Design:
`ncc-platform/prd/ncc-hur-spec.md`.

Want a **real package** to copy from? `examples/html-deck-to-pptx` (`profile=skill`, a host skill with
python + node scripts) shows how to lay the directory out, how to hook it into Claude Code / Codex /
Cursor, and how to sign and publish it. `scripts/examples-smoke.sh` guards the rule that every example
must actually pass `ncc hur verify`.

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

### `ncc gateway` (attaching to the control plane)

The gateway keeps audit **on the machine** by default. Once attached to the control
plane (= ncc.ai) it also aggregates local audit into **window summaries** (minute/hour
level), signs them and reports them — **paths, payloads and credentials never leave the
machine**; the control plane only sees counts, byte totals, **hostnames**, status
buckets and latency percentiles. That red line is also enforced server-side: a "domain"
containing `/` or whitespace in a summary ⇒ 400.

```bash
ncc gateway init --accept llm            # a gateway config first (S2a)
ncc gateway bind --namespace @your-org   # register: token shown once, written to ~/.ncc/gateway.json (0600)
ncc gateway heartbeat                    # heartbeat (default meaning "online"; --status draining to self-report)
ncc gateway run                          # stay resident: heartbeats + reports on schedule
ncc gateway report --dry-run             # show which windows would be aggregated (send nothing)
ncc gateway report                       # really report (persist first, then send)
ncc gateway audit --remote --csv         # view/export what the control plane retains
ncc gateway usage                        # usage rollup (labelled "self-reported")
ncc gateway unbind                       # deregister + clear the local binding (local audit untouched)
```

Semantics worth remembering:

| Semantics | Notes |
|---|---|
| **Disconnection never loses audit** | Local ledger `~/.ncc/gateway-report.json` holds the watermark + a pending queue; items are **persisted before sending**. Unreachable control plane, revoked token, machine reboot — nothing is lost; `report` backfills later (the server dedupes by summary digest, so **no double counting**) |
| **Failures are never silent** | A failed report prints the reason, the pending count and the two ways out (backfill later / re-bind) — it never looks like "nothing happened" |
| **Online status is derived** | The control plane decides `offline` from a `last_seen_at` timeout; a client **cannot** self-report `offline` |
| **What the signature proves** | `HMAC-SHA256(key = sha256(gateway token), canonical summary)` proves **source and integrity** (this really came from the gateway holding that token, unmodified) — it does **not** prove the content is true (counters are self-reported) |
| **Re-binding = new identity** | `bind` clears the report ledger (local audit files untouched) so old queued windows are never attributed to the new gateway |

---

### `ncc app` (an NCC pod: a self-hostable personal agent)

**The product is yours, the engine is ncc, the app logic is a HUR package, the platform only provides safe interconnection.** One directory is one assistant:

```bash
ncc app init --dir ./my-pod --namespace @me --share-target cloud
ncc app doctor --dir ./my-pod     # item-by-item self-check, each with "what to run next"
ncc app up     --dir ./my-pod     # serve the loopback console at http://127.0.0.1:8487/
ncc app export --dir ./my-pod --out ./my-pod-export   # a directory someone else can deploy
```

A pod remembers **two targets** — that is the crux of the design:

| Which side | Which target | What lives there |
|---|---|---|
| Content | the **current target** (`ncc target use <node>`) | canvases/notes = knowledge base, memory, checkpoints |
| Sharing | `share.target` in `app.json` (default `hub`, the cloud) | point-to-point sharing: a **read-only snapshot** of the current content, as a link |

The console is a tiny loopback page: four panels plus one button ("generate snapshot link") — the only
write action, and a human clicks it. Whoever opens the link needs **no account** and sees a static copy
of that content; a keyed link requires sending the key along (it is shown once). They never see your
console and can never reach your node.

Three boundaries ship in the pod's own `README.md` and in `doctor` output: **the control plane only ever
holds summaries**, **a snapshot is not a grant** (long-term access is `ncc grant`), and **deleting the pod
does not delete your data** (it lives on the node).

End-to-end: `bash scripts/app-smoke.sh` (engine + node + platform, isolated ports and `NCC_HOME`, **51/51**).

---

### `ncc rsi` (a decision gate before the action: safety, drift, preferences, ledger)

Two things hurt most when nobody is watching: **slips** (`rm -rf`, `git push --force`, a write to
production) and **drift** (the task was "ship 0.3.0", and half an hour later it is refactoring the
directory layout). `ncc rsi` puts both **in front of the action**: ask *"may this step happen?"*,
record the verdict and the reasons, and be able to add it up afterwards. **Everything is local**
(policy, goal, preferences and ledger are files), it works offline, and the ledger never leaves the
machine.

| Block | Commands | Answers |
|---|---|---|
| **Policy gate** | `rsi check` / `rsi policy` | may this step happen |
| **Ledger** | `rsi guard` / `rsi report` | what actually happened while nobody watched |
| **Preferences** | `rsi pref` | what did this person say not to do |
| **Goal** | `rsi goal` | is this step still on the goal |

```bash
ncc rsi init --preset unattended-safe        # project-level .ncc-rsi/ (travels with the repo)
ncc rsi policy presets | set --preset strict-prod | check | show
ncc rsi goal set --statement "ship v0.3.0" --accept "deploy,release" --reject "refactor,rewrite"
ncc rsi pref add "do not touch the production database" --kind avoid --evidence "an incident in 2026-09"
ncc rsi check --command "git push --force"   # the exit code IS the verdict: 0 allow / 10 warn / 20 block / 1 config error
NCC_RSI_UNATTENDED=1 ncc rsi check --command "npm publish"   # unattended only tightens: warn becomes block
ncc rsi guard --reason "run tests" -- npm test  # check first; when blocked it really does not run
ncc rsi report --since 24h --json            # blocks / incidents / drifts / which preferences did work
ncc rsi hook install --host claude           # a 30-line sh shim + the PreToolUse snippet
```

| Point | Why it matters |
|---|---|
| **The exit code beats the JSON** | `0` allow / `10` warn / `20` block / **`1` usage or config error** — kept separate from "blocked", so no host ever reads a broken config as compliance |
| **Unattended only tightens** | `--unattended` / `NCC_RSI_UNATTENDED=1` does exactly one thing: warn becomes block. There is no "loosen it when unattended" switch |
| **Drift is judged against the goal** | a hit on a `--reject` word is drift; an active goal with no `--accept` hit is reported as **"cannot tell how this relates"** — never dressed up as a pass |
| **Preferences are stated by a human** | `avoid` takes part in the verdict, `prefer` is display-only; `pref suggest` only lists repeated reasons for a human to pick, **it never auto-writes** |

Three boundaries: **the gate is before the action** (`check` executes nothing; `guard` runs *your*
command, not a sandbox); **the ledger records facts only** (action, verdict, reasons — no secrets, no
file contents); **unreadable input exits non-zero** (never treat a broken config as a pass).

End-to-end: `bash scripts/rsi-smoke.sh` (**80/80**); spec in `ncc-platform/prd/ncc-rsi.md`.

---

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
| `~/.ncc/config.json` | The **target list**: address + credentials per target (session token, admin key+secret) and the current target name. Created on first run (mode 0600) |
| `~/.ncc/bin/ncc` | Binary installed by the install script or npm launcher |
| `~/.ncc/packages/` | Default root for `ncc install` |

Signing in to an intranet node does **not** sign you out of the cloud: credentials live per
target. `--base` reuses or creates a target by address instead of silently rewriting your
current one.

### Environment variables

| Variable | Used by | Effect |
|---|---|---|
| `NCC_CONFIG` | CLI | Config file location. Defaults to `~/.ncc/config.json` |
| `NCC_PACKAGES_DIR` | CLI | Install root for `ncc install`. Defaults to `~/.ncc/packages` |
| `NCC_INVITE_CODE` | CLI | Invite code for `ncc register` when `--invite` is omitted |
| `NCC_BIN` | npm wrapper | Force a specific binary path (checked first) |
| `NCC_RELEASE_BASE` | install script, npm wrapper, `ncc upgrade` | Base URL for downloading release binaries. Defaults to this repo's GitHub Releases |
| `NCC_UPDATE_URL` | `ncc upgrade` | Endpoint used for the latest-release lookup. Defaults to the GitHub releases API |
| `NCC_RSI_DIR` | `ncc rsi` | Force this directory (otherwise the nearest `.ncc-rsi/` upwards wins, else `~/.harnessuse/rsi`) |
| `NCC_RSI_UNATTENDED` | `ncc rsi` | `=1` means unattended: **warn is upgraded to block, nothing else** (for hosts and cron jobs) |
| `NCC_RSI_BLOCK_CODE` | `ncc rsi hook` | Which exit code the shim maps "blocked" to (default 2, the Claude convention) |
| `NCC_HOME` | install script, npm wrapper, `ncc upgrade` | Overrides the user home used to locate `~/.ncc/bin/ncc`. Handy for tests; must be the same value the wrappers see |

`HOME`, `HOSTNAME` and `SHELL` are read for defaults (config location, device name, POSIX summary) and can be overridden as usual.

> **The config file is plain text and holds a bearer token.** The CLI chmods it to `0600` on
> write, but if it was carried over from an older version, double-check it yourself:
> `chmod 600 ~/.ncc/config.json`. In CI, prefer `NCC_PACKAGES_DIR` / `NCC_CONFIG` pointing at a
> short-lived file over committing a credentials file.

## Using your own registry

Any NCC-compatible instance works as a backend:

```bash
# one-shot: leaves your current target alone
ncc --base https://registry.internal.example me

# make it stick: create a named target, then `ncc target use internal`
ncc target add internal --base https://registry.internal.example --use
ncc me
```

A self-hosted instance also serves its own client distribution, so its users can install a binary that already knows the right base URL:

- `GET /install.sh` — installer script, rebased onto the serving host
- `GET /downloads/<file>` — release binaries

To distribute your own builds through either path, set `NCC_RELEASE_BASE` to the mirror you control.

### Inside a private network: `ncc-registry`

[`ncc-registry`](https://github.com/fusedmodel/ncc-registry) is a self-contained intranet node — a single
Go binary (also an importable Go library) that hosts **artifacts**, hosts **nodes** (your agents and
services), and lets them **discover and connect to each other**. It scales out as one `master` plus any
number of `worker` edge nodes.

It lives in its own repository and is vendored here as a **git submodule** (`ncc-registry/`), so a plain
clone leaves that directory empty — use `git clone --recurse-submodules`, or `git submodule update --init`
in an existing checkout:

```bash
cd ncc-registry && go build -o dist/ncc-registry ./cmd/ncc-registry

# master (authoritative: accounts, artifacts, node directory, cluster view)
NCCR_PORT=8282 NCCR_NODE_NAME=office-master ./dist/ncc-registry

# worker (also hosts artifacts/nodes, reports its catalogue to the master)
NCCR_ROLE=worker NCCR_PORT=8283 NCCR_NODE_NAME=office-worker-a \
  NCCR_MASTER_URL=http://office-master:8282 ./dist/ncc-registry
```

Then point the CLI at it:

```bash
ncc --base http://office-master:8282 registry login --email you@corp.com --password '***'
ncc registry join --kind agent --name my-mac --region office --capabilities mcp,api --daemon
ncc registry status            # this node, the cluster, and my hosted nodes
ncc registry catalog           # aggregated directory (master + every worker)
ncc registry route @alice/hotel-skill   # which node actually holds it
ncc install @alice/hotel-skill          # bytes are proxied through the master
```

Rather than walking everyone through `--base` plus a password, issue a **join ticket** and hand out a
link — the secret rides in the URL fragment, so it never reaches server logs or `Referer`:

```bash
# on the registry: issue once, hand out the link
ncc registry ticket create --label "alice's agent" --uses 1 --expires 7
# → key NK-7F3A2C, secret (shown once), link http://office-master:8282/j/NK-7F3A2C#<secret>

# on the agent machine: one command, straight in
ncc registry add 'http://office-master:8282/j/NK-7F3A2C#<secret>' --join --kind agent
# or split it up:
ncc registry add --base http://office-master:8282 --key NK-7F3A2C --secret <secret> --join
```

What comes out is a **node token**: it may report its own heartbeat and read public artifacts, but it
cannot publish. Opening the link in a browser shows the same instructions plus a one-click check.

Two more things the intranet node does: **replicate** (push a copy to workers, so nearby nodes pull
locally) and **revoke** (take it down everywhere it was copied):

```bash
ncc publish --file ./hotel.SKILL.md --kind skill --name "Hotel Skill" --replicate all
ncc registry replicate @alice/hotel-skill --to all
ncc registry rm @alice/hotel-skill --yes     # master deletes it and reclaims every replica
```

And connections still are not authorization: reading someone's private artifact or private node needs
an explicit grant (`ncc grant set --user @bob --kind artifact|service`).

See [`ncc-registry/README.md`](ncc-registry/README.md) for the full API, config table and deployment notes.

## Scripting and CI

The CLI is designed to be driven by other programs:

- **Exit codes** — `0` on success, `1` on any failure.
- **Errors** — written to stderr as `✗ [error_code] message`, where the code and message come straight from the registry's JSON error body (`{"error":{"code":…,"message":…}}`). Network failures are reported separately as `网络错误: …`.
- **Structured data** — `ncc info <target>` prints the artifact record as pretty JSON on stdout, suitable for `jq`.
- **Non-interactive auth** — mint an API key once (`ncc key create --label ci`, printed exactly once) and use it in place of a login session.
- **Signature gate** — `ncc verify <file | @ns/slug> --require-signature` exits non-zero unless the signature is verifiable with a key this machine trusts; a *tampered* artifact fails without needing the flag. Note that "has a signature from an unknown key" also fails the gate — that is the point.
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
| `src/nodes.rs` | Node links (link table / discovery), grants, region aggregate and recommendations |
| `src/services.rs` | Services offered: catalog / matching / usage / declare and take down |
| `src/registry.rs`, `src/registryadd.rs` | Self-hosted intranet node (ncc-registry): login / join / catalog / route / tickets |
| `src/configs.rs` | Config hosting: catalog / fetch (masked by default) / write with revisions / rollback / bundle |
| `src/admin.rs` | Node governance (admin: users / nodes / services / audit / credential rotation) and share links |
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
3. Tag and publish a GitHub Release with those binaries attached — this is what the install script, the npm wrapper and `ncc upgrade` all resolve against.
4. `npm publish --access public` from `packages/ncc-cli`.
5. Update the [Status](#status) section: once a release exists, the install script and npm paths above become live.

Prebuilt targets: `darwin` (x86_64, arm64), `linux` (x86_64, arm64), `windows` (x86_64). Only `darwin-arm64` is checked into `release/bin` today; the rest are produced by `--all`.

## Repository layout

```
cli/                 Rust crate (bin: ncc)
packages/ncc-cli/    npm wrapper (@fusedmodel/ncc-cli) — launcher + binary downloader
examples/            Runnable package examples (one full HUR project per subdirectory; see examples/README.md)
ncc-registry/        Self-hosted intranet node — git submodule → github.com/fusedmodel/ncc-registry
                     (Go: single binary + importable library): artifacts + nodes + master/worker cluster
agent/               Agent integration pack (MCP config, SKILL.md, harness manifest)
release/bin/         Checked-in prebuilt binaries + checksums.txt
scripts/             build-release.sh (cross-compile + checksums)
```

## Use it from an agent

`ncc mcp` runs NCC as an **MCP server** over stdio, so any MCP-capable agent can search the catalog,
fetch artifacts, publish results and look up people, read its own knowledge base / memory / checkpoints
and run traces, read and leave **cross-agent / cross-user feedback**, check an organisation's gateways
plus their compliance audit summaries, and run the **RSI gate** (ask before acting) on this machine —
no extra service to run (43 tools, gated by what the connected target declares):

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
| `ncc_find_people` | Find people by role / skill / who you follow |
| `ncc_get_profile` | Someone's card: roles + portfolio + published capabilities |
| `ncc_match_services` | Match services by intent: scores, reasons, connection steps |
| `ncc_list_services` | Browse the service catalog (category / tag / region) |
| `ncc_match_index` | Find people from the **index** by one sentence of need (`want=need` finds demand); ranking has an internal weight and **never a score** |
| `ncc_list_index_channels` | Channels in the index (sizes, supply / need split) |
| `ncc_get_service` | Full connection details for one service |
| `ncc_service_categories` | Business category catalog for the `category` argument |
| `ncc_list_configs` | Hosted team configuration (public ones need no credential; `mine` for your own) |
| `ncc_get_config` | Fetch one config (**masked by default**; `reveal` returns plaintext) |
| `ncc_list_nodes` | Your node link table (`mine` / `links`) |
| `ncc_discover_nodes` | Connectable nodes on this instance |
| `ncc_region_profile` | Where your node network clusters |
| `ncc_recommend_nodes` | Nodes by region, same-region-first |
| `ncc_list_grants` | Grant relationships (outgoing / incoming) |
| `ncc_list_kb` | Hosted knowledge base: list / keyword-search documents |
| `ncc_get_kb` | Read one knowledge-base document (with its `checksum`) |
| `ncc_list_mem` | Hosted memory entries (key / value / kind / TTL / source) |
| `ncc_get_mem` | Read one memory by key — the path an agent takes |
| `ncc_list_ckpt` | Hosted checkpoints: metadata and digests (bytes via `ncc ckpt pull`) |
| `ncc_list_traces` / `ncc_trace_stats` | Run traces / the aggregate verdict (success rate, latency, tokens, cost, per version) |
| `ncc_p2p_probe` / `ncc_p2p_check` / `ncc_p2p_node` | Hole-punch preflight (local only) / a real 0-byte punch test / the target node machine's profile and entry state |
| `ncc_list_gateways` | Gateway control plane: which gateways exist, **are they online** (derived from heartbeats), how much they moved |
| `ncc_gateway_audit` | Retained audit **summaries** for one gateway: counts, **hostnames**, status buckets, latency percentiles |
| `ncc_gateway_usage` | Usage rollup for one gateway (**self-reported counters**, labelled in the response) |
| `ncc_feedback_list` | Read feedback (default: public ∪ mine ∪ addressed to me) |
| `ncc_feedback_summary` | Aggregates: counts / kinds / status / mean score / who is speaking (**not a ranking score**) |
| `ncc_feedback_send` | **Say one thing** (`about` + `body`); lands on the current target, private by default |
| `ncc_rsi_check` | **The gate before acting**: a command / an intent → `allow` / `warn` / `block` + reasons (**it judges, it never executes**) |
| `ncc_rsi_report` | Local ledger: blocks / incidents / drifts |
| `ncc_rsi_learn_digest` | Learning state (off by default; proposals need a human, `apply` is not a tool) |

The node, grant, service and **state** tools are **read-only on purpose**. Anything that changes what
another party can obtain — declaring a service, linking a node, granting access, replying to feedback,
changing a feedback's disposition, relaying a node's public feedback to the cloud — stays in the
CLI, where the user performs it deliberately. That also covers the state tools: letting a single
tool call silently rewrite an agent's memory or knowledge makes it impossible to reconstruct
afterwards who changed what — so `ncc kb set` / `ncc mem set` / `ncc ckpt save` stay in the CLI.

Two edges of feedback worth stating plainly: `ncc_feedback_send` is the front door for saying something
across agents and users, but it is **private by default** (only the author and the target owner see it)
and **append-only** (content cannot be edited), and its aggregates are **never ranking scores** (they do
not feed catalog ordering, service matching or any trust score); service feedback belongs on the target
declaring `services` and the tools **never switch targets for you**. The three RSI tools run on **the
machine running MCP**: a verdict is **not execution** (`check` runs nothing) and **not an authorization**
(anything needing the user's go-ahead still does); pass `unattended: true` when nobody is watching
(stricter only) and `dry: true` to look without leaving a ledger entry.

The three gateway tools read the **window summaries** the control plane retains — not the full audit:
paths, payloads and credentials stay on the **gateway machine**, so "which URL did who call" cannot be
answered from an agent; run `ncc gateway audit` on that machine instead. Two more caveats worth stating
out loud: online status is *derived* by the control plane from a heartbeat timeout, and usage is
**self-reported** (the signature proves source and integrity, not truth). Registering, revoking or
starting a gateway are actions taken by a person on that machine.

Search, fetch and the people directory need **no login** (the one exception is the `following`
filter, which asks "who do *I* follow" and so needs to know who you are); publishing does too.
`ncc mcp` writes only
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
