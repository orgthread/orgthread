# The MCP tools

The tools of both MCP servers, their refusals, and examples. To start a server and add it to a client, see [mcp.md](mcp.md).

## The tools

| Tool | Field | What it holds |
|---|---|---|
| `read` | `file` | The full path of the file |
| | `who` | The name of this chat, on one line, the same one each time |
| `write` | `file` | The full path of the file |
| | `who` | The name of this chat, on one line, the same one each time |
| | `text` | The whole new text |
| `patch` | `file` | The full path of the file |
| | `who` | The name of this chat, the same one as for the read |
| | `edits` | Optional: a list of changes, each with `old` and `new`, and an optional `line` |
| | `append` | Optional: lines to add at the end. A patch needs an edit, an append or both |
| `delete` | `file` | The full path of the file |
| | `who` | The name of this chat, the same one as for the read |
| | `expect` | Optional: the fingerprint of the version to delete, for example from `list` with `fingerprint`. Needed when this name has not read the file |
| | `check` | Optional: `true` only checks the rules and the version, and deletes nothing. The default is `false` |
| `copy` | `pairs` | A list of 1 to 25 pairs. Each pair has `from`, the full path of the source, and `to`, the full path of the new file. An optional `fingerprint` is the version the source must have |
| | `who` | The name of this chat, the same one each time |
| `list` | `folders` | A list of full paths of folders, at least one |
| | `depth` | Optional: how many levels down, a number from 1, or `"all"`. The default is 1 |
| | `all` | Optional: `true` also shows the names that start with a dot. The default is `false` |
| | `fingerprint` | Optional: `true` also shows the fingerprint of each file. The default is `false` |
| `download` | `files` | A list of 1 to 25 full paths of files. Remote server only |
| | `who` | The name of this chat, the same one each time |
| `upload` | `files` | A list of 1 to 25 full paths, where the files will go. Remote server only |
| | `who` | The name of this chat, the same one each time |
| `version` | | No fields |

The fields of `read` and `write` are all required, `file` and `who` of `patch` and `delete`, and `pairs` and `who` of `copy`, with `from` and `to` in each pair. With the local folder, `file` and each folder must lie inside `ORGTHREAD_ROOT`. With Dropbox, they start at the root of the Dropbox: `/org/a.md`, or `org/a.md` as the log and `orgthread watch` show it. Both name the same file. A list may name the root itself: `/`.

`read` gives the exact text of the file, and notes what this name read.

`write` replaces the whole file. To change a file, read it first with the same name. A new file needs no read. After a write, the same name may write again with no new read. A write answers like this:

```
written: notes/a.md, 4 bytes
```

The path in the answer is the path from the root.

`patch` changes part of a file, so that a large file need not be sent whole. Each edit replaces its exact `old` text with its `new` text:

- `old` must be whole lines, with their newlines, and it must occur once in the file. A match never starts or ends in the middle of a line.
- If `old` occurs more than once, the patch is refused, and the refusal names the lines. Add a line before or after it that differs, such as the heading of the section, or give `line`, the number of the line where it starts. `line` only picks between equal matches, and it must pick exactly.
- An empty `new` deletes the old text. To insert, take the line before as `old`, and that line plus the new lines as `new`.
- `append` adds lines at the end, after the edits. A missing newline before or after them is added.
- All edits are applied, or none. Two edits must not touch the same lines.
- This name must have read the file once. A change elsewhere in the file since the read does no harm, as the exact text is the check. After a patch, the same name may write or patch again with no new read.
- A file that does not exist yet is made with `write`.

A patch answers like this:

```
patched: notes/walks.csv, 1 edit, 1 append, 120 bytes
```

If a `line` was given for a text that occurs once, at another line, the edit is applied, and a line `note: edit 1 found at line 14, not 12` follows.

A task that may send the same patch twice, for example after a lost answer, puts its new lines into an edit, with the lines before them as `old`, and not into `append`. A second try of such an edit is refused with code 11 and the words "may have gone through already". A second `append` would add the lines twice.

`list` gives one group per folder. The header is the folder as given, then a colon. Each line is the size in bytes, the time and the name. A folder name ends with `/`, and a folder shows `-` for its size and its time. To read a file, join the header and the name. The sort order is by name, and capitals do not count.

- Names that start with a dot, such as `.DS_Store`, stay out unless `all` is `true`.
- A path that the limits close stays out, always. A read-only path gets `(read only)` after its name.
- With a `depth` above 1, each subfolder gets its own group, with its full path as the header.
- A file in place of a folder shows under its own folder.
- A list shows at most 1000 entries. After that, a last line says `more entries not shown: list a smaller folder or a lower depth`.
- The time is in the time zone of the machine that runs the server, with the day name. On a server, set `TZ`, for example `TZ=Europe/Vienna`.
- With `fingerprint` set to `true`, each line gets the fingerprint of the file before its name: the content hash that Dropbox reports, 64 hex signs. It is the same as `orgthread fingerprint` gives. A folder shows `-`. With Dropbox, the fingerprint comes with the list, so it costs no more web calls. With a local folder, the server reads each file for it, so a large folder takes longer. A task can compare it with the fingerprint from an earlier run, to find the files that changed, and read only those.

A list notes nothing, so it needs no `who`.

`delete` removes one file, and only the version that this name saw:

- The version is the one this name read, or the one given as `expect`. If the file changed since, or this name has not read it and no `expect` is given, it is refused with code 4, and nothing is deleted.
- It never removes a folder. One file per call.
- It works only in the folders that the owner opened for deletes, with `ORGTHREAD_DELETE_ALLOW`. With that setting empty, the default, every delete is refused with code 2. The other limits still count: a closed or read-only path is refused too.
- There is a way back. The local door keeps the last version beside the program, as for every write. With Dropbox, the file stays in its Deleted files, and the answer and the log line give its rev.
- With Dropbox, a read notes the rev of the file next to its fingerprint. The delete is then one web call, and Dropbox checks the version at the same moment as it deletes. With `expect` and no read, the server first looks up the rev.

A delete answers like this, on a local door:

```
deleted: notes/old-plan.md, 17 bytes. The last version is kept.
```

With Dropbox, the answer ends with `Dropbox keeps it in Deleted files, rev <rev>.` in place of the last sentence.

`copy` makes new files with the exact bytes of their sources, one pair at a time, in the order given:

- Up to 25 pairs in one call. For more, call again with the rest. 26 pairs are wrong use: a JSON-RPC error with the code -32602, and nothing is copied. The same holds for a pair with no `from` or `to`.
- It never replaces a file. If anything is at `to`, a file or a folder, that pair is refused with code 5.
- It makes the missing folders of `to`.
- The source must be open to read, and `to` open to write, as for `read` and `write`. A file that is not text may go only to the folders that the owner allowed. On the server, only the names tell this, as for `upload`: a picture whose name has no extension passes. The local door also reads the bytes.
- With `fingerprint`, a source of another version is refused with code 4. Without it, the copy takes the source as it is.
- After the copy, the fingerprint of the new file must be the one of the source. If not, the copy is removed again, code 7 and the log word `removed`, or 9 and `not-removed` if the removal fails.
- With Dropbox, the copy is made inside Dropbox, in two web calls per pair: one look at the source, and the copy. No byte passes the server. If Dropbox gives no answer, one more look at `to` tells whether the copy is there. If something other than the source is there, it is never removed, as it may be older than the copy.
- Each pair gives one line: `copied:`, the source, the new name, the size and the fingerprint, or `refused, exit code N:` and the reason. The pairs that passed stay copied, and the next pairs still run. With a refusal, the text starts with `exit code N` of the first pair that failed.

```
exit code 5
copied: notes/plan.md -> archive/2026/plan.md, 17 bytes, 4c79418b76038c58388a702c49afa45be960cf0b6430a2461fd57450b6647f62
/tmp/claude/example-copy/root/notes/plan.md -> /tmp/claude/example-copy/root/notes/taken.md  refused, exit code 5: refused, nothing copied. notes/taken.md exists, and a copy never replaces a file. Name a new copy.
```

The log line of a copy has 8 fields: the time, `copied`, the new name, the name of the chat, `-`, the fingerprint, the size, and the source with a `/` at its start, for example `/notes/plan.md`. A refused copy has the same 8th field, so `orgthread watch` names it as a copy, never as a delete.

`download` and `upload` give one line for each file, in the order given. A line that works gives the path, for a download the size, and the link last. A file that is refused gets `refused, exit code N:` and the reason, and the others still get their links. With a refusal, the text starts with `exit code N` of the first file that failed. Use each link at once in the code sandbox, and never show, store or log it:

```
curl -sS -o magazine-2026-11.pdf '<link>'
curl -sS -T p088-1.jpg '<link>'
```

- A download gives any file, also a text, as exact bytes, up to 250 MB. It notes what this name read, as `read` does.
- An upload keeps the rules of `write`: a new file needs no read, and a replace needs a download or a read by the same name first. A text file may go wherever `write` may write. A file that is not text may go only to the folders that the owner allowed. It may have up to 20 MB.
- The checks run when the tool makes the link, so that a refusal comes early, and again when the bytes arrive.

`version` gives four lines: the version and the commit, the door, the storage and the tools:

```
orgthread 0.2.5 (abc1234)
door: remote
storage: Dropbox
tools: read, write, patch, delete, copy, list, download, upload, version
```

- The first line is the same as `orgthread --version`. A `+` after the commit means that the build had changes not yet committed.
- `door` is `local` for `orgthread serve` and `remote` for `orgthread serve --http`.
- `storage` is `local folder` or `Dropbox`.
- `tools` names the tools that this server gives now. If the list of tools of the chat lacks one of them, refresh that list in the client.
- The server itself answers, and starts no command. So the answer names the build that runs, also when a newer program is already on the disk and waits for a restart.
- The answer holds no path, no address, no setting and no limit. A list notes nothing, and neither does `version`.

`curl` prints what an upload answers: the same `written:` line as `write`, or `exit code N` and the refusal. The HTTP status says the same:

| Status | When |
|---|---|
| 200 | Done |
| 409 | Refused. The text starts with `exit code N`, as in [exit-codes.md](exit-codes.md) |
| 410 | The link waited more than 5 minutes. Ask the tool for a new one |
| 404 | No such link, or it was used already. The answer is empty, as for any wrong address |
| 405 | The link is for the other direction |

If `curl` says `CONNECT tunnel failed, response 403`, the request never reached the server: the host is not on the list of allowed websites of claude.ai. See [mcp.md](mcp.md).

A download sends the bytes in chunks. If it fails halfway, the last chunk never comes, so `curl` reports an error instead of a short file.

## Refusals

A refusal is a tool result with `isError` set to true. Its text starts with the exit code, followed by what the command said:

```
exit code 4
orgthread: refused, nothing written. notes/a.md changed since you read it. Read it again and build the change from that text.
```

Each code, the commands that give it and what to do are in [exit-codes.md](exit-codes.md).

So `isError` does not always mean that nothing changed. With 8 and 10, the text is written. With 9, the file may be damaged.

A list with several folders goes on after a folder that fails. That folder gets a line with the reason in its own group. The exit code is the code of the first folder that failed, and the text still holds the folders that worked.

A missing field or an unknown tool is no refusal. It gets a JSON-RPC error with the code -32602, and the server goes on.

## Examples

These come from a real run of the local server, with `ORGTHREAD_ROOT=/tmp/claude/example`. The file `notes/a.md` held the line "First line", and the client had sent `initialize` first. Each message is one line of JSON.

### `read`

Request:

```json
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"read","arguments":{"file":"/tmp/claude/example/notes/a.md","who":"chat a"}}}
```

Reply:

```json
{"id":2,"jsonrpc":"2.0","result":{"content":[{"text":"First line\n","type":"text"}],"isError":false}}
```

### `write`

This name has read the file, so the write goes through.

Request:

```json
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"write","arguments":{"file":"/tmp/claude/example/notes/a.md","who":"chat a","text":"First line\nSecond line\n"}}}
```

Reply:

```json
{"id":3,"jsonrpc":"2.0","result":{"content":[{"text":"written: notes/a.md, 23 bytes\n","type":"text"}],"isError":false}}
```

### A refusal

A person then changed the file, and the same name wrote again. The file kept the change of the person.

Request:

```json
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"write","arguments":{"file":"/tmp/claude/example/notes/a.md","who":"chat a","text":"Third try\n"}}}
```

Reply:

```json
{"id":4,"jsonrpc":"2.0","result":{"content":[{"text":"exit code 4\norgthread: refused, nothing written. notes/a.md changed since you read it. Read it again and build the change from that text.\n","type":"text"}],"isError":true}}
```

### `list`

Here the folder `notes` held the file `a.md`, the folder `drafts` with the file `b.md`, and a `.DS_Store`. The server ran with `TZ=Europe/Vienna`.

Request:

```json
{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"list","arguments":{"folders":["/tmp/claude/example/notes"]}}}
```

Reply:

```json
{"id":5,"jsonrpc":"2.0","result":{"content":[{"text":"/tmp/claude/example/notes:\n         11  2026-11-08 Sun 14:05  a.md\n          -  -                     drafts/\n","type":"text"}],"isError":false}}
```

### A list with fingerprints

The same folder, two levels down, with the fingerprints:

Request:

```json
{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"list","arguments":{"folders":["/tmp/claude/example/notes"],"depth":2,"fingerprint":true}}}
```

Reply:

```json
{"id":5,"jsonrpc":"2.0","result":{"content":[{"text":"/tmp/claude/example/notes:\n         11  2026-11-08 Sun 14:05  e56b9a770f9012640f47cafa3ae75b4fa504d84aed0268245c5b3b246070410d  a.md\n          -  -                     -                                                                 drafts/\n\n/tmp/claude/example/notes/drafts:\n          2  2026-11-09 Mon 09:03  c606dd677840d364890dce4afe87cfa83260633ed6020d197e8837f553dbbb89  b.md\n","type":"text"}],"isError":false}}
```

### A list that fails in part

Two levels down, with a second folder that does not exist:

Request:

```json
{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"list","arguments":{"folders":["/tmp/claude/example/notes","/tmp/claude/example/tasks"],"depth":2}}}
```

Reply:

```json
{"id":6,"jsonrpc":"2.0","result":{"content":[{"text":"exit code 6\n/tmp/claude/example/notes:\n         11  2026-11-08 Sun 14:05  a.md\n          -  -                     drafts/\n\n/tmp/claude/example/notes/drafts:\n          6  2026-11-07 Sat 19:12  b.md\n\n/tmp/claude/example/tasks:\n  there is no file or folder /tmp/claude/example/tasks\n","type":"text"}],"isError":true}}
```

### `patch`

This comes from a second real run of the local server, with `ORGTHREAD_ROOT=/tmp/claude/example-patch`. The file `notes/walks.csv` held four lines, and this name had read it:

```
date,place,km,notes
2026-11-05,park,4.2,
2026-11-06,river,6.0,"windy, cold"
2026-11-07,park,3.8,
```

One edit corrects a row, and an append adds one.

Request:

```json
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"patch","arguments":{"file":"/tmp/claude/example-patch/notes/walks.csv","who":"chat a","edits":[{"old":"2026-11-07,park,3.8,\n","new":"2026-11-07,park,4.1,\n"}],"append":"2026-11-08,forest,7.5,\n"}}}
```

Reply:

```json
{"id":3,"jsonrpc":"2.0","result":{"content":[{"text":"patched: notes/walks.csv, 1 edit, 1 append, 120 bytes\n","type":"text"}],"isError":false}}
```

### A patch sent again

The same edit, sent a second time. Its old text is gone, and its new text is in the file.

Request:

```json
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"patch","arguments":{"file":"/tmp/claude/example-patch/notes/walks.csv","who":"chat a","edits":[{"old":"2026-11-07,park,3.8,\n","new":"2026-11-07,park,4.1,\n"}]}}}
```

Reply:

```json
{"id":4,"jsonrpc":"2.0","result":{"content":[{"text":"exit code 11\norgthread: refused, nothing written. Edit 1: the old text is not in the file. Its new text is at line 4, so this patch may have gone through already.\n","type":"text"}],"isError":true}}
```

### `delete`

These come from a run of the local server with `ORGTHREAD_ROOT=/tmp/claude/example-delete/root` and `ORGTHREAD_DELETE_ALLOW=notes`. The file `notes/old-plan.md` held the line "walk in the park".

First the delete came before any read, so it was refused:

```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"delete","arguments":{"file":"/tmp/claude/example-delete/root/notes/old-plan.md","who":"chat a"}}}
```

```json
{"id":1,"jsonrpc":"2.0","result":{"content":[{"text":"exit code 4\norgthread: refused, nothing deleted. chat a has not read notes/old-plan.md, and no --expect was given. Read it first with: orgthread read <file> --who <same name>\n","type":"text"}],"isError":true}}
```

After a `read` by the same name, a check, then the delete:

```json
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"delete","arguments":{"file":"/tmp/claude/example-delete/root/notes/old-plan.md","who":"chat a","check":true}}}
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"delete","arguments":{"file":"/tmp/claude/example-delete/root/notes/old-plan.md","who":"chat a"}}}
```

```json
{"id":3,"jsonrpc":"2.0","result":{"content":[{"text":"check passed, nothing deleted: notes/old-plan.md, 17 bytes\n","type":"text"}],"isError":false}}
{"id":4,"jsonrpc":"2.0","result":{"content":[{"text":"deleted: notes/old-plan.md, 17 bytes. The last version is kept.\n","type":"text"}],"isError":false}}
```

The log line of the delete has 8 fields: the time, `deleted`, the path, the name, the fingerprint before, `-`, the size, and the rev. A local folder has no rev, so the last field is `-`.

### `version`

From the same run of the local server, with the commit shown as `abc1234`:

Request:

```json
{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"version","arguments":{}}}
```

Reply:

```json
{"id":7,"jsonrpc":"2.0","result":{"content":[{"text":"orgthread 0.2.5 (abc1234)\ndoor: local\nstorage: local folder\ntools: read, write, patch, list, version\n","type":"text"}],"isError":false}}
```

### `download` and `upload`

These come from a real run of the remote server on the same machine, with `ORGTHREAD_ROOT=/tmp/claude/example-transfer`, `ORGTHREAD_BINARY_ALLOW=scans` and the invented public address `https://notes.example.com`. The secret of each link is shown as `LINK`. The file `scans/magazine-2026-11.pdf` held 18 bytes, and there was no file `scans/none.pdf`.

Request:

```json
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"download","arguments":{"files":["/tmp/claude/example-transfer/scans/magazine-2026-11.pdf","/tmp/claude/example-transfer/scans/none.pdf"],"who":"chat a"}}}
```

Reply. The second file is refused, and the first still gets its link:

```json
{"id":2,"jsonrpc":"2.0","result":{"content":[{"text":"exit code 6\n/tmp/claude/example-transfer/scans/magazine-2026-11.pdf  18 bytes  https://notes.example.com/t/LINK\n/tmp/claude/example-transfer/scans/none.pdf  refused, exit code 6: there is no file scans/none.pdf\n","type":"text"}],"isError":true}}
```

Request:

```json
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"upload","arguments":{"files":["/tmp/claude/example-transfer/scans/p088-1.jpg"],"who":"chat a"}}}
```

Reply:

```json
{"id":3,"jsonrpc":"2.0","result":{"content":[{"text":"/tmp/claude/example-transfer/scans/p088-1.jpg  https://notes.example.com/t/LINK\n","type":"text"}],"isError":false}}
```

Then the download link fetched the file, and the upload link sent a picture of 48211 bytes. In this run on one machine, `curl` used the local address of the server in place of the public one:

```
$ curl -sS -o magazine-2026-11.pdf 'https://notes.example.com/t/LINK'
$ curl -sS -T p088-1.jpg 'https://notes.example.com/t/LINK'
written: scans/p088-1.jpg, 48211 bytes
```
