# archipel HTTP API

JSON bodies are the types of `crates/archipel-core/src/api.rs` (and `GameDef` from
`game.rs`). Every endpoint but `POST /api/login`, `GET /api/about` and
`POST /api/probe` wants `Authorization: Bearer <token>`. Errors are `ErrorBody { error }` with a 4xx/5xx
status; 401 means "log in again".

The desktop app serves the same API to its own window, without login: there every
request is its one user's, and there is no `POST /api/login`.

## Account

| | | |
|---|---|---|
| `GET /api/about` | | `About`: `mode` (`hosted` or `desktop`) and version |
| `POST /api/login` | `LoginRequest` | `LoginResponse` |
| `POST /api/logout` | | 204 |
| `GET /api/me` | | `UserView` |
| `PUT /api/me/password` | `ChangePassword` | 204 |

## Games and servers

| | | |
|---|---|---|
| `GET /api/games` | | `Vec<GameDef>`, sorted by name |
| `GET /api/servers` | | `Vec<ServerView>`: the caller's, every one for admins |
| `POST /api/servers` | `CreateServer` | `ServerView` |
| `GET /api/servers/{id}` | | `ServerView` |
| `PUT /api/servers/{id}` | `UpdateServer` | `ServerView`; applies at the next start |
| `DELETE /api/servers/{id}` | | 204; stops it and deletes its files |
| `POST /api/servers/{id}/start` | | `ServerView` (answers at once; poll for the state) |
| `POST /api/servers/{id}/stop` | | `ServerView` |
| `POST /api/servers/{id}/restart` | | `ServerView` |
| `POST /api/servers/{id}/reinstall` | | `ServerView`; downloads its game files again (stopped servers) |
| `GET /api/servers/{id}/console?tail=200&follow=true` | | `text/plain` stream: past lines, then new ones as they come, one per line |
| `GET /api/servers/{id}/console/lines?after=0&wait=25` | | `ConsoleLines`: the lines after number `after`, waiting up to `wait` s (25 at most) for some; `reset` when the page must replace what it shows |
| `POST /api/servers/{id}/console` | `CommandRequest` | 204; typed into the server's console |
| `GET /api/servers/{id}/files` | | `Vec<FileView>`: the files its owner may edit |
| `PUT /api/servers/{id}/files?path=<path>` | `SaveFile` | `FileView` |
| `GET /api/servers/{id}/addons` | | `AddonsView`: the files in its game's `[addons]` directory, the settings they need that it lacks (`unmet`), and the game version a search looks for |
| `PUT /api/servers/{id}/addons/files/{name}` | the file's bytes | 204; adds or replaces it (512 MB at most; hosted, within the owner's disk quota). 400 for a name without one of the game's extensions |
| `DELETE /api/servers/{id}/addons/files/{name}` | | 204 |
| `GET /api/servers/{id}/addons/search?q=<text>` | | `Vec<AddonResult>`: from the game's catalogue (`search = "modrinth"`: Paper plugins for its Minecraft version); the most downloaded without `q` |
| `POST /api/servers/{id}/addons/install` | `InstallAddon` | `InstalledAddons`; downloads the project's newest release for that version, and what it requires that is not there yet. Installing one again replaces its file |
| `GET /api/games/{game}/options/{key}?<setting>=<value>&_server=<id>` | | `Vec<ChoiceOption>`: a `choice` setting's options, with those archipel lists (`options_from`); `_server` for the maps among that server's addons, or in its Workshop items (`workshop_maps`, after the gamemode's own; `workshop_gamemodes`) |
| `GET /api/servers/{id}/workshop` | | `WorkshopView`: what its gamemode brings from the Steam Workshop (`preset`), and the items and collections its owner added, as Steam describes them, with the maps and gamemodes the server found in them |
| `POST /api/servers/{id}/workshop` | `AddWorkshopItem` | `WorkshopView`; `link` is an item's or collection's page address, or its number. 400 for one of another game, private or taken down |
| `DELETE /api/servers/{id}/workshop/{item}` | | `WorkshopView` |

`ServerView.connect` may contain `{host}`: replace it with the address the page is
browsed at (`window.location.hostname`). Settings are `Value`s: a JSON string,
number or boolean, as the game's `SettingDef` says.

On the desktop, `ServerView.reach` says who can join a running server: `state` is
`checking`, `internet` (friends can join: the router forwards its ports, or a
check from outside got through where it would not; `public` is the address to
share), `local_only` (it would not: `forward` names the pool, to forward by hand)
or `no_public_address` (the connection has none of its own, whatever the router
does); `local` is the address on the home network, `detail` what went wrong.
`verified` (on `Internet` too) is what this computer's last check from outside
saw, made whatever the router said: `outside` is `reached`, `not_reached` (with
`detail`), `unverifiable` (no spare port, IPv6) or `unavailable` (checking off, or
the hosted archipel did not answer). `public` uses the settings' `public_host`
only once that check saw the name lead to this connection (`host`); otherwise
`host_note` says why not.

## Machine and users (admins)

| | | |
|---|---|---|
| `GET /api/overview` | | `Overview` (any user may read it) |
| `GET /api/internet` | | `Internet` (desktop app): whether friends could reach this computer, from a test forwarding asked of the router when the app opens; 404 hosted |
| `POST /api/internet/check` | | `Internet`, `checking` until the new check is done |
| `GET /api/settings` | | `Settings` (desktop app): the archipel asked to check from outside (`probe_url`, empty when off; `default_probe_url`) and the hostname for friends (`public_host`); 404 hosted |
| `PUT /api/settings` | `Settings` | `Settings`; a change checks again, as `POST /api/internet/check`. `probe_url` is empty or `https://…` (`http://localhost…` for development), `public_host` empty or a hostname |
| `GET /api/users` | | `Vec<UserView>` |
| `POST /api/users` | `CreateUser` | `UserView` |
| `PUT /api/users/{username}` | `UpdateUser` | `UserView` |
| `DELETE /api/users/{username}` | | 204; refused while they have servers |

## Checking from outside (hosted, `ARCHIPEL_PROBE=on`)

| | | |
|---|---|---|
| `POST /api/probe` | `ProbeRequest` | `ProbeResponse`; no login. 404 unless turned on |

A desktop app asks a hosted archipel to check that its ports reach it from the
internet. The hosted archipel connects to the caller's own address (the
connection's, or `X-Real-IP` when the connection comes from facade's entry point on
the same machine) on `port`, over TCP and UDP, sends `archipel-probe <nonce>\n`
and expects the same line back. `tcp` and `udp` are `reached`, `silent`,
`refused`, `wrong_answer` (something else answered) or `unsupported` (an IPv6
caller). With `host`, it also says whether that name's IPv4 addresses include the
caller's (`HostCheck`). 400 for a private caller or a request out of bounds (port
1024-65535, nonce of 16 to 64 letters and digits, a hostname), 429 after 10
requests in 10 minutes from one address, 503 when 16 checks run already.
