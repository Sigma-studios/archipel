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

It also comes as a desktop app for Linux, for one person running servers for their
friends on their own computer ([below](#on-your-own-computer)).

Games: **Minecraft** (vanilla or Paper, any version; Java is fetched to match),
**Urban Terror** 4.3 (its 1.4 GB of files downloaded once, shared by every server),
and from Steam **Puck**, **Team Fortress 2**, **Garry's Mod** and **Fistful of
Frags**. Adding one is writing a file (below).

Servers take files of their own in a tab named after them: **Plugins** for Paper
(uploaded, or found on [Modrinth](https://modrinth.com/plugins) for the server's
Minecraft version and installed with the plugins they require), **Maps** for Urban
Terror (`.pk3`), Team Fortress 2 and Fistful of Frags (`.bsp`), **Addons** for
Garry's Mod (`.gma`). Maps added there are offered as the server's first map.

Garry's Mod plays Sandbox, **Trouble in Terrorist Town**, **Murder** or **Prop Hunt**
out of the box: picking one brings what it needs from the Steam Workshop (the
gamemode, a map vote for TTT, and maps that need no Counter-Strike: Source), lists
its maps as the first map, and sets it up to change maps in turn. Its **Workshop**
tab shows that, and takes items or whole collections pasted by link: the server
downloads them at start (no Steam account needed), the maps and gamemodes found in
them join the settings' lists, and players get the map being played, the gamemode
and the models and sounds of the rest. The console offers the game's
commands as one types (Tab completes them), and lists them all on Tab with nothing
typed or from its Commands button; none take a slash, and a slash typed out of habit
before a known command is dropped.

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
and the ports are published there. The node must run x86_64 for every game but
Minecraft. Steam games install in each server's directory: Team Fortress 2 takes
about 15 GB, Garry's Mod 7 GB, Fistful of Frags 3 GB: size disk quotas for them.

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
| `ARCHIPEL_PROBE` | `off` | `on`: desktop apps may ask this archipel to check their ports from the internet |

With `ARCHIPEL_PROBE=on` (`facade env set archipel ARCHIPEL_PROBE=on --public`),
anyone may ask this archipel, without logging in, to try to reach their own
address on one port (`POST /api/probe`, see [docs/api.md](docs/api.md)): it is what
the desktop app's "checked from outside" asks. It only probes the caller's public
IPv4 address, ten times per address in ten minutes, sixteen at once. The caller's
address is the `X-Real-IP` facade's entry point sets: with a CDN or another proxy
in front of facade, that would be the proxy's, and every probe would aim at it.

## On your own computer

The desktop app (Linux, x86_64 or arm64: a `.deb` or an AppImage from the
[releases](https://github.com/Sigma-studios/archipel/releases)) is the same archipel
and the same UI, in a window, for the one person using the computer:

* **No login, no network port.** The window talks to archipel inside the app through a
  URL scheme of its own (`archipel://`): nothing listens for the UI, so nothing else
  on the network can reach it.
* **No limits, no quotas.** A server's memory is what its game is given (Minecraft's
  Java heap). Servers start when asked, never by themselves: not at login, not when
  the app opens. Closing the window stops them, once confirmed (worlds are saved).
* **Ports on the router.** When the app opens, archipel asks the router for a test
  forwarding, and the home page says whether friends could join at all. When a
  server starts, archipel asks the home router to
  forward its ports (NAT-PMP, then UPnP), renews that while it runs and takes it back
  when it stops. The server page then says who can join: anyone (with the address
  to share), only the home network (with the ports to forward by hand, if the router
  would not), or no one outside whatever the router does (the connection has no
  public address of its own: carrier-grade NAT, or a second router in front).
* **Kept to its files.** Servers run as you, so each is confined with Landlock
  (Linux 5.13 or later) to its own directory, the game files archipel shares and
  the system's: not your home folder, not the other servers. Without Landlock they
  run anyway, and their console says they are not confined.

A firewall on the computer (NixOS's, on by default; firewalld; ufw) turns players
away even when the router forwards: archipel says so, with how to open the pool in
it, until it sees the ports open.

A router may also accept a forwarding and not honour it, or the public address may
have changed; or it refuses, and you forward the pool by hand. So archipel checks
from outside, whatever the router said: it listens on a spare port of the pool and
asks a hosted archipel on the internet (`https://games.sigma-dev.eu` by default; any
archipel run with `ARCHIPEL_PROBE=on` will do) to reach it there, over TCP and UDP.
That service sees only your public address and that port. The check runs when the
app opens, on "Check again" and every 20 minutes, never while a server starts. Then
the app says "checked from outside", or that nothing got through and why. When the
router would not open the ports but the check gets through (the pool forwarded by
hand), friends can join and the app says so. Under **Settings** you can name another
archipel to ask, or turn checking off (the app then says what the router says, as
before). You can also give a hostname for friends, such as a dynamic DNS name like
`yourname.ddns.net`. Servers show it as their address only once the check saw it
point to this connection; otherwise they keep the IP address and say why.

Its data is in `~/.local/share/archipel`. Servers get the app's `PATH`. On NixOS,
the games archipel downloads (Java, Steam's, Urban Terror) are built for usual
distributions: build [`nix/`](nix/default.nix), which runs the app in an FHS
environment with what they need, or run the AppImage with `appimage-run`, or turn
on `programs.nix-ld`.
Steam games come through DepotDownloader, a 64-bit program that needs the system's
OpenSSL (`libssl3` on Debian and Ubuntu; `nix/` includes it); Fistful of Frags's
server is 32-bit, though, and needs 32-bit libraries (`lib32gcc-s1` and
`lib32stdc++6` on Debian and Ubuntu), which NixOS cannot provide where it looks for
them: it does not run there. Source servers also want `libcurl3-gnutls` and
`libtinfo5`. Every game but Minecraft needs x86_64.

## How servers are kept apart

* Each server runs as a user of its own (uid 20000 + its number) with a directory
  only it can read (`/data/servers/<n>`). archipel touches files there as that user,
  so a planted symlink leads nowhere.
* Each one has a cgroup with a memory limit, no swap and a process limit, inside its
  owner's cgroup holding their memory and CPU quota. Without cgroup delegation (older
  facade, or `--delegate-cgroups` off), archipel starts no server, and the Machine page
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

A `choice` can also list options archipel fetches itself, after its own: with
`options_from = "minecraft_versions"`, Minecraft's releases, newest first (those
Paper builds when the `flavor` setting is `paper`); with `options_from = "maps"`,
the maps among the server's addons; `workshop_maps` and `workshop_gamemodes` with
a `[workshop]` (below).

A game may take files of its owner's (`[addons]`), shown in a tab of their own,
and list its console commands (`[[commands]]`), offered as one types:

```toml
[addons]
label = "Maps"                  # the tab's name
dir = "maps"                    # in the server's directory
extensions = ["bsp"]            # the files that count there
help = "Shown above the list."
when = { flavor = "paper" }     # settings they need (optional)
search = "modrinth"             # Paper plugins on Modrinth (optional)

[[commands]]
name = "kick"
args = "<player>"
help = "Throw a player out"
```

Garry's Mod's Steam Workshop items come from a `[workshop]` section: where the
server reads the list of items to download at start and caches them, and
presets, one per gamemode, with their items, maps and console settings (see
[`games/gmod.toml`](games/gmod.toml)).

Installs are `minecraft`, `shared` (an archive downloaded once, read-only for every
server: `{base}`) or `steamcmd` (a Steam app: `app_id`, and `beta` for a branch of
it, downloaded anonymously with [DepotDownloader](https://github.com/SteamRE/DepotDownloader)
into each server's directory). Templates use `{port.<name>}`, `{setting.<key>}`, `{memory_mb}`,
`{java_memory_mb}`, `{dir}`, `{base}` and `{java}`; a value that is a single
placeholder keeps its type in JSON. See [`games/`](games/) for complete examples.

## Crates

| crate | |
|---|---|
| `archipel-core` | game definitions, templates, API types |
| `archipel` | the server: API, supervisor, installers, cgroups |
| `archipel-ui` | the web UI (Leptos, WebAssembly), embedded in `archipel` |
| `archipel-desktop` | the desktop app (Tauri): archipel and its UI in a window |

The API is described in [docs/api.md](docs/api.md).

### Working on it

```sh
# the UI, rebuilt on change and proxied to a local archipel on :7810
cd crates/archipel-ui && trunk serve        # http://127.0.0.1:8080
```

archipel itself needs Linux and root (it switches users and manages cgroups); run it
in its container (`docker build -t archipel .`). The desktop app needs WebKitGTK
(`libwebkit2gtk-4.1-dev`) and the Tauri CLI: build the UI as above, then
`cd crates/archipel-desktop && cargo tauri build`.
