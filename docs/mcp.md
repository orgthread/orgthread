# The MCP servers

Not ready. Do not use them on real files yet.

MCP is a standard way for a chat to call tools. The program has two MCP servers, for a chat that has no shell. Both offer the same nine tools: `read`, `write`, `patch`, `delete`, `copy`, `move`, `list`, `search` and `version`. The remote server also offers `download` and `upload`, for files that are not text, through one-time links.

| Server | Start it with | For | Login |
|---|---|---|---|
| Local | `orgthread serve` | A client on the same machine, such as Claude Code | None |
| Remote | `orgthread serve --http <address>` | A chat in the cloud, such as claude.ai | One password |

Each tool call starts the command itself: `orgthread read`, `orgthread write`, `orgthread patch`, `orgthread delete`, `orgthread copy` or `orgthread move` for each pair, `orgthread list`, `orgthread search`, `orgthread download` or `orgthread upload`. So the rules, the log and the refusals are the same as in the shell. Only `version` is answered by the server itself, as it reads no file. A chat never deals with a lock.

The tools, their refusals and examples are in [mcp-api.md](mcp-api.md).

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

### Links for files that are not text

`download` and `upload` give a link for each file, at `https://<host>/t/<secret>`. It is on the host of the public address, with no path, because a chat shows each command it runs: a link must give nothing of the secret path away. A link that was used, or never existed, gets an empty 404, as any wrong address does. The code sandbox of the chat uses it with `curl`, so the bytes go between the sandbox and this server, and never through the chat. A link works once and must start within 5 minutes, so a chat can ask for 25 links and run them one after the other. Once it starts, a transfer may take as long as it needs. The links live in memory only, so a restart ends them all. They work without a login, which is why they are short-lived, and they never go into the log.

The sandbox of claude.ai reaches only the hosts on its list. Add the host of the public address on claude.ai, under Settings, Capabilities, Allowed websites. For the public address `https://notes.example.com/a-long-invented-secret`, the host is `notes.example.com`: no `https://` and no path, so the secret stays out of the list. Without it, `curl` in the sandbox gets `CONNECT tunnel failed, response 403`. That answer comes from the proxy of claude.ai, not from this server, so the log of this server shows nothing.

A file that is not text may be uploaded only into the folders of `ORGTHREAD_BINARY_ALLOW`. With that setting empty, the default, no such upload works.

## Settings

Each setting comes from the environment. A file `.env` beside the program, with lines `ORGTHREAD_NAME=value`, fills in each setting that the environment does not hold. Write a value with no quotes, as quotes count as part of it.

Every setting, with its default and the doors that use it, is in [settings.md](settings.md). `orgthread config` changes them on a screen in a terminal, and `orgthread config --check` lists them. A wrong value stops every command and tool with code 2, and names the setting.

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
