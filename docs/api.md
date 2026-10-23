# archipel HTTP API

JSON bodies are the types of `crates/archipel-core/src/api.rs` (and `GameDef` from
`game.rs`). Every endpoint but `POST /api/login` wants `Authorization: Bearer <token>`.
Errors are `ErrorBody { error }` with a 4xx/5xx status; 401 means "log in again".

## Account

| | | |
|---|---|---|
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
| `POST /api/servers/{id}/console` | `CommandRequest` | 204; typed into the server's console |
| `GET /api/servers/{id}/files` | | `Vec<FileView>`: the files its owner may edit |
| `PUT /api/servers/{id}/files?path=<path>` | `SaveFile` | `FileView` |

`ServerView.connect` may contain `{host}`: replace it with the address the page is
browsed at (`window.location.hostname`). Settings are `Value`s: a JSON string,
number or boolean, as the game's `SettingDef` says.

## Machine and users (admins)

| | | |
|---|---|---|
| `GET /api/overview` | | `Overview` (any user may read it) |
| `GET /api/users` | | `Vec<UserView>` |
| `POST /api/users` | `CreateUser` | `UserView` |
| `PUT /api/users/{username}` | `UpdateUser` | `UserView` |
| `DELETE /api/users/{username}` | | 204; refused while they have servers |
