# archipel

Game servers for a group of people on one machine, from the browser. An admin gives
each user a share of the machine (memory, CPU, disk, a number of servers); users
create servers from a list of games, start and stop them, type into their console,
change their settings and edit their configuration files.

archipel runs as one app on [facade](https://github.com/Sigma-studios/facade): its container is the whole pie, and
game servers are its own processes, each with its own user and its own cgroup inside
its owner's, so a user's servers together never take more than their share.

```
facade container (facade's limits: the pie)
└── archipel ─ web UI + API on :7810, behind facade's HTTPS
    ├── users/u2 (alice: 4 GB, 2 cores)
    │   ├── s1 Minecraft (Paper, 2 GB)
    │   └── s3 Puck (1.5 GB)
    └── users/u3 (bob: 2 GB, 1 core)
        └── s2 Urban Terror (512 MB)
```

Games: **Minecraft** (vanilla or Paper, any version; Java is fetched to match),
**Urban Terror** 4.3 (its 1.4 GB of files downloaded once, shared by every server),
**Puck** (from Steam). Adding one is writing a file (below).

## Deploying on facade

archipel needs facade's cgroup delegation (to enforce each server's limits), a pool
of ports published once, and a volume for its data:

```sh
facade app create archipel --git https://github.com/Sigma-studios/archipel --port 7810 \
  --domain games.example.com \
  --publish 30000-30019:30000-30019/tcp --publish 30000-30019:30000-30019/udp \
  -v data:/data:40G --memory 8192 --cpus 4 --pids 16384 \
  --delegate-cgroups true --stop-timeout 150 \
  --backup-every 6 --backup-before "archipel hook before-backup" \
  --backup-after "archipel hook after-backup"
facade env set archipel ARCHIPEL_PORTS=30000-30019 ARCHIPEL_PUBLIC_HOST=games.example.com --public
facade env set archipel ARCHIPEL_ADMIN_PASSWORD        # prompts; used on the first start only
facade deploy archipel
```

Then log in at `https://games.example.com` as `admin`, create users and give them
quotas. Players connect to `games.example.com:<port>`: the domain points at the node,
and the ports are published there. The node must run x86_64 for Urban Terror and Puck.

* **Memory and CPU**: archipel shares out its container's limits (`--memory`,
  `--cpus`) less 256 MB for itself (`ARCHIPEL_RESERVE_MB`). A server's memory is a hard
  limit (going over ends it); a user's CPU share caps all their servers together.
* **Ports** (`ARCHIPEL_PORTS`): each server gets consecutive ports from the pool, the
  same inside and outside. Publish the whole range in TCP and UDP, as above.
* **Stopping**: `--stop-timeout` must leave archipel time to stop every server cleanly
  (Minecraft saves its world on `stop`, given up to 90 s), so that updating archipel
  loses nothing.
* **Backups**: facade copies the volume; the hooks have every running Minecraft server
  save its world and pause saving meanwhile.

| variable | default | |
|---|---|---|
| `ARCHIPEL_DATA` | `/data` | database, servers' files, downloads |
| `ARCHIPEL_LISTEN` | `0.0.0.0:7810` | web UI and API, plain HTTP |
| `ARCHIPEL_PORTS` | `30000-30019` | the port pool |
| `ARCHIPEL_PUBLIC_HOST` | | the address players use; without it, the one the UI is browsed at |
| `ARCHIPEL_RESERVE_MB` | `256` | memory kept for archipel |
| `ARCHIPEL_ADMIN_PASSWORD` | | the first admin's password (otherwise random, in `<data>/initial-admin-password`) |

## How servers are kept apart

* Each server runs as a user of its own (uid 20000 + its number) with a directory
  only it can read (`/data/servers/<n>`). archipel touches files there as that user,
  so a planted symlink leads nowhere.
* Each one has a cgroup with a memory limit, no swap and a process limit, inside its
  owner's cgroup holding their memory and CPU quota. Without cgroup delegation (older
  facade, or `--delegate-cgroups` off), servers run without limits and the Machine page
  says so.
* If memory runs out anyway, the kernel ends a game server before archipel.

They share the network: a server can reach another's ports on localhost. Plugins and
mods are code, so give accounts to people you trust with the machine.

## Adding a game

A game is a TOML file: built-in ones are in [`games/`](games/), more go in
`/data/games/` (read at start; one with a built-in id replaces it). A definition says
how to get the game's files, which ports it needs, the settings its owner can change,
the files written from them before each start, and how to run, stop and tell it is
ready:

```toml
id = "mygame"
name = "My game"
description = "One line shown when picking a game."
badge = "🎮"
memory = { min_mb = 512, default_mb = 1024 }
ports = [{ name = "game", protocol = "udp", label = "Game" }]
install = { kind = "shared", url = "https://example.com/mygame-server.tar.gz", subdir = "server" }
connect = "{host}:{port.game}"

[run]
command = ["{base}/mygame-server", "--port", "{port.game}", "--config", "config.json"]
stop_command = "quit"           # typed into its console; or stop_signal = "SIGINT"
stop_timeout_secs = 30
ready = "Server started"        # a log line saying it is up

[[settings]]
key = "name"
label = "Server name"
type = "text"                   # text, secret, number (min, max), toggle, choice (options)
default = "My server"

[[files]]
path = "config.json"
format = "json"                 # properties, json (keys set, others kept) or text
editable = true                 # its owner can edit it in the Files tab
set = { port = "{port.game}", name = "{setting.name}" }
```

Installs are `minecraft`, `shared` (an archive downloaded once, read-only for every
server: `{base}`) or `steamcmd` (`app_id`, anonymous login, in each server's
directory). Templates use `{port.<name>}`, `{setting.<key>}`, `{memory_mb}`,
`{java_memory_mb}`, `{dir}`, `{base}` and `{java}`; a value that is a single
placeholder keeps its type in JSON. See [`games/`](games/) for complete examples.

## Crates

| crate | |
|---|---|
| `archipel-core` | game definitions, templates, API types |
| `archipel` | the server: API, supervisor, installers, cgroups |
| `archipel-ui` | the web UI (Leptos, WebAssembly), embedded in `archipel` |

The API is described in [docs/api.md](docs/api.md).

### Working on it

```sh
# the UI, rebuilt on change and proxied to a local archipel on :7810
cd crates/archipel-ui && trunk serve        # http://127.0.0.1:8080
```

archipel itself needs Linux and root (it switches users and manages cgroups); run it
in its container (`docker build -t archipel .`).
