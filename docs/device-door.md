# The device door

Not ready. Do not use it on real files yet.

A device door is a third door, for a device such as an e-reader. Think of a side door with a key card: it opens only a few rooms, it reads each card, and one card can be blocked without changing the lock of the house.

- It runs as its own process, from its own folder, with its own `.env`, log, locks and tokens. The door for claude.ai stays as it is.
- One door per app. The app is a name in the settings, such as `reader`.
- One token per app and device. A lost device costs only its own token.
- Only three tools: `list`, `read` and `patch`, and only on the paths that the settings name. Every other tool and every other path is refused with exit code 2, as a closed path is on the other doors.
- A door for a sync can also turn on `download` and `upload` of exact bytes, and `trash`, a move into a trash folder. They are off by default. Only the folders of their own settings open them: see [Sync a folder](#sync-a-folder).
- A folder can be open to `list` only: the device sees the names, sizes, times and fingerprints of the files, never their text.
- No password and no OAuth. A device pairs once with a code of 8 signs, and then sends its token.
- A read cap per device, with a push to the owner when a device reaches it. A door for a sync also has a cap in MB per device and day.
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

**2. Write its `.env`.** For example, for an invented app `reader` that reads books, glossaries and summaries, and keeps one progress file per book. Each book is a folder in `/books/`, and `/books/` itself also holds files that no device may read, such as the whole books:

```
ORGTHREAD_STORAGE=dropbox
ORGTHREAD_DEVICE_APP=reader
ORGTHREAD_DEVICE_READ=/books/*/,/reading/glossary/,/reading/summaries/
ORGTHREAD_DEVICE_PATCH=/reading/progress-*.md
ORGTHREAD_DEVICE_CAPPED=/books/*/chapter-*.txt
ORGTHREAD_DEVICE_NOTIFY=/home/example/orgthread-reader/push.sh
ORGTHREAD_PUBLIC_URL=https://reader.example.com/another-long-invented-secret
```

Another example, for an invented app `lessons` that only checks which files of `/lessons/` it has: it may list `/lessons/` with fingerprints, and read nothing.

```
ORGTHREAD_STORAGE=dropbox
ORGTHREAD_DEVICE_APP=lessons
ORGTHREAD_DEVICE_LIST=/lessons/
ORGTHREAD_DEVICE_NOTIFY=/home/example/orgthread-lessons/push.sh
ORGTHREAD_PUBLIC_URL=https://lessons.example.com/a-third-long-invented-secret
```

A third example, for an invented app `desk` that keeps `/notes/` the same on the device and in the storage. It lists the folder with fingerprints, and moves whole files:

```
ORGTHREAD_STORAGE=dropbox
ORGTHREAD_DEVICE_APP=desk
ORGTHREAD_DEVICE_LIST=/notes/
ORGTHREAD_DEVICE_DOWNLOAD=/notes/
ORGTHREAD_DEVICE_UPLOAD=/notes/
ORGTHREAD_DEVICE_TRASH=/notes/
ORGTHREAD_BINARY_ALLOW=/notes/
ORGTHREAD_DEVICE_NOTIFY=/home/example/orgthread-desk/push.sh
ORGTHREAD_PUBLIC_URL=https://desk.example.com/a-fourth-long-invented-secret
```

A device door needs at least one of `ORGTHREAD_DEVICE_READ`, `ORGTHREAD_DEVICE_PATCH`, `ORGTHREAD_DEVICE_LIST`, `ORGTHREAD_DEVICE_DOWNLOAD`, `ORGTHREAD_DEVICE_UPLOAD` and `ORGTHREAD_DEVICE_TRASH`, and any one of them is enough.

Add the Dropbox settings of [settings.md](settings.md), as for the remote door. Do not set `ORGTHREAD_LOGIN_HASH`: a device door with a password refuses to start, as it then shares a folder with the other door. The other way round, `serve --http` without `--device` refuses to start where any of these six settings is set.

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
- In `ORGTHREAD_DEVICE_READ`, `*` stands for any part of one folder name, and never for a `/`. So `/books/*/` opens `/books/an-invented-book/` and all it holds, but no file directly in `/books/`: `/books/an-invented-book.epub` is refused. A list of `/books/`, the folder right above the `*`, shows only the names of its folders, never a file or its name. A list of any other folder above, such as `/`, is refused.
- `ORGTHREAD_DEVICE_LIST` names folders in the form of `ORGTHREAD_DEVICE_READ`, with `/` at both ends and `*` for any part of one folder name. A device may list them, and every folder in them, one level at a time, with `fingerprint`. A `read` or a `patch` of any file in them is refused with exit code 2, and the log gets a `refused` line with the path and the device, never a text of the file. So with `/lessons/`, a list of `/lessons/` and of `/lessons/archive/` works, and a read of `/lessons/lesson-1.pdf` is refused.
- Where a list folder meets a read folder or a patch file, the list folder wins: a file in it is closed to `read` and `patch`, also when `ORGTHREAD_DEVICE_READ` or `ORGTHREAD_DEVICE_PATCH` names it. The door checks this at each call, so no overlap of two rules with a `*` can slip through, and a mistake in the settings closes a file, never opens it.
- `ORGTHREAD_DEVICE_PATCH` names files. A device may read and patch them. `*` stands for any part of one name, and never for a `/`. So `/reading/progress-*.md` opens `/reading/progress-a.md`, and not `/reading/notes.md` or `/reading/old/progress-a.md`. A patch never makes a file: a file that does not exist gives exit code 6 on the read, and the patch is then refused.
- `ORGTHREAD_DEVICE_DOWNLOAD`, `ORGTHREAD_DEVICE_UPLOAD` and `ORGTHREAD_DEVICE_TRASH` name folders in the form of `ORGTHREAD_DEVICE_READ`. Each opens every file in its folders, at any depth, for its one tool only. A folder rule fits a sync better than a file rule: a sync needs every file of a folder and of the folders in it, and a `*` in a file rule never covers a `/`. They do not open a `list`, a `read` or a `patch`: give the folder to `ORGTHREAD_DEVICE_LIST` too. A list folder does not close them.
- No path with a part `.trash` is open to `download`, `upload` or `trash`, whatever the rules say, also with other capitals, such as `.Trash`. The trash belongs to the owner.
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
- `list` takes `folders` and `fingerprint`, and goes one level down. It takes no `depth` and no `all`. With `fingerprint` set to `true`, each file shows its fingerprint before its name: the content hash that Dropbox reports, the same as `orgthread fingerprint` gives.
- `version` is not offered.

## A door that only lists

With the `lessons` example above, the device sends:

```
{"jsonrpc": "2.0", "id": 1, "method": "tools/call",
 "params": {"name": "list", "arguments": {"folders": ["/lessons/"], "fingerprint": true}}}
```

The text of the answer has one line per entry: the size in bytes, the time, the fingerprint and the name. A folder shows `-` for its size, time and fingerprint, and its name ends with `/`. For example, with invented files:

```
/lessons/:
          -  -                     -                                                                 archive/
         13  2026-11-21 Sat 19:24  7a2229db9c520d7b3617d2089d56599b43001c12949a56f4c106562501c27a71  lesson-1.pdf
         13  2026-11-21 Sat 19:24  e3f108bb948e6e32a77b9389d8cb0ad1d2572a9ba15e3527dda071ee2555ff3c  lesson-1.pdf.mark
```

The device compares each fingerprint with the content hash of its own copy: the same hash, the same file. A read of `/lessons/lesson-1.pdf` gives:

```
exit code 2
orgthread: /lessons/lesson-1.pdf lies in a folder that this door may only list, not read or patch
```

## The read cap

Each read of a file of `ORGTHREAD_DEVICE_CAPPED` counts: by default 8 in 60 minutes and 40 in 24 hours for each device, set with `ORGTHREAD_DEVICE_CAP_HOUR` and `ORGTHREAD_DEVICE_CAP_DAY`. With `ORGTHREAD_DEVICE_CAPPED` not set, every read counts. A refused read does not count. A list never counts, with or without `fingerprint`.

Above the cap, the read is refused with exit code 2 and the time it opens again:

```
exit code 2
orgthread: refused. This device reached its read cap of 8 in 60 minutes. It may read again after 2026-11-17 15:05 UTC.
```

Each refused read logs `capped` with its path. The first one in an hour runs `ORGTHREAD_DEVICE_NOTIFY`. The times of the reads stay in the file of the tokens, so a restart does not reset the cap.

## Sync a folder

With the `desk` example above, a device keeps `/notes/` the same on both sides. It lists the folder with `fingerprint`, as in [A door that only lists](#a-door-that-only-lists), compares each fingerprint with its own copy, then moves each file that differs.

A file can be larger than a message of `/mcp`, which takes at most 1 MiB. So `download` and `upload` are not MCP tools: they are two places of their own, with the raw bytes in the body. `trash` moves no bytes, so it is an MCP tool. A `tools/call` of `download` or `upload` is refused with exit code 2.

Each place needs the token in `Authorization: Bearer <token>`. A missing or wrong token gets an empty 401, as at `/mcp`. Another method gets an empty 405.

### The answers

- Success: status 200.
- A refusal or a failure: status 422, with a JSON body `{"exit": N, "text": "exit code N\norgthread: ..."}`. The exit code and the text are those of a refused tool, see [exit-codes.md](exit-codes.md). Exit code 2 is a refusal of the door or wrong use. A refusal of the door logs `refused` with the path and the device.

### Download

```
GET /<secret path>/download?file=<path>
Authorization: Bearer <token>
```

`<path>` is the full path from the root, such as `/notes/sub/a.note`, in the form of a query value: UTF-8, with `%XX` for each sign that is not a letter, a digit, `-`, `_`, `.` or `*`. `URLEncoder.encode(path, "UTF-8")` of Java gives this form. A `+` stands for a space.

The answer 200:

```
HTTP/1.1 200 OK
Content-Type: application/octet-stream
X-Orgthread-Fingerprint: <64 signs>
X-Orgthread-Exit: 0
Content-Length: <size>

<the exact bytes>
```

The fingerprint is the one of these bytes, the same as `list` with `fingerprint` shows. `X-Orgthread-Exit` is 0, or 8 when the bytes are whole but the log line failed. The door runs `orgthread download`, so the log gets `downloaded`, and `state/reads` notes the device.

For example, with an invented token in `$TOKEN`:

```
curl -sS -o a.note -D - -H "Authorization: Bearer $TOKEN" \
  'https://desk.example.com/a-fourth-long-invented-secret/download?file=%2Fnotes%2Fa.note'
```

### Upload

```
PUT /<secret path>/upload?file=<path>&fingerprint=<the fingerprint the device last saw>
Authorization: Bearer <token>
Content-Length: <size>

<the exact bytes>
```

- `fingerprint` is the fingerprint of the version that the device last saw, from a `list` or a download. The upload replaces only that version. If the file changed since, the answer is 422 with exit code 4, and nothing is written. If the file is gone since, it is exit code 6.
- With no `fingerprint`, the file must be new. If a file is at that path, the answer is 422 with exit code 5, and nothing is written. So an upload never goes over a file that the device did not see.
- `Content-Length` is required. A chunked body is refused with exit code 2. An empty file is refused.
- `Expect: 100-continue` works: the door then sends `100 Continue` only after the token, the rules and the limits allow the upload.
- A file that is not text, such as a `.note`, also needs `ORGTHREAD_BINARY_ALLOW` for its folder, as every upload does.
- The door runs `orgthread upload --expect <fingerprint>`, or `--new`. The write goes through a temporary file and a rename in a local folder, and through the Dropbox API with the same checks on a server, as every upload does.

The answer 200:

```
{"exit": 0, "fingerprint": "<64 signs>", "size": 3000, "text": "written: notes/a.note, 3000 bytes\n"}
```

`fingerprint` is the one of the new file. The device keeps it as the version it saw. `exit` is 0, or 8 or 10 when the file is written but a log line or the clean-up failed.

When the door refuses before the body, it answers first and then drops at most the bytes of the file limit. A device sends the size before the bytes, so it checks a file against the limit before it sends it.

From Java, with `HttpsURLConnection`, for example:

```
conn.setRequestMethod("PUT");
conn.setDoOutput(true);
conn.setFixedLengthStreamingMode(bytes.length);
conn.setRequestProperty("Authorization", "Bearer " + token);
```

`setFixedLengthStreamingMode` sends a `Content-Length` and no chunks.

### Trash

```
{"jsonrpc": "2.0", "id": 1, "method": "tools/call",
 "params": {"name": "trash", "arguments": {"file": "/notes/sub/b.note", "fingerprint": "<64 signs>"}}}
```

It moves the file to `.trash/YYYY-MM-DD/` inside the folder of the rule, with the rest of its path, on the day in UTC. So with the rule `/notes/`, `/notes/sub/b.note` goes to `/notes/.trash/2026-11-25/sub/b.note`. It never goes over a file: when that name is taken, it tries `b (2).note`, then `b (3).note`, up to `b (20).note`. Missing folders are made.

- `fingerprint` is required: only that version moves. If the file changed since, the result is exit code 4, and nothing moves.
- The door runs `orgthread move`, so the log gets `moved`, and the text of the result names both paths, for example `moved: notes/sub/b.note -> notes/.trash/2026-11-25/sub/b (2).note, 3000 bytes, <fingerprint>`.
- `tools/list` shows `trash` only where `ORGTHREAD_DEVICE_TRASH` is set. Without it, a call of `trash` is refused with exit code 2.
- `ORGTHREAD_MOVE_ALLOW`, `ORGTHREAD_FIXED_FOLDERS` and `ORGTHREAD_BINARY_ALLOW` still count, as for every move. So a file that is not text goes to the trash only where `ORGTHREAD_BINARY_ALLOW` covers the trash folder, as `/notes/` does in the example. Do not name the folder itself in `ORGTHREAD_FIXED_FOLDERS`, or the first `.trash` folder is refused.

### The limits

- One file has at most `ORGTHREAD_DEVICE_FILE_MB`, 20 MB by default. A larger file is refused with exit code 2, and no byte moves. `ORGTHREAD_DOWNLOAD_MB` and `ORGTHREAD_UPLOAD_MB` still count as well, so keep them at least as high.
- Downloads and uploads of one device count together, at most `ORGTHREAD_DEVICE_CAP_MB_DAY` in 24 hours, 200 MB by default. A transfer counts when the door allows it, before the bytes move, with the size of the file. A transfer that the door refuses does not count. One that the command refuses after that still counts, such as an upload with a stale fingerprint, as its bytes did move. They do not count for the read cap, and reads do not count here.
- Above the cap, the transfer is refused with exit code 2 and the time it opens again:

```
exit code 2
orgthread: refused. This device reached its cap of 200 MB in 24 hours for downloads and uploads. It may move files again after 2026-11-26 09:12 UTC.
```

Each refusal logs `capped` with its path. The first one in an hour runs `ORGTHREAD_DEVICE_NOTIFY`. `device list` shows the transfers of the last 24 hours of each device.

The read cap keeps its own settings, 8 and 40 by default. A desk door that also reads text files can raise it in its own `.env`, for example `ORGTHREAD_DEVICE_CAP_HOUR=16` and `ORGTHREAD_DEVICE_CAP_DAY=80`.

## The push

`ORGTHREAD_DEVICE_NOTIFY` names a program. The door runs it with one line of text as its only argument, does not wait for it, and drops its output. The line names the device and what happened, never a path or a text of a file. It comes at a pairing, at the read cap and at the cap in MB, for example:

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
| `state/devices.json`, or `ORGTHREAD_DEVICES` | The SHA-256 of each token and waiting code, the device, the app, the times, the times of the capped reads of the last day, and the time and size of each download and upload of the last day. Mode 600. Never a raw token |
| `state/writes.log` | One line for each pairing, wrong code, refusal, capped read or transfer, revoke, and each patch, download, upload and trash move: the time, the word, the path and the device. Never a request or an answer |
| `state/reads` | The fingerprint of each file that a device read, under its name. Never a text |

With Dropbox, no text of a file goes to the disk: a read, a download and an upload pass through the memory of the door and of one command, and go on. A failed check after a patch can leave the text of the patched file in `.locks`, as on every door, so name only files with no book text in `ORGTHREAD_DEVICE_PATCH`. The door writes nothing about a request to its output, which a server sends to its journal. With a local folder, each patch keeps a copy of the patched file in `ORGTHREAD_VERSIONS`, as on every door.

## T6: the check before the first real chapter

Run it on a test server, with invented text only. Write the result down.

1. Make an invented chapter with a marker word that is in no other file, such as `quendelmorgenrot`, in a test folder of the rules, for example `/books/an-invented-book/chapter-01.txt`.
2. Pair a test device with `device pair` and `/pair`. Pair the same code again: 400.
3. Allowed calls: `tools/list` gives `read`, `patch` and `list` only. `list` of each read folder, `read` of the chapter, of a glossary, of a summary and of a progress file, then a `patch` of the progress file. All work. A `list` of the folder above a `*`, such as `/books/`, shows the book folders and no file.
4. Every other tool: `write`, `delete`, `search`, `copy`, `move`, `download`, `upload`, `trash`, `version`. Each gives `exit code 2`, and nothing changes. On a door with no `ORGTHREAD_DEVICE_DOWNLOAD` and `ORGTHREAD_DEVICE_UPLOAD`, `GET /download` and `PUT /upload` give 422 with exit code 2 too.
5. Path tricks, each with `read`: a `..` part, a `.` part, other capitals, no `/` at the start, a `//`, the folder name with a letter added, a file next to a progress file, a path outside every rule, and a file directly in the folder above a `*`, such as a whole book in `/books/`. Each gives `exit code 2`.
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
