# Hearth

Hearth is the server for Bonfire, a private game launcher for a group of friends. You run
one Hearth at home. It keeps your games, your friends' accounts, their saves and playtime,
and it lets them play together without Steam: lobbies, invites, achievements and a relay for
when a direct connection fails.

It is one program with nothing to install around it. There is no config file: you start it,
open the address it prints, and set everything up in the browser.

## Start your Hearth

1. **Get the program.** Download the file for your machine from the
   [releases page](https://github.com/JanRi3D/Hearth/releases) and put it in a folder of its
   own, for example `C:\Hearth` or `/opt/hearth`. Or [build it yourself](#build-from-source).

   | Machine | File |
   |---|---|
   | Windows | `hearth-x86_64-pc-windows-msvc.exe` |
   | Linux | `hearth-x86_64-unknown-linux-gnu` |
   | Linux on ARM, such as a Raspberry Pi | `hearth-aarch64-unknown-linux-gnu` |
   | macOS, Apple silicon | `hearth-aarch64-apple-darwin` |

   On Linux and macOS, make it runnable once: `chmod +x hearth-*`.

2. **Start it.** Double-click it, or run it in a terminal. It prints something like this:

   ```
   Hearth 0.1.0
     Data folder   C:\Hearth\data
     Setup code    K7QM-2XWD

   This Hearth has no owner yet. Open http://192.168.1.20:7300 in a browser and enter the setup code.
   ```

   On a machine with a desktop, the browser opens on its own.

3. **Follow the five steps in the browser.** You enter the setup code, make your admin
   account, name the Hearth and say how friends reach it, point it at the folders that hold
   your games, and switch on the extras you want. The last step shows an invite code for your
   first friend.

That is all. From here on, everything is on the admin pages, and each setting takes effect
when you change it. You do not need the terminal again.

### What your friends need

They install Bonfire, add your Hearth by its address, and enter an invite code. You make
invite codes under **People and invites**.

### Reaching your Hearth

| Port | For | Change it in |
|---|---|---|
| 7300 TCP | The admin pages and everything Bonfire does | Settings, Address and joining |
| 7301 UDP | The Bonfire relay, and finding the Hearth on your network | Settings, Playing together |

If friends connect from outside your home, forward both ports on your router to the machine
that runs Hearth, and enter your public address or domain as the Hearth's address.

**Secure connection** has three choices:

- **Self-signed certificate** is the default and needs nothing from you. Browsers show a
  warning the first time, which you accept. Bonfire remembers the certificate and refuses
  any other.
- **Let's Encrypt** gets a certificate browsers trust. The address must be a domain that
  points at your home, and port 443 from outside must reach the Hearth's port. Until the
  certificate arrives, the Hearth keeps using its own.
- **Behind my own proxy** makes the Hearth speak plain HTTP, for when Caddy, nginx or
  Traefik in front of it adds HTTPS. The proxy must pass WebSocket connections through and
  must not limit the size of uploads.

## Your games

Each entry directly inside a games folder is one game: a folder with the game ready to run,
or a single `.zip`, `.7z`, `.rar`, `.exe` or `.iso`.

```
D:\Games
├── Factorio            a folder, ready to run
├── Stardew Valley 1.6.15
├── Tunic_win_1.0.2.zip
└── Tyrian 2000 (1999).zip
```

Hearth only reads these folders. It never changes or moves your files.

- New entries are picked up on their own, or when you press **Rescan**. You can also
  **Upload** a file from the Library page.
- With an IGDB key, each game is matched by its name and gets a description, year, genre and
  cover. A match the Hearth is unsure about waits under **Metadata review** for your
  decision. Without a key, games are added under the name of their file. The keys are free;
  Settings says where to get them.
- An entry that looks like a newer version of a game you already have, such as
  `Factorio 2.0.16` next to `Factorio`, also waits in Metadata review, where you add it as an
  update. Players then download only the files that changed. The old version stays
  available, so a group can stay on it until everyone is ready.
- In the game editor you set how a game starts, which installers it needs first, and
  whether Bonfire networking stands in for Steam.

Emulators go into the emulators folder, one folder each, and are added on the **Emulators**
page. A game that names an emulator ships only its own files.

## The data folder

Everything the Hearth knows is in one folder, `data` next to the program. To keep it
somewhere else, start the program with the one option it has:

```
hearth --data /srv/hearth
```

| Inside | |
|---|---|
| `hearth.db` | Accounts, settings, the library, playtime, chat, news. |
| `backups` | A copy of the database every night, the last 14 kept. |
| `saves`, `worlds` | Rested saves and shared worlds. |
| `art` | Covers. |
| `emulators` | The default emulators folder. |
| `tls` | The Hearth's certificate. |
| `logs` | One file per day, two weeks kept. |

**To move a Hearth** to another machine, stop it, copy the data folder, and start the
program there with `--data` pointing at the copy.

**To go back to a backup**, stop the Hearth, replace `hearth.db` with a file from `backups`
(delete `hearth.db-wal` and `hearth.db-shm` if they are there), and start it again. Backups
hold the database. Saves and worlds are plain files: copy the data folder to keep those safe too.

## When something goes wrong

**You forgot the admin password.** On the machine that runs Hearth:

```
hearth reset-password mika
```

It prints a new password and signs that account out everywhere. Add `--data <folder>` if
you started the Hearth with it. For anyone else, an admin makes a reset link under
**People and invites**.

**A friend cannot connect.** Check that the Overview shows the API and the relay as up,
that both ports are forwarded, and that the address under Settings is the one your friend
types. `https://<address>:7300/api/v1/health` answers `{"status":"ok"}` when the Hearth is
reachable.

**You want to know what happened.** The Overview lists recent activity. The files in
`data/logs` hold everything, one JSON object per line.

## Updates

**Settings, Maintenance, Check for updates** looks for a newer version and installs it with
one more click. The Hearth restarts on its own, and downloads resume afterwards. From a
terminal, `hearth update` does the same for a Hearth that is stopped. Either way the
download is checked against the release's checksums before anything is replaced.

## Keeping it running

Hearth runs until you stop it with Ctrl+C. To have it start with the machine:

- **Windows:** Task Scheduler, a task that runs `hearth.exe` "At startup", with "Run whether
  user is logged on or not".
- **Linux:** a systemd unit.

  ```ini
  [Unit]
  Description=Hearth
  After=network-online.target

  [Service]
  ExecStart=/opt/hearth/hearth --data /srv/hearth
  Restart=always
  User=hearth

  [Install]
  WantedBy=multi-user.target
  ```

  `Restart=always` matters: after an update from the admin pages, the Hearth starts its new
  version itself, and systemd covers the case where that fails.

## Build from source

You need [Rust](https://rustup.rs) and nothing else. The repository pins the toolchain and
the WebAssembly target the web UI is built for, so the first build fetches both.

```
git clone https://github.com/JanRi3D/Hearth
cd Hearth
cargo build --release
```

The program is `target/release/hearth`. The web UI, its assets and the database migrations
are inside it.

```
cargo test --workspace
cargo clippy --workspace --all-targets
```

The tests start real Hearths on temporary data folders and talk to them the way Bonfire
does: over HTTP, WebSocket, WebDAV and UDP.

| Crate | |
|---|---|
| `crates/hearth` | The server and the `hearth` command. |
| `crates/hearth-api` | The types of every request and answer. Bonfire depends on this crate. |
| `crates/hearth-web` | The admin pages, written in Rust with Leptos and compiled to WebAssembly by the server's build. |

## Building Bonfire against it

[docs/API.md](docs/API.md) is the reference: accounts, the library and downloads, play
data, the live connection, lobbies and the relay, cloud saves over WebDAV, and the calls the
`steam_api64.dll` replacement maps onto.
