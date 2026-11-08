# orgthread

Not ready. Do not use it on real files yet.

The first block: an agent reads and writes a plain text file, and it deals with nothing else.

```
orgthread read <file> --who <name>
orgthread write <file> --who <name>   < the whole new text
orgthread patch <file> --who <name>   < only the changes, as JSON
orgthread list <folder>...
orgthread download <file> --who <name> --to <copy>
orgthread upload <file> --who <name>  < any file
orgthread watch
```

To see what a folder holds, list it: `orgthread list notes tasks` shows each folder as a group, with the size, the time and the name of each entry, as `ls` does. `ls` is a short form of `list`. It goes one level down, and `--depth 3` or `--depth all` goes deeper. Names that start with a dot stay out, unless you add `--all`. Paths that the limits close stay out too, and read-only ones get a mark.

To change a file, read it first with the same name. A new file needs no read. orgthread remembers what each name read, and it refuses a write when the file changed since.

For a small change to a large file, `patch` sends only the change. Each edit names its exact old lines and their new text, and `append` adds lines at the end:

```
orgthread patch notes/walks.csv --who me <<'END'
{"edits": [{"old": "2026-11-07,park,3.8,\n", "new": "2026-11-07,park,4.1,\n"}],
 "append": "2026-11-08,forest,7.5,\n"}
END
```

The old text must occur once, or the patch is refused with exit code 11, and nothing is written. The exact text is the check, so a change elsewhere in the file since the read does no harm. The rules and examples are in [docs/mcp-api.md](docs/mcp-api.md).

For a file that is not text, such as a picture or a PDF, `download` gives the exact bytes, and `upload` writes them with the rules of `write`. A download notes what this name read, so the same name may then replace the file. A file that is not text may go only to the folders named in `ORGTHREAD_BINARY_ALLOW`, and with that setting empty, to none. A download may have up to 250 MB and passes through in parts, and an upload up to 20 MB. `--check` checks the rules and the size, and moves nothing.

The steps of a write: take the lock with an exclusive create, compare the fingerprint, write a temporary file and move it into place, read it back and compare, release the lock. Every refusal has its own exit code, and every attempt leaves one log line. `orgthread --help` lists them.

The fingerprint is the content hash that Dropbox reports. Today the tool writes to a local folder, behind one small storage interface.

`orgthread watch` is a read-only screen: the locks held now, the latest writes and refusals, and the reason for each. It cannot change a file. Press Enter on a write to see what changed in it, and Esc to go back. For this, each write keeps a copy of the old and the new text for 30 days, beside the program and outside the synced folder. `orgthread watch --once` prints the same screen as plain text.

orgthread keeps its own things beside the program, on the Mac and on a server alike: its settings in `.env`, its locks in `.locks`, its log in `state/writes.log`, and, for a door, the logins that outlive a restart in `state/logins.json` (hashes of tokens only, mode 600). To end every login at once, stop the door and delete that file, or change the password. Nothing of it goes into your notes. A chat never deals with a lock. On a server, the Dropbox API takes the place of the folder, for example with `ORGTHREAD_STORAGE=dropbox` in `.env`. It keeps no copies, so `orgthread watch` works there with no web call and shows no diff. If a write fails its check and finds a text it did not expect, that text stays in `.locks`, and the screen names it.

For a local folder, keep the program on the same disk as your notes, as a write moves its new file into place in one step. On another disk a write fails and changes nothing.

Run the tests with `cargo test`. They use invented files in a temporary folder.

## MCP

Two MCP servers give the same read, write, patch and list to a chat that has no shell, and `version`, which names the version, the commit, the door, the storage and the tools: `orgthread serve` for a client on the same machine, and `orgthread serve --http <address>`, behind a login, for a chat in the cloud such as claude.ai. The remote server also gives `download` and `upload`: a one-time link for each file, which the code sandbox of the chat uses with `curl`. How to start and add each, and the settings, are in [docs/mcp.md](docs/mcp.md). The tools, the refusals and examples are in [docs/mcp-api.md](docs/mcp-api.md).

## Licence

Licensed under either of the [MIT licence](LICENSE-MIT) or the [Apache License, Version 2.0](LICENSE-APACHE), at your option.

Unless you say otherwise, any contribution you submit for inclusion in this work is licensed under both, with no added terms or conditions.
