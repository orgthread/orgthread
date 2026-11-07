# The MCP servers

Not ready. Do not use them on real files yet.

MCP is a standard way for a chat to call tools. The program has two MCP servers, for a chat that has no shell. Both offer the same two tools, `read` and `write`.

| Server | Start it with | For | Login |
|---|---|---|---|
| Local | `orgthread serve` | A client on the same machine, such as Claude Code | None |
| Remote | `orgthread serve --http <address>` | A chat in the cloud, such as claude.ai | One password |

Each tool call starts the command itself, `orgthread read` or `orgthread write`. So the rules, the log and the refusals are the same as in the shell. A chat never deals with a lock.

## Build it

```
cargo build --release
```

The program is `target/release/orgthread`. It keeps its settings, locks and log beside itself, so copy it to a folder of its own first.

Tell it where your files are, in a file `.env` beside the program:

```
ORGTHREAD_ROOT=/full/path/to/your/notes
```

## The local server

The client starts the server by itself. It speaks over standard input and output, one JSON message per line, and it ends when its input ends.

To add it to Claude Code:

```
claude mcp add orgthread -- /full/path/to/orgthread serve
```

This adds it for you alone, in the current project. With `--scope project`, Claude Code writes it into a file `.mcp.json`, in the form that other clients often take as well:

```json
{
  "mcpServers": {
    "orgthread": {
      "type": "stdio",
      "command": "/full/path/to/orgthread",
      "args": ["serve"],
      "env": {}
    }
  }
}
```

## The remote server

This server speaks plain HTTP, so traffic from outside must reach it through HTTPS. A separate program on the same machine handles it, and passes each request to this server over plain HTTP. So give this server an address on its own machine. That program must pass each path on unchanged.

`orgthread serve --http` accepts `127.0.0.1`, `[::1]` and `localhost`, each with a port. Any other address, such as `0.0.0.0`, `[::]`, `:8080` or a host name, stops it at once with exit code 2. Without encryption, a password and your notes could cross the network in plain sight. To allow such an address on purpose, set `ORGTHREAD_LISTEN_ANYWHERE=yes`. Do this only on a network you trust, or with HTTPS in front.

**1. Make the hash of a password.** Choose a long password, as the hash is one plain SHA-256:

```
printf '%s' 'an invented long password' | orgthread password-hash
```

**2. Add two settings to `.env`.** The address must start with `https://`, and the hash must have 64 signs:

```
ORGTHREAD_PUBLIC_URL=https://notes.example.com/a-long-invented-secret
ORGTHREAD_LOGIN_HASH=the hash from step 1
```

The path in the address is optional. With one, everything lives under it, and any other path gets a 404. So a stranger without it finds no server and cannot use up the password tries.

**3. Start it:**

```
orgthread serve --http 127.0.0.1:8080
```

Without the two settings, it stops at once with exit code 2.

**4. Add it to claude.ai.** Open the connectors in the settings and add a custom connector. Its address is the public address with `/mcp` at the end:

```
https://notes.example.com/a-long-invented-secret/mcp
```

A page then asks for the password, with the button "Allow Claude to use orgthread".

The login sends a browser back to one place only: `https://claude.ai/api/mcp/auth_callback`. A client that names any other place is refused when it registers, also one on its own machine. So for Claude Code, use the local server.

Five wrong passwords within 15 minutes close the login for up to 15 minutes, even for the right password. The count lives in memory, so a restart clears it.

Each POST to `/mcp` gets one JSON answer. There is no session and no stream. A request may hold up to 64 MiB.

## The tools

| Tool | Field | What it holds |
|---|---|---|
| `read` | `file` | The full path of the file |
| | `who` | The name of this chat, on one line, the same one each time |
| `write` | `file` | The full path of the file |
| | `who` | The name of this chat, on one line, the same one each time |
| | `text` | The whole new text |

All fields are required. With the local folder, `file` must lie inside `ORGTHREAD_ROOT`. With Dropbox, it starts at the root of the Dropbox: `/org/a.md`.

`read` gives the exact text of the file, and notes what this name read.

`write` replaces the whole file. To change a file, read it first with the same name. A new file needs no read. After a write, the same name may write again with no new read. A write answers like this:

```
written: notes/a.md, 4 bytes
```

The path in the answer is the path from the root.

## Refusals

A refusal is a tool result with `isError` set to true. Its text starts with the exit code, followed by what the command said:

```
exit code 4
orgthread: refused, nothing written. notes/a.md changed since you read it. Read it again and build the change from that text.
```

| Code | Meaning | What to do |
|---|---|---|
| 1 | Failed, the file is unchanged | Read the reason |
| 2 | Wrong use, or not text: an empty text, a NUL sign, a path outside the root, a path the limits forbid, a missing setting | Read the reason |
| 3 | Busy: another writer holds the lock | Try again in a moment |
| 4 | Changed since this name read it | Read it again and build the change from that text |
| 5 | Exists, and this name has not read it | Read it first |
| 6 | Missing: there is no such file, or it is gone since the read | Look for the file |
| 7 | The check after the write failed, and the old state is back | Read the reason |
| 8 | Written, but the log line failed | Look at the log file |
| 9 | The check after the write failed, and the old state is NOT back | Look at the file now |
| 10 | Written, but the clean-up did not finish, or another writer may have been at work | Read the message. If it names another writer, check the file. If not, there is nothing to do: the file is free again within 15 minutes |

So `isError` does not always mean that nothing changed. With 8 and 10, the text is written. With 9, the file may be damaged.

A missing field or an unknown tool is no refusal. It gets a JSON-RPC error with the code -32602, and the server goes on.

## Settings

Each setting comes from the environment. A file `.env` beside the program, with lines `ORGTHREAD_NAME=value`, fills in each setting that the environment does not hold. Write a value with no quotes, as quotes count as part of it.

| Setting | What it does | Default |
|---|---|---|
| `ORGTHREAD_ROOT` | The folder all paths live in | `~/Library/CloudStorage/Dropbox` |
| `ORGTHREAD_ALLOW` | Comma-separated folders or files that the tools may touch | All |
| `ORGTHREAD_DENY` | Folders or files that the tools may not touch | None |
| `ORGTHREAD_READ_ONLY` | Folders or files that the tools may only read | None |
| `ORGTHREAD_PUBLIC_URL` | The public address of the remote server | None, required by it |
| `ORGTHREAD_LOGIN_HASH` | The hash of its password | None, required by it |
| `ORGTHREAD_LOGINS` | The file of the logins | `state/logins.json` beside the program |
| `ORGTHREAD_LISTEN_ANYWHERE` | `yes` lets the remote server listen on an address that others can reach, with no encryption | Only `127.0.0.1`, `[::1]` and `localhost` |
| `ORGTHREAD_LOCK_DIR` | The locks. For a local folder, keep them on the same disk as the root | `.locks` beside the program |
| `ORGTHREAD_LOG` | The log file | `state/writes.log` beside the program |
| `ORGTHREAD_READS` | The notes of what each name read | `state/reads` beside the program |
| `ORGTHREAD_VERSIONS` | A copy of each version written, for 30 days, for the diff screen | `state/versions` beside the program |
| `ORGTHREAD_STORAGE` | `dropbox` uses the Dropbox API in place of a folder. It keeps no copies | A local folder |
| `ORGTHREAD_DROPBOX_TOKEN` | The Dropbox token | None |
| `ORGTHREAD_DROPBOX_REFRESH`, `ORGTHREAD_DROPBOX_APP_KEY`, `ORGTHREAD_DROPBOX_APP_SECRET` | In place of a token: these three get a fresh short-lived token for each command | None |

The limits use paths from the root, such as `notes/drafts`, and capitals do not count. They guard against a confused chat, not against a thief. Messages call a server "this door".

The Dropbox calls use the program `curl`, so it must be on the machine.

## What a login keeps

The remote server keeps its logins in `state/logins.json`, so that they outlive a restart. Only its owner may read that file (mode 600).

| Field | What it holds |
|---|---|
| `owner` | A hash that ties the file to this address and this password |
| `clients` | The id of each client that registered, at most 100 |
| `access` | The SHA-256 of each live access token, and when it ends |
| `refresh` | The SHA-256 of each live refresh token, its client, and when it ends |

An access token lasts 1 hour. A refresh token lasts 30 days and works once, as each refresh gives a new pair.

The file holds no raw token, no password and no text of a note. A login code lasts 60 seconds and lives in memory only.

To end every login at once, do one of these:

- Stop the server, delete the file, and start it again.
- Change the password.
- Change the public address.

With a file that is missing, broken or made for another address or password, chats log in again. Nothing else breaks. If the file cannot be saved, the server removes it, so that a used token can never come back after a restart.
