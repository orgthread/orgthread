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
| `list` | `folders` | A list of full paths of folders, at least one |
| | `depth` | Optional: how many levels down, a number from 1, or `"all"`. The default is 1 |
| | `all` | Optional: `true` also shows the names that start with a dot. The default is `false` |
| `download` | `files` | A list of 1 to 25 full paths of files. Remote server only |
| | `who` | The name of this chat, the same one each time |
| `upload` | `files` | A list of 1 to 25 full paths, where the files will go. Remote server only |
| | `who` | The name of this chat, the same one each time |

The fields of `read` and `write` are all required, and `file` and `who` of `patch`. With the local folder, `file` and each folder must lie inside `ORGTHREAD_ROOT`. With Dropbox, they start at the root of the Dropbox: `/org/a.md`. A list may name the root itself: `/`.

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

A list notes nothing, so it needs no `who`.

`download` and `upload` give one line for each file, in the order given. A line that works gives the path, for a download the size, and the link last. A file that is refused gets `refused, exit code N:` and the reason, and the others still get their links. With a refusal, the text starts with `exit code N` of the first file that failed. Use each link at once in the code sandbox, and never show, store or log it:

```
curl -sS -o magazine-2026-11.pdf '<link>'
curl -sS -T p088-1.jpg '<link>'
```

- A download gives any file, also a text, as exact bytes, up to 250 MB. It notes what this name read, as `read` does.
- An upload keeps the rules of `write`: a new file needs no read, and a replace needs a download or a read by the same name first. A text file may go wherever `write` may write. A file that is not text may go only to the folders that the owner allowed. It may have up to 20 MB.
- The checks run when the tool makes the link, so that a refusal comes early, and again when the bytes arrive.

`curl` prints what an upload answers: the same `written:` line as `write`, or `exit code N` and the refusal. The HTTP status says the same:

| Status | When |
|---|---|
| 200 | Done |
| 409 | Refused. The text starts with `exit code N`, as in the table below |
| 410 | The link waited more than 60 seconds. Ask the tool for a new one |
| 404 | No such link, or it was used already |
| 405 | The link is for the other direction |

A download sends the bytes in chunks. If it fails halfway, the last chunk never comes, so `curl` reports an error instead of a short file.

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
| 4 | Changed since this name read it. For `patch`: changed during the patch | Read it again and build the change from that text. For `patch`: send the same patch again |
| 5 | Exists, and this name has not read it | Read it first |
| 6 | Missing: there is no such file or folder, or it is gone since the read | Look for the file |
| 7 | The check after the write failed, and the old state is back | Read the reason |
| 8 | Written, but the log line failed | Look at the log file |
| 9 | The check after the write failed, and the old state is NOT back | Look at the file now |
| 10 | Written, but the clean-up did not finish, or another writer may have been at work | Read the message. If it names another writer, check the file. If not, there is nothing to do: the file is free again within 15 minutes |
| 11 | An edit of a patch does not fit: its old text is not in the file, it occurs more than once with no fitting `line`, two edits overlap, or the new text drops the newline of the old text. Nothing is written | Read the message. It names the edit and the lines. Fix the edit and send the patch again. No new read is needed |

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
