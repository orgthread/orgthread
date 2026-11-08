# The design of `download` and `upload`

Not built yet. This file is the plan agreed on 2026-11-08 Sun. When the tools are built, the user docs go into the README, [mcp.md](mcp.md) and [mcp-api.md](mcp-api.md), and this file can go.

## Why

A chat in the cloud works on large files in its sandbox. One example is an issue PDF, which a script splits into articles and pictures. Today such a file reaches the sandbox through a Dropbox download link. The split pictures cannot go back at all: the Dropbox connector writes only text, so the chat hands over a zip, and a person drops it in by hand.

`download` and `upload` give a chat a one-time link for each file. The sandbox moves the bytes with `curl`, so the model never types or reads them. The same limits, rules and refusals as `read` and `write` hold. With these two tools, the Dropbox connector can close.

## The goal: keep it lean

- One way for every file: a link. There are no bytes in a tool reply. A model gains nothing from raw bytes, and no task needs to see a picture in the reply.
- Text keeps `read` and `write`. Both give every byte as it is.
- The tools exist on the remote server only. A chat on the same machine has the disk, and the local server has no address for a link.

## How a chat uses it

The tool gives a link for each file. The script in the sandbox uses it at once:

```
curl -sS -o magazine-2026-11.pdf 'https://notes.example.com/a-long-invented-secret/t/LINK'
curl -sS -T p088-1.jpg 'https://notes.example.com/a-long-invented-secret/t/LINK'
```

The bytes go straight between the sandbox and the server. The model sees only paths, links and result lines.

## The MCP tools

| Tool | Field | What it holds | Required |
|---|---|---|---|
| `download` | `files` | A list of 1 to 25 full paths, as for `read` | Yes |
| | `who` | The name of this chat, the same one each time | Yes |
| `upload` | `files` | A list of 1 to 25 full paths, where the files will go | Yes |
| | `who` | The name of this chat, the same one each time | Yes |

The answer has one line for each file, in the order given. A line that works gives the path, the size for a download, and the link last:

```
/scans/magazine-2026-11.pdf  17645389 bytes  https://notes.example.com/a-long-invented-secret/t/LINK
/notes/private/scan.pdf  refused, exit code 2: the path is closed
```

A file that is refused does not stop the others. With a code other than 0, `isError` is true, the text starts with `exit code N`, and N is the code of the first file that failed, as for `list`.

The tool descriptions tell the chat to use each link at once in the sandbox, and never to show, store or log it.

## The link

- It works once, for one file and one direction.
- It must start within 60 seconds. Once it starts, the transfer may run as long as it needs, so a large PDF does not stop halfway.
- It holds a long random secret, from the system random source. The server keeps the links in memory only, so a restart ends them all.
- It works without a login, like a key under the mat. That is why it is short-lived and works once.
- It lives under the public address, at `<public address>/t/<secret>`. A download is a GET, an upload a PUT.
- The log never holds a link, and neither do the error messages.

## The rules

### Download

- The same limits as `read`: `ORGTHREAD_ALLOW`, `ORGTHREAD_DENY`, `ORGTHREAD_READ_ONLY`.
- Any type of file, text too. So a chat can get the exact bytes of a text file in its sandbox.
- At most 250 MB, set by `ORGTHREAD_DOWNLOAD_MB`. A larger file is refused with code 2 before any link is made, as the size is known first.
- The bytes stream through the server in parts, so the memory of the server stays small at any size.
- A download notes what this name read, with the fingerprint, as `read` does. So a later upload by the same name may replace the file.

### Upload

- The same rules as `write`: a new file needs no read. To replace a file, the same name must have downloaded or read it first. The refusals are the same: 2 closed or read only, 3 busy, 4 changed since the read, 5 exists and not read, 6 missing.
- A text file, valid UTF-8 with no NUL sign, may go wherever `write` may write.
- Any other file may go only to the folders named in `ORGTHREAD_BINARY_ALLOW`, for example `/scans` and `/notes/images`. Elsewhere it is refused with code 2. With the setting empty, the default, no binary file can be uploaded.
- At most 20 MB, set by `ORGTHREAD_UPLOAD_MB`. The server counts the bytes as they arrive. At the first byte over the limit, it stops, writes nothing, and refuses with code 2.
- The upload keeps the whole file in memory and goes through the write procedure of today: the lock, the last check, the write, the check after it, the log. 20 MB keeps this safe.
- The checks run twice: when the tool makes the link, so that a refusal comes early, and again when the bytes arrive, as the file can change in between.

### What `curl` gets back

An upload answers with the same text as `write`, and the script prints it:

| HTTP status | Text |
|---|---|
| 200 | `written: scans/magazine-2026-11/images/p088-1.jpg, 48211 bytes` |
| 409 | `exit code N`, then the refusal, as in [mcp-api.md](mcp-api.md) |
| 410 | The link was used, or its 60 seconds passed. Ask the tool for a new one |
| 404 | No such link |

A download answers 200 with the bytes, or the same 409, 410 and 404 with a text.

## The log

One line for each finished transfer, as for a write: the time, the name, `download` or `upload`, the path from the root and the size. Never the link.

## The settings

| Setting | What it does | Default |
|---|---|---|
| `ORGTHREAD_BINARY_ALLOW` | Comma-separated folders where an upload of a file that is not text may go | None |
| `ORGTHREAD_DOWNLOAD_MB` | The largest download, in MB | 250 |
| `ORGTHREAD_UPLOAD_MB` | The largest upload, in MB | 20 |

A MB here is 1,000,000 bytes.

## Not part of this step

- A picture or a PDF in the tool reply.
- An upload that streams, for files over 20 MB. If one is ever needed, it is a step of its own, as it needs a new write procedure.
- The tools on the local server.
- Links made by Dropbox in place of the server's own links. This is the way out if the sandbox test below fails.
- A change of the rules of the chats that use the Dropbox connector today. They can move to these tools after the build.

## The build

0. **The sandbox test, first.** In a claude.ai chat, run `curl` in the sandbox against the public address of the server. If the sandbox cannot reach it, stop and bring the design back for a new decision: the links would then come from Dropbox.
1. Two commands for the server to start, as the tools start `read` and `write` today:
   - `orgthread download <file> --who <name>` streams the bytes to the standard output. It refuses a terminal there. With `--to <copy>`, it saves an exact copy instead, as `read --to` does.
   - `orgthread upload <file> --who <name> --size <bytes>` takes the bytes on the standard input and writes them with the write procedure, with the binary rule above in place of the text check.
2. The links in the remote server: made by the tools, kept in memory, used once at `/t/<secret>`.
3. Streaming from Dropbox: today a Dropbox call collects the whole answer and has a time limit of 120 seconds. A download needs the bytes passed on in parts, with no such time limit once the transfer has started.
4. Tests with invented files in a temporary folder, for each row of the rules above: several files with one refused, a closed path, a read-only path, a binary upload outside `ORGTHREAD_BINARY_ALLOW`, a replace with and without a download first, a file changed between the link and the upload, a download and an upload over the limit, a link used twice, a link after its 60 seconds, a link that never appears in the log. The Dropbox side has no test server today. So one run by hand against the real Dropbox checks it, with the largest PDF at hand.
5. Updates to the help text, the README, [mcp.md](mcp.md) and [mcp-api.md](mcp-api.md), with a real run as the example.
6. A new feature that needs no action, as no binary upload works until a folder is named. So the last number of the version goes up, for example from 0.2.2 to 0.2.3.
