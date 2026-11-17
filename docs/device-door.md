# The device door

Not ready. Do not use it on real files yet.

A device door is a third door, for a device such as an e-reader. Think of a side door with a key card: it opens only a few rooms, it reads each card, and one card can be blocked without changing the lock of the house.

- It runs as its own process, from its own folder, with its own `.env`, log, locks and tokens. The door for claude.ai stays as it is.
- One door per app. The app is a name in the settings, such as `reader`.
- One token per app and device. A lost device costs only its own token.
- Only three tools: `list`, `read` and `patch`, and only on the paths that the settings name. Every other tool and every other path is refused with exit code 2, as a closed path is on the other doors.
- No password and no OAuth. A device pairs once with a code of 8 signs, and then sends its token.
- A read cap per device, with a push to the owner when a device reaches it.
- No request and no answer goes into the log.

| Server | Start it with | For | Login |
|---|---|---|---|
| Device | `orgthread serve --http <address> --device` | One app on your own devices | A token per device |

## Set it up

**1. Give the door its own folder.** Copy the program, do not link it: the program reads the `.env` beside its real file, and a link would lead to the `.env` of the other door.

```
mkdir ~/orgthread-reader
cp target/release/orgthread ~/orgthread-reader/
```

**2. Write its `.env`.** For example, for an invented app `reader` that reads books, glossaries and summaries, and keeps one progress file per book:

```
ORGTHREAD_STORAGE=dropbox
ORGTHREAD_DEVICE_APP=reader
ORGTHREAD_DEVICE_READ=/books/,/reading/glossary/,/reading/summaries/
ORGTHREAD_DEVICE_PATCH=/reading/progress-*.md
ORGTHREAD_DEVICE_CAPPED=/books/*/chapter-*.txt
ORGTHREAD_DEVICE_NOTIFY=/home/example/orgthread-reader/push.sh
ORGTHREAD_PUBLIC_URL=https://reader.example.com/another-long-invented-secret
```

Add the Dropbox settings of [settings.md](settings.md), as for the remote door. Do not set `ORGTHREAD_LOGIN_HASH`: a device door with a password refuses to start, as it then shares a folder with the other door. The other way round, `serve --http` without `--device` refuses to start where `ORGTHREAD_DEVICE_READ` or `ORGTHREAD_DEVICE_PATCH` is set.

`ORGTHREAD_PUBLIC_URL` is optional here. With a path in it, everything lives under that path, and any other path gets an empty 404, as on the remote door.

**3. Start it** on its own port, behind the same program for HTTPS as the remote door:

```
~/orgthread-reader/orgthread serve --http 127.0.0.1:8091 --device
```

It stops at once with exit code 2 when a setting is wrong or missing, and names it.

## The paths

The rules of a device door are stricter than the limits of the other doors:

- Each path starts with `/`. A path with no `/` at its start is refused.
- A path has no `//`, no `.` part, no `..` part and no `\`.
- Capitals count. With `/books/` in the rules, `/Books/a.txt` is refused.
- `ORGTHREAD_DEVICE_READ` names folders, with `/` at both ends. A device may read every file in them, at any depth, and list them, one level at a time. It may not patch there.
- `ORGTHREAD_DEVICE_PATCH` names files. A device may read and patch them. `*` stands for any part of one name, and never for a `/`. So `/reading/progress-*.md` opens `/reading/progress-a.md`, and not `/reading/notes.md` or `/reading/old/progress-a.md`. A patch never makes a file: a file that does not exist gives exit code 6 on the read, and the patch is then refused.
- In a local folder, a path that goes through a link is refused. Dropbox has no links.
- A list with one closed folder is refused whole.

A refusal of the door gets one log line with the word `refused`, the path and the device.

## Pair a device

In a terminal on the server, in the folder of the door:

```
./orgthread device pair tablet
```

It shows the code, for example `K7QM-3XPA`, and the time until it works. The code works once, for 5 minutes. A new code for the same name ends the code before it. The code shows only in a terminal: in a pipe, a script or the output of an agent, the command refuses with exit code 2.

The device sends the code as JSON:

```
curl -sS https://reader.example.com/another-long-invented-secret/pair \
  -H 'Content-Type: application/json' -d '{"code": "K7QM-3XPA"}'
```

Small letters, spaces and the dash are fine. The answer:

```
{"token":"<64 signs>","expires":"2027-02-15"}
```

| Status | Answer | When |
|---|---|---|
| 200 | `{"token": "...", "expires": "YYYY-MM-DD"}` | The code was right. The token lasts 90 days to the second, so it ends during that date, in UTC |
| 400 | `{"error": "bad_request"}` | The body has no `code` as text |
| 400 | `{"error": "wrong_code"}` | The code is wrong, used or older than 5 minutes |
| 429 | `{"error": "too_many_tries"}` | 5 wrong codes within 15 minutes. `/pair` stays closed for 15 minutes. The count lives in memory, so a restart clears it |
| 500 | `{"error": "server_error"}` | The file of the tokens could not be read or saved |

A pairing ends the old token of the same device, logs `paired`, and runs `ORGTHREAD_DEVICE_NOTIFY`, so you learn of a pairing you did not make.

## Use the token

The device sends the token in each MCP call to `/mcp`, as one JSON message per POST:

```
curl -sS https://reader.example.com/another-long-invented-secret/mcp \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"jsonrpc": "2.0", "id": 1, "method": "tools/call",
       "params": {"name": "read", "arguments": {"file": "/books/an-invented-book/chapter-01.txt"}}}'
```

A missing, wrong, revoked or old token gets an empty 401, and nothing else does. No answer is a 403. A refused tool, a refused path and the read cap are no HTTP errors: each is a tool result with `isError` set to true and a text that starts with `exit code 2`, as on the other doors. The fields of the tools are those of [mcp-api.md](mcp-api.md), with three changes:

- There is no `who`. The door names the device itself, as `tablet (reader)`, in the log and in the notes of what it read. A `who` that the device sends is not used. So a patch needs a read by the same device first.
- `list` takes `folders` and `fingerprint`, and goes one level down. It takes no `depth` and no `all`.
- `version` is not offered.

## The read cap

Each read of a file of `ORGTHREAD_DEVICE_CAPPED` counts: by default 8 in 60 minutes and 40 in 24 hours for each device, set with `ORGTHREAD_DEVICE_CAP_HOUR` and `ORGTHREAD_DEVICE_CAP_DAY`. With `ORGTHREAD_DEVICE_CAPPED` not set, every read counts. A refused read does not count.

Above the cap, the read is refused with exit code 2 and the time it opens again:

```
exit code 2
orgthread: refused. This device reached its read cap of 8 in 60 minutes. It may read again after 2026-11-17 15:05 UTC.
```

Each refused read logs `capped` with its path. The first one in an hour runs `ORGTHREAD_DEVICE_NOTIFY`. The times of the reads stay in the file of the tokens, so a restart does not reset the cap.

## The push

`ORGTHREAD_DEVICE_NOTIFY` names a program. The door runs it with one line of text as its only argument, does not wait for it, and drops its output. The line names the device and what happened, never a path or a text of a file:

```
orgthread: the device tablet (reader) reached its read cap of 8 in 60 minutes. The door refuses its capped reads until 2026-11-17 15:05 UTC. To end its token: orgthread device revoke tablet
```

For example, a script that sends it to a push service that you run yourself:

```
#!/bin/sh
curl -sS -d "$1" https://push.example.com/an-invented-topic > /dev/null
```

If the program cannot start, the log gets `not-notified`. With no setting, only the log tells.

## See and revoke the devices

```
./orgthread device list
./orgthread device revoke tablet
```

`device list` shows each device with its app, the day it paired, the day its token ends, and its capped reads in the last hour and day, and each code that waits. It shows no token, no code and no hash. `device revoke` ends the token and any waiting code of the device at once, as the door reads the file at each call, and logs `revoked`. A name with no token gives exit code 6.

## What the door keeps

| Where | What |
|---|---|
| `state/devices.json`, or `ORGTHREAD_DEVICES` | The SHA-256 of each token and waiting code, the device, the app, the times, and the times of the capped reads of the last day. Mode 600. Never a raw token |
| `state/writes.log` | One line for each pairing, wrong code, refusal, capped read, revoke, and each patch: the time, the word, the path and the device. Never a request or an answer |
| `state/reads` | The fingerprint of each file that a device read, under its name. Never a text |

With Dropbox, no text of a file goes to the disk: a read passes through the memory of one command and goes to the device. A failed check after a patch can leave the text of the patched file in `.locks`, as on every door, so name only files with no book text in `ORGTHREAD_DEVICE_PATCH`. The door writes nothing about a request to its output, which a server sends to its journal. With a local folder, each patch keeps a copy of the patched file in `ORGTHREAD_VERSIONS`, as on every door.

## T6: the check before the first real chapter

Run it on a test server, with invented text only. Write the result down.

1. Make an invented chapter with a marker word that is in no other file, such as `quendelmorgenrot`, in a test folder of the rules, for example `/books/an-invented-book/chapter-01.txt`.
2. Pair a test device with `device pair` and `/pair`. Pair the same code again: 400.
3. Allowed calls: `tools/list` gives `read`, `patch` and `list` only. `list` of each read folder, `read` of the chapter, of a glossary, of a summary and of a progress file, then a `patch` of the progress file. All work.
4. Every other tool: `write`, `delete`, `search`, `copy`, `move`, `download`, `upload`, `version`. Each gives `exit code 2`, and nothing changes.
5. Path tricks, each with `read`: a `..` part, a `.` part, other capitals, no `/` at the start, a `//`, the folder name with a letter added, a file next to a progress file, a path outside every rule. Each gives `exit code 2`.
6. A `patch` of a file in a read folder: `exit code 2`. A `read` and a `patch` of a progress file that does not exist: exit code 6, then 5, and no new file.
7. With `ORGTHREAD_DEVICE_CAP_HOUR=2` for the test: the third chapter read gives `exit code 2` and the read cap. The log has `capped`, and the push came once.
8. `device revoke` the test device. Its next call gets 401.
9. Search for the marker word, as the user of the door, with sudo where needed. Each search must give 0 hits:

```
grep -rl quendelmorgenrot ~/orgthread-reader
sudo grep -rl --exclude-dir=proc --exclude-dir=sys quendelmorgenrot / 2>/dev/null
journalctl --user -u orgthread-reader.service | grep -c quendelmorgenrot
sudo journalctl | grep -c quendelmorgenrot
```

The test `t6_no_text_of_a_book_reaches_the_disk_the_log_or_the_output` in `tests/device.rs` does steps 1, 3 and 9 for a local folder on each `cargo test`: it reads the chapter, then searches the log, the tokens, the notes, the push and the output of the door for the marker word.
