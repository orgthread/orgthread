# The design of `list`

Built in version 0.2.1. This file is the plan agreed on 2026-11-08 Sun, kept for the reasons behind it. The user docs are in the README and in [mcp-api.md](mcp-api.md).

## Why

A chat must see which files a folder holds before it reads one. Today it needs another tool for that. `list` gives the same answer as `read` and `write` give: the same root, the same limits, the same exit codes.

## The goal: keep it lean

A model reads `ls` output well. So `list` looks like `ls`, and the fine points stay inside the program. Each line explains itself. There is one text format for the command and for the MCP tool.

## The command

```
orgthread list <folder>... [--depth <n>|all] [--all]
orgthread ls <folder>...   the same command, a short form for the shell
```

- `<folder>`: one or more folders, as `ls a b` takes them. A path follows the rules of `read`: with a local folder, a path inside `ORGTHREAD_ROOT`; with Dropbox, a path that starts with `/`. The root itself is allowed too.
- `--depth <n>`: how many levels down. The default is 1, only the folder itself. `--depth all` goes to the bottom.
- `--all`: also show the names that start with a dot.
- There is no `--who`. A list notes nothing, so it needs no name.

## The answer

```
orgthread list /org/notes /org/tasks
/org/notes:
     1204  2026-11-08 Sun 14:05  a.md
        -           -            drafts/
      310  2026-11-01 Sun 08:00  old.md  (read only)

/org/tasks:
  there is no folder /org/tasks
```

- Each folder gets a group. The header is the path as given, then a colon. A blank line comes between groups.
- Each entry is one line: the size in bytes, the modification time, then the name. The name comes last, so a space in a name does no harm.
- A folder name ends with `/`. A folder shows `-` for its size and its time, as Dropbox gives no time for a folder.
- The time is in the time zone of the machine, with the day name: `2026-11-08 Sun 14:05`. On a server, set `TZ` to the zone you want.
- To read a file, a chat joins the header and the name: `/org/notes/a.md`.
- With `--depth` above 1, each subfolder gets its own group under it, with its full path as the header, as `ls -R` does.
- A file in place of a folder gives a group with that one entry. The header is the folder of the file, so that the header and the name still make its path.

### Sort order

By name, and capitals do not count. Files and folders are mixed, as in `ls`.

### What is left out, and what is marked

| Entry | What the list does |
|---|---|
| A name that starts with a dot, such as `.DS_Store`, `.obsidian/`, `.claude/`, `.cc-writes` | Left out. `--all` shows it |
| A path under `ORGTHREAD_DENY`, or outside `ORGTHREAD_ALLOW` | Left out, also with `--all`. A chat never learns that it exists |
| A path under `ORGTHREAD_READ_ONLY` | Shown, with `(read only)` after the name |
| A link to a place outside the root | Left out |
| A name that is not valid UTF-8 | Left out |

With `ORGTHREAD_ALLOW=notes/drafts`, a list of `notes` is refused, as `notes` itself lies outside the allowed folders. List `notes/drafts` directly.

There is no mark for a file that is busy. A lock lasts a few seconds, so such a mark would be old before a chat reads it. A busy write gets exit code 3 anyway.

### The limit

At most 1000 entries per call, over all groups together. After the limit, one last line:

```
more entries not shown: list a smaller folder or a lower depth
```

The exit code is still 0. There is no cursor. With Dropbox, the program follows the cursor of `list_folder` itself, up to the limit, and the chat never sees it.

## Errors and exit codes

The same codes as `read`:

| Code | When |
|---|---|
| 0 | Listed, also when the limit cut the list |
| 1 | Failed, for example a Dropbox error or no permission to read the folder |
| 2 | Wrong use: a path outside the root, a closed path, an unknown option, a bad depth |
| 6 | Missing: there is no such folder or file |

With several folders, each folder that fails gets one line in its own group, and the others are still listed. The exit code is the code of the first folder that failed. The error lines go to the standard output, in the groups, so that the answer reads in order.

## The MCP tool `list`

| Field | What it holds | Required |
|---|---|---|
| `folders` | A list of full paths, as for `read` | Yes |
| `depth` | A number from 1, or `"all"` | No, 1 by default |
| `all` | `true` also shows the names that start with a dot | No, `false` by default |

The tool starts `orgthread list`, as `read` and `write` start their commands. Its text is the same text as the command gives. With a code other than 0, `isError` is true and the text starts with `exit code N`, as for the other tools. Then the text can still hold the folders that worked.

## Not part of this step

- A fingerprint per file. A chat needs none for a write, as each read notes it. Added later as the option `--fingerprint`, for tasks that look for changed files: see [mcp-api.md](mcp-api.md).
- A mark "changed since this name read it". It needs `--who`, so it is a feature of its own, for later.
- A sort by time, a JSON shape, and a cursor for the chat.
- A move tool. It comes later, as a separate step.

## The build

- A new method of the storage interface, `list`, for the local folder and for Dropbox.
- Tests with invented files in a temporary folder, for each row of the tables above: one level and several levels, several folders, a missing folder among them, dot names with and without `--all`, closed and read-only paths, the limit, the sort order, a file in place of a folder, a path outside the root. The Dropbox side has tests against the small stand-in for the Dropbox API in `tests/dropbox.rs`, with pages of two entries, so that the cursor is at work. One run by hand against the real Dropbox checks the rest.
- Updates to the help text, the README and [mcp-api.md](mcp-api.md), with a real run of the local server as the example.
- A new feature that needs no action, so the version goes from 0.2.0 to 0.2.1, with the tag `v0.2.1`.
