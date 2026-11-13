# Hearth API, version 1

This is the reference Bonfire is built against. The Rust types for every body shown here
live in the `hearth-api` crate (`crates/hearth-api`), which a client depends on directly:
the module names below are its modules.

Contents: [Conventions](#conventions) · [Finding and trusting a Hearth](#finding-and-trusting-a-hearth) ·
[Accounts and sessions](#accounts-and-sessions) · [People and devices](#people-and-devices) ·
[Library](#library) · [Downloads](#downloads) · [Play data](#play-data) ·
[The live connection](#the-live-connection) · [Presence, chat and notifications](#presence-chat-and-notifications) ·
[Multiplayer services](#multiplayer-services) · [Shared worlds](#shared-worlds) ·
[Rest: cloud saves](#rest-cloud-saves) · [News and events](#news-and-events) ·
[Firelink](#firelink) · [Admin endpoints](#admin-endpoints)

## Conventions

| | |
|---|---|
| Base path | `/api/v1`. Every path in this document is below it unless it starts with `/dav`. |
| Bodies | JSON, UTF-8. Field names are snake_case. Ignore fields you do not know. |
| Ids | Integers below 2^53. |
| Times | Unix seconds, UTC. Fields end in `_at`. |
| Auth | `Authorization: Bearer <token>`. The token comes from sign-in. Everything needs it unless marked public. |
| Ports | 7300 TCP for everything in this document, 7301 UDP for the relay. The owner can change both. |
| Success | `200` with a body, `201`/`202` where noted, `204` without a body. |

### Errors

A failed request has a 4xx or 5xx status and this body:

```json
{ "error": { "code": "invalid_credentials", "message": "The username or password is not right." } }
```

`message` is written for the person in front of the screen and can be shown as is. Branch on
`code`, never on the message. New codes may appear: treat an unknown one like its HTTP status.

| Code | Status | Meaning |
|---|---|---|
| `bad_request` | 400, 416 | The request is malformed or a value is not allowed. |
| `unauthorized` | 401 | No session, or the session has ended. Sign in again. |
| `forbidden` | 403 | Signed in, but not allowed to do this. |
| `not_found` | 404 | It does not exist, or is not yours to see. |
| `conflict` | 409, 412 | The request does not fit the current state. The message says why. |
| `rate_limited` | 429 | Too many failed attempts from this address. Wait a few minutes. |
| `internal` | 500, 502 | Something broke on the Hearth. Its log has the detail. |
| `invalid_credentials` | 401, 403 | Wrong username or password. |
| `account_locked` | 403 | Five wrong passwords. Unlocks after 15 minutes, or with a reset link. |
| `totp_required` | 401 | Send the sign-in again with `totp`. |
| `totp_invalid` | 401, 403 | The 6-digit code is wrong. |
| `totp_setup_required` | 403 | Admins of this Hearth must set up two-step sign-in first. |
| `invite_invalid` | 403 | The invite is mistyped, used up or expired, or one is needed. |
| `username_taken` | 409 | |
| `weak_password` | 400 | Fewer than 8 characters. |
| `setup_required` | 503 | The Hearth has no admin yet. Its owner finishes setup in a browser. |
| `setup_complete` | 410 | The setup endpoints are closed. |
| `setup_code_invalid` | 403 | |
| `lobby_full` | 409 | |
| `lobby_locked` | 403 | The lobby is closed to newcomers, or needs an invite. |
| `save_conflict` | 409 | Another PC rested newer saves. See [Rest](#rest-cloud-saves). |
| `quota_exceeded` | 507 | The saves or the shared world do not fit their space. |
| `world_locked` | 409 | Someone else is hosting the shared world. |

## Finding and trusting a Hearth

### `GET /hearth` (public)

```json
{
  "name": "home", "version": "0.1.0", "api_version": 1, "setup_complete": true,
  "join": "invite", "tls": "self_signed",
  "cert_sha256": "50f4f5563073ec8dc78a05b28ca30dc59ea0ad8fe1db8e7812565792ac291ba8",
  "relay_port": 7301, "time": 1794593931
}
```

- `join` is `invite` (an invite code is needed to create an account) or `network` (anyone
  connecting from a private network address may create one).
- `tls` is `self_signed`, `lets_encrypt` or `proxy`.
- `cert_sha256` is set when the Hearth serves its own self-signed certificate: the SHA-256
  of the certificate in DER form, in hex.
- `relay_port` is `null` when the owner switched the relay off.

**Trusting a self-signed Hearth.** On first connection a client cannot verify the
certificate. It connects anyway, reads `cert_sha256`, compares it with the certificate the
server actually presented, and stores it. From then on it accepts exactly that certificate
for this Hearth and refuses any other. The certificate changes only when the owner changes
the Hearth's address.

### `GET /health` (public)

`{ "status": "ok", "version": "0.1.0", "uptime_seconds": 86400 }`

### Finding a Hearth on the local network

Send one UDP datagram of 5 bytes, `BNF1` followed by `0x04`, as a broadcast to port 7301.
Every Hearth on the network answers with `BNF1`, `0x05` and this JSON (`multiplayer::Discovered`):

```json
{ "name": "home", "version": "0.1.0", "port": 7300, "https": true, "cert_sha256": "50f4…" }
```

The Hearth's address is where the answer came from. Hearths only answer private addresses.

## Accounts and sessions

Module `auth`. A session belongs to one device, and a device has one session: signing in
again on a device ends the session it had. A client makes up a `client_id` once (any random
string), keeps it, and sends it with every sign-in, so the same PC stays the same device:

```json
"device": { "client_id": "6b1c…", "name": "Desktop", "platform": "Windows 11", "client": "Bonfire 0.1.0" }
```

| Request | Body | Answer |
|---|---|---|
| `POST /auth/register` (public) | `RegisterRequest` | `LoginResponse` |
| `POST /auth/login` (public) | `LoginRequest` | `LoginResponse` |
| `POST /auth/logout` | | `204` |
| `GET /me` | | `Me` |
| `GET /auth/sessions` | | `[Session]`, the caller's devices, most recently used first |
| `DELETE /auth/sessions/{id}` | | `204`. Own sessions, or any as an admin. |
| `POST /auth/password` | `ChangePassword` | `204`. Signs every other device out. |
| `GET /auth/reset/{token}` (public) | | `{ "username": "robin" }` or `not_found` |
| `POST /auth/reset` (public) | `ResetPassword` | `204`. Signs every device out. |
| `POST /auth/totp/setup` | | `TotpSetup` |
| `POST /auth/totp/enable` | `{ "code": "123456" }` | `204` |
| `POST /auth/totp/disable` | `{ "code": "123456" }` | `204` |

**Register.** `{ "invite_code": "BNFR-7Q2K-XM4D", "username": "robin", "password": "…", "device": { … } }`.
`invite_code` may be left out when `join` is `network` and the client is on the Hearth's
network. Codes are accepted in any case, with or without dashes. Usernames have 2 to 24
characters (letters, digits, `.`, `-`, `_`) and are unique without regard to case.
Passwords have at least 8 characters.

**Sign in.** `{ "username": "robin", "password": "…", "totp": "123456", "device": { … } }`.
`totp` is only needed when the account has two-step sign-in; without it such an account
answers `totp_required`. The answer:

```json
{
  "token": "pA3…43 characters…", "expires_at": 1797185931, "session_id": 12, "device_id": 4,
  "user": { "id": 2, "username": "robin", "role": "player", "created_at": 1794593931 },
  "totp_setup_required": false
}
```

`token` is shown once; the Hearth keeps only a hash. `expires_at` is `null` when sessions
never end; otherwise they end 7, 30 or 90 days after sign-in and the next request answers
`unauthorized`. `role` is `player` or `admin`. A sign-in from a device the Hearth has not
seen sends a `sign_in` notification to the account's other devices.

**Reset links** are made by an admin, work once, and are valid for 24 hours. The link's page
is the Hearth's own (`/reset/<token>`), so a client does not need these two endpoints.

Two-step codes are standard TOTP: SHA-1, 6 digits, 30 seconds.

## People and devices

Module `people`. Friends are everyone on the same Hearth.

| Request | Body | Answer |
|---|---|---|
| `GET /users` | | `[Person]`: id, username, role, last_seen_at |
| `GET /users/{id}/profile` | | `play::Profile` |
| `GET /me/privacy` | | `Privacy` |
| `PATCH /me/privacy` | `PrivacyPatch` | `Privacy` |
| `GET /devices` | | `[Device]`, the caller's own |
| `PUT /devices/current/installed` | `[Installed]` | `204` |
| `GET /devices/current/queue` | | `[QueuedDownload]` |
| `POST /devices/current/queue/{id}` | `{ "state": "started" }` | `204` |

**Privacy** has three switches: `share_playtime`, `share_achievements`, `share_now_playing`.
A profile's `playtime`, `recent`, `achievements` and `now_playing` are `null` when their
owner does not share them. Everyone always sees their own data in full.

**Installed games.** A client reports everything it has kindled on this PC as a list of
`{ "game_id": 1, "version_id": 3 }`. The list replaces what was known before. The ready
check for game nights reads this, so report after every install, update and uninstall.

**Remote downloads.** An admin can queue a game for a device. A connected device is told at
once with a `download_queued` event; any device asks for its queue when it comes online.
Report progress with `state` `started`, then `done` or `failed`.

## Library

Module `library`.

| Request | Answer |
|---|---|
| `GET /games` | `[GameSummary]`, the games players may see, by title |
| `GET /games/{id}` | `Game` |
| `GET /games/by-steam-app/{app_id}` | `GameSummary`, or `not_found` |
| `GET /art/{file}` | The picture a `cover`, `hero` or `logo` path points at |

```json
{
  "id": 1, "title": "Factorio", "developer": "Wube Software", "year": 2020, "genre": "Simulator, Strategy",
  "players": "online", "cover": "/api/v1/art/1-cover.jpg", "hero": null, "logo": null,
  "kind": "folder", "bytes": 1840000000, "version": { "id": 3, "name": "2.0.16" },
  "networking": true, "emulator_id": null, "visible": true, "added_at": 1794593931, "updated_at": 1794597000,

  "description": "…", "steam_app_id": 427520,
  "launch": [{ "name": "Factorio", "path": "bin/x64/factorio.exe", "args": "", "default": true }],
  "prerequisites": [{ "name": "Visual C++ 2015-2022", "path": "redist/vc_redist.x64.exe", "args": "/quiet" }],
  "versions": [{ "id": 3, "name": "2.0.16", "note": "Factory Friday stays on 2.0.15 until 6 Nov",
                 "state": "ready", "bytes": 1840000000, "files": 2315, "update_bytes": 220000000, "added_at": 1794597000 }]
}
```

The first block is `GameSummary`; `Game` adds the second.

- `players` is `single`, `coop` or `online`.
- `kind` says what the files are: `folder` (ready to run), `archive` (.zip, .7z, .rar to
  unpack), `installer` (.exe to run) or `image` (.iso).
- `version` is the newest version that can be downloaded, `null` while the Hearth is still
  preparing the first.
- `networking` is the per-game switch for Bonfire networking. When true, Bonfire puts its
  Steam API replacement in place for this game.
- `launch` paths are relative to the game's folder, with forward slashes. Exactly one entry
  is the default.
- `prerequisites` are installers inside the game's files that run once per PC. Key "already
  ran" on the file's hash from the manifest, so the same runtime is not run twice for two games.
- `emulator_id` names a shared [emulator](#emulators). The game's default launch `path` is
  then the game file.
- A cover that is `null` means the client draws its own stand-in.
- **Ownership** is the library's answer: a game the caller can fetch is a game they own.
  `GET /games/by-steam-app/{app_id}` is how the Steam replacement asks.

## Downloads

Each version has a manifest: its files, with a BLAKE3 hash per file and per 4 MiB chunk.

| Request | Answer |
|---|---|
| `GET /versions/{id}/manifest` | `Manifest` |
| `GET /versions/{id}/files/{index}` | The file's bytes. Honours `Range`. |
| `GET /versions/{to}/diff?from={from}` | `Diff` |

```json
{
  "version_id": 3, "game_id": 1, "chunk_size": 4194304, "bytes": 1840000000,
  "files": [{ "path": "bin/x64/factorio.exe", "size": 23068672,
              "hash": "9f2c…64 hex…", "chunks": "…64 hex per chunk, one after the other…" }]
}
```

- A file's position in `files` is its `index`.
- **Kindling.** For each file, fetch chunk `n` with `Range: bytes=<n*chunk_size>-<end>`,
  hash it with BLAKE3 and compare with characters `n*64 .. n*64+64` of `chunks`. The last
  chunk is shorter. When all chunks are in, the BLAKE3 of the whole file equals `hash`.
- **Resuming.** Keep the chunks that verified, fetch the rest. Any byte range works:
  `bytes=a-b`, `bytes=a-`, `bytes=-n`. One range per request.
- **Verifying an install.** Hash the local files the same way and compare with the manifest.
  Fetch the chunks that differ.
- `409 conflict` on a file means it changed on the Hearth's disk since it was hashed. The
  Hearth hashes it again; fetch the manifest again in a moment.
- Old versions stay downloadable, so a client can stay on one to match a server.

**Updating** from version `from` to version `to`:

```json
{
  "from": 2, "to": 3, "bytes": 220000000,
  "delete": ["old.txt"],
  "fetch": [
    { "index": 14, "path": "data/base.pak", "size": 8389842, "chunks": [1], "bytes": 4194304 },
    { "index": 20, "path": "new.txt", "size": 19, "chunks": null, "bytes": 19 }
  ]
}
```

Delete what `delete` names. For each entry of `fetch`, download the listed chunks into the
file at their offsets (resize the file to `size` first), or the whole file when `chunks` is
`null`. `bytes` says up front how much will travel. Everything not named is already right.

### Emulators

Module `community`. An emulator is stored once on the Hearth and shared by the games that
name it, so a player downloads it with the first such game only.

| Request | Answer |
|---|---|
| `GET /emulators` | `[Emulator]` |
| `GET /emulators/{id}/manifest` | `EmulatorManifest`, same shape of `files` as a version's |
| `GET /emulators/{id}/files/{index}` | The file's bytes. Honours `Range`. |

```json
{ "id": 1, "name": "DuckStation", "version": "0.1", "systems": ["PlayStation"],
  "command": "duckstation.exe -fullscreen \"{rom}\"", "folder": "DuckStation",
  "bytes": 52000000, "ready": true, "games": 3 }
```

To start a game with `emulator_id`, run the emulator's `command` from the emulator's folder
with `{rom}` replaced by the full path of the game's default launch `path`.

## Play data

Module `play`. All of it is stored on the Hearth, so it follows an account to every PC.

| Request | Body | Answer |
|---|---|---|
| `POST /play/sessions` | `{ "game_id": 1 }` | `PlaySession` |
| `POST /play/sessions/{id}/heartbeat` | | `204` |
| `POST /play/sessions/{id}/stop` | | `204` |
| `GET /me/playtime` | | `[Playtime]`: game_id, seconds, sessions, last_played_at |
| `GET /me/sessions?game_id=&limit=` | | `[PlaySession]`, newest first |
| `GET /games/{id}/achievements` | | `Achievements`: `defined` and the caller's `unlocked` |
| `PUT /games/{id}/achievements/{name}` | | `204`. Unlocking twice keeps the first time. |
| `DELETE /games/{id}/achievements/{name}` | | `204` |
| `GET /games/{id}/stats` | | `{ "rockets": 4, "hours": 41.5 }` |
| `PUT /games/{id}/stats` | `{ "rockets": 5 }` | All stats. Stats not named keep their value. |
| `GET /games/{id}/players` | | `[GamePlayer]`: how everyone compares in this game |
| `GET /collections` | | `[Collection]`: id, name, game_ids |
| `POST /collections` | `{ "name": "Couch co-op", "game_ids": [1, 4] }` | `Collection` |
| `PATCH /collections/{id}` | `{ "name": "…", "game_ids": […] }`, either or both | `Collection` |
| `DELETE /collections/{id}` | | `204` |
| `GET /games/{id}/note` | | `{ "text": "…", "updated_at": 1794597000 }` |
| `PUT /games/{id}/note` | `{ "text": "…" }` | The note. Empty text removes it. |

**Sessions.** Start one when a game starts, send a heartbeat every 60 seconds, stop it when
the game exits. A session without a heartbeat for 180 seconds counts as ended at its last
heartbeat, so playtime is right even when a PC loses power. While a session runs, the
person's presence is `playing`.

**Achievements** are unlocked by the name the game itself uses. An admin may describe a
game's achievements (title, description, hidden); unlocks are recorded either way.

## The live connection

Module `social`. Open one WebSocket at `GET /api/v1/ws`, authenticated like any request
(`Authorization: Bearer …`). The Hearth sends events as JSON text frames:

```json
{ "type": "chat_message", "data": { "id": 7, "sender_id": 2, "recipient_id": 3, "body": "Bring iron.", "sent_at": 1794597000, "read_at": null } }
```

Send `{ "type": "ping" }` (or a WebSocket ping frame) at least every 30 seconds; the answer
is `{ "type": "pong" }`. A connection that stays silent for 90 seconds is closed. A
connection is also closed when its session ends, for example when the device is signed out.

Being connected is what makes someone online. Lobbies and peer-to-peer sessions need a
connection, and someone who stays disconnected for 20 seconds leaves them.

Everything an event announces can also be fetched with a plain request, so a client that
was offline catches up by asking.

| Event `type` | `data` | When |
|---|---|---|
| `hello` | `Hello`: user_id, time, ping_seconds | First event on every connection |
| `presence` | `Presence` | Someone's presence changed |
| `chat_message` | `ChatMessage` | A message to you, or one you sent from another device |
| `chat_read` | `ChatRead`: reader_id, peer_id, up_to | Messages were read |
| `notification` | `Notification` | See below |
| `download_queued` | `people::QueuedDownload` | An admin queued a download for this device |
| `lobby_updated` | `multiplayer::Lobby`, the whole lobby | Members, owner, data or settings changed |
| `lobby_left` | `{ "lobby_id": 1 }` | You are no longer in this lobby |
| `lobby_chat` | `multiplayer::LobbyChatMessage` | A lobby message, your own included |
| `invite` | `multiplayer::Invite` | Someone invited you |
| `join_requested` | `multiplayer::JoinRequested` | Someone asks to join your game |
| `p2p_offer` | `multiplayer::P2pSession`, your side of it | Someone opened a session with you |
| `p2p_signal` | `multiplayer::P2pSignalled` | The other side sent a signal |
| `p2p_closed` | `{ "session_id": 1 }` | The session was closed or timed out |
| `world_updated` | `saves::World` | A shared world you are in changed |
| `pong` | | Answer to a ping |

Ignore event types you do not know.

## Presence, chat and notifications

| Request | Body | Answer |
|---|---|---|
| `GET /presence` | | `[Presence]`, everyone, offline people included |
| `PUT /presence` | `SetPresence` | `204` |
| `GET /chats` | | `[Conversation]`: peer_id, last message, unread count |
| `GET /chats/{user_id}/messages?before=&limit=` | | `[ChatMessage]`, oldest first |
| `POST /chats/{user_id}/messages` | `{ "body": "…" }` | `ChatMessage` |
| `POST /chats/{user_id}/read` | `{ "up_to": 7 }` | `204` |
| `GET /notifications?unread=true` | | `[Notification]`, newest first, at most 100 |
| `POST /notifications/read` | `{ "ids": [1, 2] }`; empty marks all | `204` |

```json
{ "user_id": 2, "status": "playing", "game_id": 1, "detail": "in Factory Friday",
  "since": 1794597000, "rich": { "status": "Factory Friday" }, "lobby_id": 4401 }
```

- `status` is `offline`, `online`, `away` or `playing`. The Hearth works it out: connected
  means online, a running play session means playing. A client only sets `away`, the
  `detail` line and the `rich` pairs (Steam's rich presence) with `PUT /presence`.
- `lobby_id` is a lobby friends can join through this person.
- Someone who switched off "what I am playing now" shows as `online` without a game.

Notification `kind` is `invite`, `update`, `event`, `news` or `sign_in`. `data` holds the
ids the notification is about (`game_id`, `version_id`, `lobby_id`, `event_id`, `news_id`,
`user_id`, `device_id`, whichever apply), for the client to open the right screen.

## Multiplayer services

Module `multiplayer`. This is what Bonfire's replacement for `steam_api64.dll` maps onto.
Lobbies and sessions live in the Hearth's memory and need the [live connection](#the-live-connection).

| Steam interface | What the replacement calls |
|---|---|
| `ISteamUser` | `GET /me` for identity, `POST /tickets`, `POST /tickets/verify`, `DELETE /tickets/{ticket}` |
| `ISteamFriends` | `GET /users`, `GET /presence`, `PUT /presence`, `POST /invites`, `POST /users/{id}/join-request` |
| `ISteamMatchmaking` | The lobby endpoints below |
| `ISteamNetworkingSockets`, `ISteamNetworkingMessages`, `ISteamNetworking` | `POST /p2p/sessions` and the relay |
| `ISteamUserStats` | The achievement and stats endpoints under [Play data](#play-data) |
| `ISteamApps` | `GET /games/by-steam-app/{app_id}` |
| `ISteamUtils` | `GET /hearth` for the server time |
| `ISteamRemoteStorage` | [Rest](#rest-cloud-saves) |

Ids are integers below 2^53, so they fit into the account-id part of a 64-bit Steam id.

### Tickets

| Request | Body | Answer |
|---|---|---|
| `POST /tickets` | `{ "game_id": 1 }` | `{ "ticket": "…", "expires_at": … }` |
| `POST /tickets/verify` | `{ "ticket": "…" }` | `{ "user_id": 2, "username": "robin", "game_id": 1, "owned": true }` |
| `DELETE /tickets/{ticket}` | | `204`. Only the ticket's owner cancels it. |

A ticket is proof of identity to hand to another player or a game server, who asks the
Hearth what it stands for. A dedicated server may call `verify` with the Firelink registry
token as its bearer token instead of a session. Tickets last 24 hours.

### Lobbies

| Request | Body | Answer |
|---|---|---|
| `POST /lobbies` | `CreateLobby` | `Lobby` |
| `GET /lobbies` | | `[Lobby]` the caller is in |
| `POST /lobbies/search` | `LobbySearch` | `[Lobby]` |
| `GET /lobbies/{id}` | | `Lobby` |
| `PATCH /lobbies/{id}` | `LobbyPatch`, owner only | `Lobby` |
| `POST /lobbies/{id}/join` | | `Lobby`, or `lobby_full` / `lobby_locked` |
| `POST /lobbies/{id}/leave` | | `204` |
| `PUT /lobbies/{id}/members/me` | `{ "ready": "1" }` | `Lobby`. Pairs not named keep their value. |
| `POST /lobbies/{id}/chat` | `{ "body": "…" }` | `204`. Everyone gets a `lobby_chat` event. |

```json
{ "id": 4401, "game_id": 1, "owner_id": 2, "kind": "friends", "max_members": 4, "joinable": true,
  "members": [{ "user_id": 2, "username": "robin", "data": { "ready": "1" } }],
  "data": { "name": "Factory Friday", "map": "Nauvis" }, "created_at": 1794597000 }
```

- `kind` is `private` (invite only, never found), `friends`, `public` (both found by
  searches; on a Hearth everyone is a friend) or `invisible` (not found, joinable by id).
- The owner changes `kind`, `max_members`, `joinable`, hands over with `owner_id`, and edits
  data with `set` (pairs) and `delete` (keys).
- When the owner leaves, the lobby passes to the member who has been in it longest. When
  the last member leaves, it is gone.
- A search returns joinable `friends` and `public` lobbies of the game with a free place.
  `filters` compare a data key with a value (`equal`, `not_equal`, `less`, `less_or_equal`,
  `greater`, `greater_or_equal`), as text without regard to case or, with `numeric`, as
  numbers. `near` sorts by closeness of a numeric key. `slots_available` and `limit` narrow further.
- `body` of a lobby message is opaque to the Hearth, up to 8 KB. Encode bytes as text.

### Invites and join requests

| Request | Body | Answer |
|---|---|---|
| `POST /invites` | `{ "user_id": 3, "game_id": 1, "lobby_id": 4401, "connect": "" }` | `204` |
| `POST /users/{id}/join-request` | `{ "game_id": 1 }` | `204` |

An invite arrives as an `invite` event and as a notification. Naming a `lobby_id` the
caller is in also lets the invited person into a private lobby. `connect` carries what a
game without lobbies needs to connect. A join request arrives as `join_requested`; the
other client usually answers with an invite.

### Peer-to-peer sessions and the relay

| Request | Body | Answer |
|---|---|---|
| `POST /p2p/sessions` | `{ "peer_id": 3, "game_id": 1, "lobby_id": 4401 }` | `P2pSession` |
| `GET /p2p/sessions/{id}` | | `P2pSession` |
| `POST /p2p/sessions/{id}/signal` | `{ "data": { … } }` | `204`. The peer gets `p2p_signal`. |
| `POST /p2p/sessions/{id}/route` | `{ "route": "direct" }` | `204` |
| `DELETE /p2p/sessions/{id}` | | `204`. The peer gets `p2p_closed`. |

```json
{ "id": 9012, "peer_id": 3, "game_id": 1, "lobby_id": 4401,
  "token": "00112233445566778899aabbccddeeff", "relay_port": 7301, "route": "pending" }
```

Opening a session that already exists between the two people for the game returns it. The
peer is told with a `p2p_offer` event carrying their own side: same `id`, their own `token`.

**Connecting, direct first:**

1. Each side opens a UDP socket and sends a `PROBE` to the relay from it. The answer is the
   address the relay sees that socket at, which is its public address.
2. Each side sends its candidate addresses (the public one, and its local ones) to the
   other with `signal`. The Hearth passes `data` on untouched.
3. Both send packets to each other's candidates. When one arrives, that path is the direct
   route: report `direct`.
4. If nothing arrives within a few seconds, send through the relay instead.
5. Send a `PROBE` at least once a minute for as long as the session is in use. It keeps the
   session and the path through the router alive. A session silent for 5 minutes is closed.

**The relay** is one UDP port. Every datagram starts with the 4 bytes `BNF1` and one type byte.

| Type | Byte | After the type byte | Meaning |
|---|---|---|---|
| `PROBE` | 1 | token, 16 bytes | "Which address do you see me at?" Also tells the relay where this side is. |
| `PROBE_REPLY` | 2 | family (4 or 6), address (4 or 16 bytes), port (2 bytes, big endian) | The answer. |
| `DATA` | 3 | token, 16 bytes, then up to 1400 bytes of payload | Forwarded to the other side. |
| `DISCOVER` | 4 | nothing | See [Finding a Hearth](#finding-a-hearth-on-the-local-network). |
| `DISCOVER_REPLY` | 5 | JSON | |

The token is the 32 hex characters of `token`, as 16 raw bytes. A side sends `DATA` with
its own token. The other side receives the same payload from the relay's address, with the
token replaced by **its own** token for the session, which is how it tells sessions apart.
Neither side learns the other's token. The relay answers nothing for a token it does not
know, and forwards nothing until both sides have sent it at least one packet.

`route` becomes `relay` as soon as the relay forwards data for the session. With the relay
switched off, `relay_port` is `null` and only direct paths are possible.

## Shared worlds

Module `saves`. A shared world is a multiplayer save that lives on the Hearth instead of on
one person's PC. Whoever starts it becomes the host and holds it until they give it back.

| Request | Body | Answer |
|---|---|---|
| `GET /worlds?game_id=` | | `[World]` the caller is a member of |
| `POST /worlds` | `{ "game_id": 1, "name": "Noor's farm", "max_players": 4 }` | `World` |
| `GET /worlds/{id}` | | `World` |
| `DELETE /worlds/{id}` | | `204`. Its maker, or an admin. |
| `POST /worlds/{id}/members` | `{ "user_id": 3 }` | `World`. Any member can bring someone in. |
| `DELETE /worlds/{id}/members/{user_id}` | | `204`. Yourself, or others as its maker or an admin. |
| `POST /worlds/{id}/host` | | `World`, or `world_locked` |
| `POST /worlds/{id}/heartbeat` | | `204` |
| `POST /worlds/{id}/release` | `{ "store": true, "note": "Planted pumpkins." }` | `World` |
| `GET /worlds/{id}/journal` | | `[JournalEntry]`, newest first |
| `POST /worlds/{id}/journal` | `{ "note": "…" }` | `201` |

```json
{ "id": 1, "game_id": 4, "name": "Noor's farm", "max_players": 4, "created_by": 2,
  "members": [{ "user_id": 2, "username": "robin" }, { "user_id": 3, "username": "noor" }],
  "revision": 7, "bytes": 2300000, "host": { "user_id": 3, "username": "noor", "since": 1794597000 },
  "updated_at": 1794590000 }
```

**Hosting a world:**

1. `POST /worlds/{id}/host`. From now on only the caller can write to the world.
2. Fetch the files from `/dav/worlds/{id}/upload/` (they start as the world's stored state),
   start the game's server on them, and send a heartbeat every 60 seconds.
3. When the host stops: sync the files back into `/dav/worlds/{id}/upload/`, then
   `POST /worlds/{id}/release` with `store: true` and a note for the next host.

`revision` counts up with every store; `0` means the world has no files yet. A host who has
been silent for 5 minutes no longer holds the world, and the next person can take it; what
that host did not store is lost. Members read the stored state at
`/dav/worlds/{id}/current/`. The journal lists every hosting session with its length and note.

## Rest: cloud saves

Module `saves`. Files travel over WebDAV at `/dav`, which rclone speaks. The JSON endpoints
say what is stored and turn an upload into a revision.

**rclone** is configured like this, with the session token:

```ini
[hearth]
type = webdav
url = https://home.lan:7300/dav
vendor = rclone
bearer_token = pA3…
```

With `vendor = rclone`, rclone sends each file's modification time, so a sync compares size
and time and skips what did not change. WebDAV clients that only know passwords can send
the token as the password of basic auth, with any username.

**Layout** for the saves of game `{game_id}`:

| Path | |
|---|---|
| `/dav/saves/{game_id}/upload/` | The caller's working area. Sync the local saves into it, then commit. After a commit it holds the committed files again, so the next sync sends only changes. |
| `/dav/saves/{game_id}/current/` | The newest revision, read-only. Sync from it to restore. |
| `/dav/saves/{game_id}/rev/{number}/` | One kept revision, read-only. |

| Request | Body | Answer |
|---|---|---|
| `GET /saves` | | `Saves`: used_bytes, quota_bytes, games |
| `GET /saves/{game_id}` | | `GameSaves`: head, revisions newest first |
| `POST /saves/{game_id}/commit` | `{ "base": 3, "force": false }` | `Revision`, or `save_conflict` / `quota_exceeded` |
| `POST /saves/{game_id}/restore` | `{ "revision": 2 }` | `Revision`, the new head |
| `DELETE /saves/{game_id}` | | `204`. Deletes every rested save of the game. |

```json
{ "number": 4, "device_id": 7, "device_name": "Laptop", "committed_at": 1794597000,
  "bytes": 2300000, "files": 12, "hash": "…64 hex…" }
```

**Resting** (after a game exits): `rclone sync <saves> hearth:saves/{game_id}/upload`, then
commit with `base` set to the revision number this PC last synced with (`null` if never).
Remember the returned `number` and `hash`.

**Before a game starts:** `GET /saves/{game_id}`.

- `head` is `null`, or its `number` is what this PC remembers: nothing to do.
- `head` is newer and the local saves are unchanged since the last sync: restore with
  `rclone sync hearth:saves/{game_id}/current <saves>` and remember the new head.
- `head` is newer **and** the local saves changed too: that is a conflict. Ask the person
  which to keep. To keep the local ones, sync up and commit with `force: true`; the other
  revision stays in the history. To keep the Hearth's, restore.

A commit whose `base` is not the head answers `save_conflict` unless `force` is set. Two
revisions with the same `hash` hold the same saves; committing files identical to the head
returns the head without making a revision. Five revisions per game are kept. The quota
(2, 5 or 10 GB per person) covers all kept revisions: older ones are dropped to make room,
and a single set of saves larger than the quota is refused with `quota_exceeded`.

## News and events

Module `community`.

| Request | Body | Answer |
|---|---|---|
| `GET /news` | | `[NewsPost]`, pinned first, then newest |
| `GET /events` | | `[GameNight]`, upcoming, soonest first |
| `PUT /events/{id}/rsvp` | `{ "rsvp": "going" }` | `GameNight` |
| `GET /events/{id}/ready` | | `ReadyCheck` |

A news `body` is Markdown. `rsvp` is `going`, `maybe`, `no`, or `null` to take the answer back.

```json
{ "event_id": 3, "game_id": 1, "version": { "id": 3, "name": "2.0.16" },
  "guests": [{ "user_id": 2, "username": "robin", "ready": "needs_update" }] }
```

The ready check covers everyone who is going. `ready` is `ready` (a PC of theirs has the
newest version), `needs_update`, `not_kindled`, or `getting_ready` (a download is queued for
one of their PCs). It is built from what devices report with `PUT /devices/current/installed`.

## Firelink

Module `community`. The registry of dedicated game servers.

| Request | Body | Answer |
|---|---|---|
| `GET /game-servers?game_id=` | | `[GameServer]` |
| `POST /game-servers/heartbeat` | `Heartbeat` | `204` |

```json
{ "id": 2, "game_id": 1, "name": "Sunday factory", "address": "home.lan:34197",
  "players": 3, "max_players": 16, "map": "nauvis", "version": "2.0.16",
  "online": true, "ping_ms": 12, "last_seen_at": 1794597000, "self_listed": false, "added_by": "mika" }
```

Servers an admin added are checked every 30 seconds, first with the server query many
dedicated servers answer over UDP (which also gives players and map), then with a plain TCP
connection.

**A server lists itself** by posting every 30 seconds, with the registry token from the
Firelink page of the admin UI as its bearer token:

```sh
curl -X POST https://home.lan:7300/api/v1/game-servers/heartbeat \
  -H "Authorization: Bearer hb_7f3k9q2mxw81…" -H "Content-Type: application/json" \
  -d '{"name":"Noor'"'"'s cabin","game_id":4,"port":24642,"players":2,"max_players":4,"map":"Farm"}'
```

Name the game with `game_id` or `steam_app_id`. `host` defaults to the address the
heartbeat came from. A server that misses three heartbeats drops off the list.

## Admin endpoints

Everything under `/admin` needs a session whose user is an admin. The web UI uses these,
and so can scripts. Bodies are in the modules named.

| Request | Purpose |
|---|---|
| `GET /admin/overview` | `admin::Overview`: figures for the Overview page. |
| `GET` / `PATCH /admin/settings` | `settings::SettingsView` / `SettingsPatch`. Secrets are blank on read, with a `*_set` flag. A changed port or encryption mode makes the Hearth listen anew within a second. |
| `GET /admin/folders/browse?path=` | Folders inside a folder on the Hearth's machine. Never files. |
| `POST /admin/folders/scan` | How many games a folder holds and how large it is. |
| `GET /admin/library` | `library::Library`: every game, hidden ones included, and the scan status. |
| `POST /admin/library/scan` | Start a rescan. `202`. |
| `PUT /admin/library/upload?name=&folder=` | Upload one .zip, .7z, .rar, .exe or .iso into a games folder. The body is the file. `201`. |
| `GET` / `PATCH /admin/games/{id}` | `library::Game` / `GamePatch`. |
| `PATCH /admin/versions/{id}` | `library::VersionPatch`: name and note. |
| `PUT /admin/games/{id}/achievements` | `[play::AchievementDef]`, replaces the list. |
| `GET /admin/review` | `[library::ReviewItem]`: files waiting in Metadata review. |
| `POST /admin/review/{id}` | `library::ReviewAction`: accept, add_plain, update, ignore. |
| `POST /admin/metadata/test` | `{ "provider": "igdb" }` or `"sgdb"`. `204` when the stored key works. |
| `GET /admin/users` · `PATCH` / `DELETE /admin/users/{id}` | `people::AdminUser` list, role change, removal. |
| `POST /admin/users/{id}/reset-link` | `people::ResetLink`, with the link's `path` this once. |
| `GET /admin/reset-links` · `DELETE /admin/reset-links/{id}` | Pending reset links. |
| `GET` / `POST /admin/invites` · `DELETE /admin/invites/{id}` | `auth::Invite`, `auth::CreateInvite`. |
| `GET /admin/sessions` | Every signed-in device. Sign one out with `DELETE /auth/sessions/{id}`. |
| `GET /admin/devices` | `[people::Device]` of everyone. |
| `GET` / `POST /admin/downloads` · `DELETE /admin/downloads/{id}` | The remote download queue. |
| `GET /admin/networking` | `admin::Networking`: lobbies, worlds, relay figures, recent calls. |
| `GET /admin/storage` | `saves::Storage`: Rest storage per account. |
| `POST /admin/news` · `PATCH` / `DELETE /admin/news/{id}` | `community::PublishNews`, `NewsPatch`. |
| `POST /admin/events` · `DELETE /admin/events/{id}` | `community::PlanEvent`. |
| `POST /admin/events/{id}/ready` | Queue the game for every guest who is not ready. Answers with the `ReadyCheck`. |
| `POST /admin/emulators` · `PATCH` / `DELETE /admin/emulators/{id}` | `community::EmulatorInput`. |
| `GET` / `POST /admin/game-servers` · `DELETE /admin/game-servers/{id}` | `community::Registry`, `AddGameServer`. |
| `POST /admin/game-servers/token` | Make a new registry token. |
| `GET /admin/maintenance` | `admin::Maintenance`: newest backup, last update check. |
| `POST /admin/backup` | Back up the database now. |
| `POST /admin/update/check` · `POST /admin/update` | Look for a newer version; install it and restart. |

### First-time setup

Setup happens in the Hearth's own web UI, not in Bonfire. A client only needs to know that
a Hearth answering `setup_required`, or `"setup_complete": false` on `GET /hearth`, is not
ready yet. For completeness: the wizard uses `POST /setup/begin`, `GET /setup/browse`,
`POST /setup/scan` and `POST /setup/complete`, each with the setup code from the console in
an `x-setup-code` header. All four answer `setup_complete` once an admin exists. When setup
moves the Hearth to another port or to HTTPS, the wizard signs the new admin in there with
`POST /auth/handoff` and a token from the setup answer that works once, for two minutes.

### The web UI's cookie

The web UI signs in with `"cookie": true` in the sign-in body. The token then travels in an
`HttpOnly`, `SameSite=Strict` cookie named `hearth_session` and is left out of the answer.
Requests that rely on the cookie and change something must carry an `x-hearth-ui` header,
which other sites cannot add. Clients that send a bearer token need neither.
