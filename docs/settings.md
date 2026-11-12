# Settings

This file is made from the list in `src/settings.rs`. A test fails when it is out of date. To make it again, run `UPDATE_SETTINGS_DOC=1 cargo test settings_doc`.

Each setting is a line `ORGTHREAD_<NAME>=<value>`. It comes from the environment, or from the file `.env` beside the program. A name that the environment holds wins over `.env`. Write a value with no quotes, as quotes count as part of it. A line that starts with `#` is a comment.

A wrong value stops every command with code 2, wrong use, and the message names the setting. For example, `ORGTHREAD_UPLOAD_MB=abc` gives `ORGTHREAD_UPLOAD_MB needs a whole number of MB, not abc`. Only `orgthread config`, `--help` and `--version` still run.

## Change the settings

`orgthread config` opens a screen in the terminal, with the settings in groups. It works only in a real terminal, as `orgthread watch` does. No command for a script and no MCP tool changes a setting, so no agent can widen its own limits through orgthread.

| Key | What it does |
|---|---|
| `j`, `k` or the arrows | Choose a setting |
| Enter | Change the value. Enter again keeps it, Esc goes back. An empty value is the default |
| `d` | Back to the default: the line goes from `.env` |
| `s` | Save |
| `?` | Help |
| `q` | Quit. With a change not saved, it asks first |

- A secret shows only as `secret: set` or `secret: not set`. Enter replaces it. The typing shows nothing, not even its length. The old value is never shown, never logged and never listed.
- A setting that the environment holds shows as `from the environment`, with the value in effect, and cannot be changed on the screen.
- A line that names no setting of the list, such as a typo `ORGTHREAD_OWNR`, shows at the bottom as `not used`. Such lines and the comments stay in place.
- `s` checks every value first: a wrong one saves nothing. If `.env` changed on the disk since the screen read it, it saves nothing either. Then it keeps the old file as `.env.before-config-<date>-<time>`, writes the new `.env` in one step, a temporary file and then a rename, and sets its rights to 600, so only the owner can read it. A new setting goes at the end. The log gets one line `configured` per changed setting, by `<owner> (config)`, with the name only, never the value.
- The screen shows the restart command, and does not run it. On a server: `systemctl --user restart orgthread.service`. On a Mac, each command reads `.env` again, and the local MCP server of the desktop app needs a restart of the app.
- The way back is the copy `.env.before-config-<date>-<time>`, by hand. The copies stay until you remove them.

`orgthread config --check` lists every setting with its value and where it comes from, and secrets as `set` or `not set`. It changes nothing and works without a terminal. It ends with code 2 when a value is wrong, else 0.

## The list

The doors: `local folder` is the door on a Mac, which reads a folder on the disk. `Dropbox` is the door on a server, with `ORGTHREAD_STORAGE=dropbox`. `both` is both.

### Folders and logs

| Setting | What it does | A right value | Default | Doors |
|---|---|---|---|---|
| `ORGTHREAD_ROOT` | The folder that all paths live in. | a path, not empty | ~/Library/CloudStorage/Dropbox | local folder |
| `ORGTHREAD_LOCK_DIR` | The locks of orgthread, on this machine only. A path that does not start with / starts at the folder of the program. For a local folder, keep it on the same disk as ORGTHREAD_ROOT. | a path, not empty | .locks beside the program | both |
| `ORGTHREAD_LOG` | The log file: one line per write, refusal and change of a setting, never a text. | a path, not empty | state/writes.log beside the program | both |
| `ORGTHREAD_READS` | The notes of what each name read. | a path, not empty | state/reads beside the program | both |
| `ORGTHREAD_VERSIONS` | A copy of each version written, for 30 days, for the diff screen and the revert of watch. | a path, not empty | state/versions beside the program | local folder |
| `ORGTHREAD_OWNER` | Your name, for the log lines of a revert in watch and of a change in config: <name> (watch), <name> (config). | a name on one line | owner | both |

### Limits

| Setting | What it does | A right value | Default | Doors |
|---|---|---|---|---|
| `ORGTHREAD_ALLOW` | Comma-separated folders or files that a door may touch. | paths, separated by commas | all | both |
| `ORGTHREAD_DENY` | Comma-separated folders or files that a door may not touch. | paths, separated by commas | none | both |
| `ORGTHREAD_READ_ONLY` | Comma-separated folders or files that a door may only read. | paths, separated by commas | none | both |
| `ORGTHREAD_BINARY_ALLOW` | Comma-separated folders where a file that is not text may go, by upload, copy or move. | paths, separated by commas | none | both |
| `ORGTHREAD_DELETE_ALLOW` | Comma-separated folders where delete may remove a file. | paths, separated by commas | none, so every delete is refused | both |
| `ORGTHREAD_MOVE_ALLOW` | Comma-separated folders. When set, move needs both names in them. | paths, separated by commas | not set, so move has only the other limits | both |
| `ORGTHREAD_FIXED_FOLDERS` | Comma-separated folders: no new folder may appear directly inside them, for write, upload, copy and move. Deeper new folders are fine. / stands for the root. | paths, separated by commas | not set, so any new folder is made | both |

### Sizes

| Setting | What it does | A right value | Default | Doors |
|---|---|---|---|---|
| `ORGTHREAD_DOWNLOAD_MB` | The largest download, in MB of 1,000,000 bytes. | a whole number | 250 | both |
| `ORGTHREAD_UPLOAD_MB` | The largest upload, in MB of 1,000,000 bytes. | a whole number | 20 | both |
| `ORGTHREAD_SEARCH_MB` | With Dropbox, the largest zip of a search, in MB. With a local folder, the largest file that a search reads. | a whole number | 25 | both |

### Dropbox

| Setting | What it does | A right value | Default | Doors |
|---|---|---|---|---|
| `ORGTHREAD_STORAGE` | dropbox: use the Dropbox API, with paths from its root, such as /org/a.md. It keeps no copies on the disk. local: a folder on this machine. | dropbox or local | local | both |
| `ORGTHREAD_DROPBOX_APP_KEY` | The app key. With ORGTHREAD_DROPBOX_REFRESH and ORGTHREAD_DROPBOX_APP_SECRET, it gets a fresh token for each command. | text on one line | none | Dropbox |
| `ORGTHREAD_DROPBOX_API` | The address of the Dropbox API. Change it only for a test. | an address that starts with http:// or https:// | https://api.dropboxapi.com | Dropbox |
| `ORGTHREAD_DROPBOX_CONTENT` | The address of the Dropbox content API. Change it only for a test. | an address that starts with http:// or https:// | https://content.dropboxapi.com | Dropbox |

### Server

| Setting | What it does | A right value | Default | Doors |
|---|---|---|---|---|
| `ORGTHREAD_PUBLIC_URL` | The public address of serve --http, with its secret path. | an address that starts with https:// | none, required by serve --http | both |
| `ORGTHREAD_LOGINS` | The logins of a door over a restart: hashes of its tokens, no raw token and no text of a note. | a path, not empty | state/logins.json beside the program | both |
| `ORGTHREAD_LISTEN_ANYWHERE` | yes: let serve --http listen on an address that others can reach, with no encryption. | yes or no | no: only 127.0.0.1, [::1] and localhost | both |

### Secrets

| Setting | What it does | A right value | Default | Doors |
|---|---|---|---|---|
| `ORGTHREAD_DROPBOX_TOKEN` | A Dropbox token. With none, ORGTHREAD_DROPBOX_REFRESH, ORGTHREAD_DROPBOX_APP_KEY and ORGTHREAD_DROPBOX_APP_SECRET get a fresh one for each command. | text on one line | none | Dropbox |
| `ORGTHREAD_DROPBOX_REFRESH` | The refresh token of the Dropbox app. | text on one line | none | Dropbox |
| `ORGTHREAD_DROPBOX_APP_SECRET` | The app secret of the Dropbox app. | text on one line | none | Dropbox |
| `ORGTHREAD_LOGIN_HASH` | The hash of the password of serve --http, from orgthread password-hash. | the 64 signs of orgthread password-hash | none | both |
