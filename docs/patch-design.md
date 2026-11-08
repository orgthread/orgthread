# The design of `patch`

Built in version 0.2.2. This file is the plan agreed on 2026-11-08 Sun. The user docs are in the README and [mcp-api.md](mcp-api.md), so this file can go.

## Why

`write` sends the whole new file. For a large file with a small change, that is slow and costly. The model must copy every line into the tool call, and each copied line is a chance for a slip. Example: a CSV file of 150 KB where a nightly task adds one row and replaces a few. `patch` sends only the lines that change, so a few KB instead of the whole file.

A chat with a shell could build the whole file with a script and send it. That route belongs to the upload of the transfer design, not to this step. `patch` is for every client, also a chat on a phone with MCP tools only.

## The goal: keep it lean

One form only: exact old text and new text, plus an append at the end. Models know this form from their own edit tools. There are no line numbers to count and no diff format to get right. The exact text is the check: an edit that does not fit is refused, and nothing is written.

## The command

```
orgthread patch <file> --who <name>
```

The edits come as JSON on the standard input:

```json
{
  "edits": [
    {"old": "2026-11-07,park,3.8,\n", "new": "2026-11-07,park,4.1,\n"}
  ],
  "append": "2026-11-08,forest,7.5,\n"
}
```

- `<file>` and `--who` follow the rules of `write`: the same root, the same limits, the same name as for the read.
- `edits`: a list, which can be empty. Each edit has `old` and `new`, and may have `line`.
- `append`: text for the end of the file. It is optional.
- At least one edit or an append is needed.

## How an edit finds its place

- `old` must not be empty.
- A match counts only at whole lines. It starts at the start of a line, and it ends after a newline or at the end of the file. So `"  make test\n"` never matches inside `"  make test-all\n"`.
- When `old` ends with a newline, `new` must end with one too, or be empty. Two lines cannot join by accident.
- A delete is an edit with an empty `new`.
- An insert is an edit with the line before it as `old`, and that line plus the new lines as `new`.
- `line` is the number of the line where `old` starts, counted from 1 in the file as it was before this call. It only picks between equal matches, and it must pick exactly:

| Matches of `old` | `line` | What happens |
|---|---|---|
| None | Any | Refused: the old text is not in the file |
| One | None, or any | Applied. A `line` that differs gets a note in the answer: `edit 1 found at line 14, not 12` |
| Two or more | None | Refused, with the lines of each match |
| Two or more | At one of the matches | Applied at that match |
| Two or more | At no match | Refused, with the lines of each match. There is no "closest match" |

- Two edits must not touch the same lines. Such a call is refused, as the order would be unclear.

A refusal names the edit and the lines, so that the chat can fix the edit at once:

```
orgthread: refused, nothing written. Edit 2: the old text occurs 3 times, at lines 4, 88 and 140. Add a line before or after it so that it occurs once, or give the line of the one you mean.
```

The help text tells the chat to take a line that differs, such as the heading of the section, as the neighbours of a repeated line often repeat too.

## The append

- The edits come first, then the append.
- If the file does not end with a newline, a newline goes in before the new text. If the new text does not end with one, a newline is added. So the file always ends with a newline.
- The text is kept exactly as it is sent. The program does not read CSV and does not count columns.
- The append only adds at the end. An insert in the middle is an edit.

## A patch sent twice

The answer of a patch can get lost on its way back, for example when the connection breaks. The task does not know that the patch went through, and sends it again.

- An edit is safe. Its old text is gone after the first try, so the second try gets exit code 11, and nothing is written.
- An append is not safe. It has no old text, so the same lines go in twice, and nothing warns.

So a task that can send again puts its new lines into an edit with an anchor, not into an append. Example: a nightly task replaces the rows of the last days and adds the new row in the same edit, with those old rows as the old text:

```json
{"edits": [{
  "old": "2026-11-06,river,6.0,\"windy, cold\"\n2026-11-07,park,3.8,\n",
  "new": "2026-11-06,river,6.0,\"windy, cold\"\n2026-11-07,park,4.1,\n2026-11-08,forest,7.5,\n"
}]}
```

A second try of this patch is refused. An append stays for a chat that sends once and reads the answer.

This also holds when none of the old rows changes. Then the old text is still in the file after the first try, inside the new text. So an edit is refused when the file already holds its new text at the place of the match (found in the build, 2026-11-08 Sun):

```
orgthread: refused, nothing written. Edit 1: the file already holds its new text at line 1, so this patch may have gone through already.
```

To tell "done already" from a real mismatch, a refusal adds a hint when the new text of the edit is already in the file:

```
orgthread: refused, nothing written. Edit 1: the old text is not in the file. Its new text is at line 3, so this patch may have gone through already.
```

The program changes nothing for this case. The hint is only a few more words in the message.

## The checks

- The file must exist. A new file is made with `write`. A missing file gets exit code 6.
- This name must have read the file, or written it, before. Otherwise the call gets exit code 5, as with `write`.
- The file need not be unchanged since that read. The exact text of each edit is the check. So a change in another part of the file, such as a new row from another chat, does not force a new read of the whole file.
- The program takes the lock, reads the file, applies all edits and the append, and checks the result as `write` checks a new text: not empty, valid UTF-8, no NUL sign. Then it writes the whole file with the same compare-and-set as `write`, reads it back and releases the lock. A store cannot change a file in place, so the patch only changes how the new text is made.
- All or nothing: if one edit does not fit, nothing is written.
- If the file changes in the short time between this read and the write, for example a person saves in an editor in that same second, the patch gets exit code 4 with the advice "Send the same patch again". It needs no new read, as the next try applies the edits to the new text.
- After a patch, the program notes the new state for this name, as after a write. A next patch or write needs no new read.

## The answer

```
patched: notes/walks.csv, 1 edit, 1 append, 120 bytes
```

The path is the path from the root, and the size is the size of the whole new file. A note about a `line` that differs comes on the next line.

## Errors and exit codes

The codes of `write`, and one new code:

| Code | When |
|---|---|
| 0 | Patched |
| 2 | Wrong use: bad JSON, no edits and no append, a path outside the root, a closed or read-only path, a result that is empty or not text |
| 3 | Busy: another writer holds the lock |
| 4 | The file changed during the patch. Send the same patch again |
| 5 | This name has not read the file |
| 6 | Missing: there is no such file |
| 11 | An edit does not fit: the old text is not there, it occurs more than once with no fitting `line`, two edits overlap, or `new` drops the newline of `old`. Nothing is written |

1, 7, 8, 9 and 10 mean what they mean for `write`. Code 4 is not used for an edit that does not fit, as its advice, "read it again", is wrong there: the chat needs a better edit, not a full new read.

## The log and the screens

- The log gets one line per call, as for a write, with the word `patched` and the fingerprints before and after.
- The diff screen of `watch` keeps the old and the new text, as for a write, and counts `patched` as a good outcome, as it counts `written` today.

## The MCP tool `patch`

| Field | What it holds | Required |
|---|---|---|
| `file` | The full path, as for `write` | Yes |
| `who` | The name of this chat, the same one as for the read | Yes |
| `edits` | A list of `{old, new}`, each with an optional `line` | No, but one edit or `append` is needed |
| `append` | Text for the end of the file | No |

The tool starts `orgthread patch`, as `write` starts its command, and passes the JSON with its size, so that a server stopped halfway cannot pass a cut-off text. Its text is the text of the command. With a code other than 0, `isError` is true and the text starts with `exit code N`.

## Not part of this step

- The whole file built by a script: the upload of the transfer design.
- A unified diff as input. It is as safe as exact text, but models write it with more slips.
- A "closest match" for a line number that is off.
- A flag to replace every occurrence. A chat sends one edit per place.
- A check of the CSV columns.
- A new file through `patch`.

## The build

- A new step in the write procedure: build the new text from the old bytes under the lock, then go on as `write` does.
- Tests with invented files in a temporary folder, for each row of the tables above: one match, no match, two matches with and without `line`, a `line` at no match, overlapping edits, a delete, an insert, a lost newline, the append with and without a newline at the end of the file, edits and an append in one call, all or nothing, a file never read (5), a missing file (6), a change during the patch (4), a result that is empty, repeated lines as in a table of contents, and the same patch sent twice, with the hint "may have gone through already".
- The log word, the `watch` screen, the help text, the README and [mcp-api.md](mcp-api.md), with a real run of the local server as the example.
- A new feature that needs no action. `list` takes 0.2.1, so this step raises the version to 0.2.2, with the tag `v0.2.2`. If both ship together, they share one version.
