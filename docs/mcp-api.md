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
| `list` | `folders` | A list of full paths of folders, at least one |
| | `depth` | Optional: how many levels down, a number from 1, or `"all"`. The default is 1 |
| | `all` | Optional: `true` also shows the names that start with a dot. The default is `false` |

The fields of `read` and `write` are all required. With the local folder, `file` and each folder must lie inside `ORGTHREAD_ROOT`. With Dropbox, they start at the root of the Dropbox: `/org/a.md`. A list may name the root itself: `/`.

`read` gives the exact text of the file, and notes what this name read.

`write` replaces the whole file. To change a file, read it first with the same name. A new file needs no read. After a write, the same name may write again with no new read. A write answers like this:

```
written: notes/a.md, 4 bytes
```

The path in the answer is the path from the root.

`list` gives one group per folder. The header is the folder as given, then a colon. Each line is the size in bytes, the time and the name. A folder name ends with `/`, and a folder shows `-` for its size and its time. To read a file, join the header and the name. The sort order is by name, and capitals do not count.

- Names that start with a dot, such as `.DS_Store`, stay out unless `all` is `true`.
- A path that the limits close stays out, always. A read-only path gets `(read only)` after its name.
- With a `depth` above 1, each subfolder gets its own group, with its full path as the header.
- A file in place of a folder shows under its own folder.
- A list shows at most 1000 entries. After that, a last line says `more entries not shown: list a smaller folder or a lower depth`.
- The time is in the time zone of the machine that runs the server, with the day name. On a server, set `TZ`, for example `TZ=Europe/Vienna`.

A list notes nothing, so it needs no `who`.

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
| 6 | Missing: there is no such file or folder, or it is gone since the read | Look for the file |
| 7 | The check after the write failed, and the old state is back | Read the reason |
| 8 | Written, but the log line failed | Look at the log file |
| 9 | The check after the write failed, and the old state is NOT back | Look at the file now |
| 10 | Written, but the clean-up did not finish, or another writer may have been at work | Read the message. If it names another writer, check the file. If not, there is nothing to do: the file is free again within 15 minutes |

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
