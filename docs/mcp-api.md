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
