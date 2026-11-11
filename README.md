# orgthread

Not ready. Do not use it on real files yet.

The first block: an agent reads and writes a plain text file, and it deals with nothing else.

```
orgthread read <file> --who <name>
orgthread write <file> --who <name>   < the whole new text
orgthread patch <file> --who <name>   < only the changes, as JSON
orgthread list <folder>...
orgthread search <text> <folder> --who <name>
orgthread download <file> --who <name> --to <copy>
orgthread upload <file> --who <name>  < any file
orgthread delete <file> --who <name>
orgthread copy <file> <new> --who <name>
orgthread move <file> <new> --who <name>
orgthread watch
```

To see what a folder holds, list it: `orgthread list notes tasks` shows each folder as a group, with the size, the time and the name of each entry, as `ls` does. `ls` is a short form of `list`. It goes one level down, and `--depth 3` or `--depth all` goes deeper. Names that start with a dot stay out, unless you add `--all`. Paths that the limits close stay out too, and read-only ones get a mark. With `--fingerprint`, each file also shows its fingerprint, the content hash that Dropbox reports. With Dropbox it costs nothing more. With a local folder, it reads each file.

To find a text, search a folder: `orgthread search "blood pressure" notes --who me` gives each line that holds it, as `notes/log.md:14: the line`, in the files of that folder and of all its folders. Case does not count, and the text is plain text. `--names` matches only the names of files and folders. An answer shows 100 hits, or up to `--max 1000`, and a last line says how many more were found. The rules of `list` hold: closed paths stay out, read-only hits get a mark, names that start with a dot stay out unless `--all`, and files that are not text are skipped for content. The log line `searched` names the folder and the number of files with a hit, never the text. With Dropbox, a content search fetches the folder as one zip, in memory only, up to `ORGTHREAD_SEARCH_MB`, 25 MB by default, and it refuses a folder that holds a closed path.

To change a file, read it first with the same name. A new file needs no read. orgthread remembers what each name read, and it refuses a write when the file changed since.

`write`, `upload`, `copy` and `move` make every missing folder of the path, and the answer names each one: `written: notes/2026/a.md, 3 bytes, new folders: notes/2026`. The log gets a line `made-folder` for each new folder, right before the line of the file. To keep the first level fixed, set `ORGTHREAD_FIXED_FOLDERS` to a comma-separated list of folders. Then a new folder directly inside one of them is refused with exit code 2, and nothing is made. Deeper new folders are fine.

For a small change to a large file, `patch` sends only the change. Each edit names its exact old lines and their new text, and `append` adds lines at the end:

```
orgthread patch notes/walks.csv --who me <<'END'
{"edits": [{"old": "2026-11-07,park,3.8,\n", "new": "2026-11-07,park,4.1,\n"}],
 "append": "2026-11-08,forest,7.5,\n"}
END
```

The old text must occur once, or the patch is refused with exit code 11, and nothing is written. The exact text is the check, so a change elsewhere in the file since the read does no harm. The rules and examples are in [docs/mcp-api.md](docs/mcp-api.md).

For a file that is not text, such as a picture or a PDF, `download` gives the exact bytes, and `upload` writes them with the rules of `write`. A download notes what this name read, so the same name may then replace the file. A file that is not text may go only to the folders named in `ORGTHREAD_BINARY_ALLOW`, and with that setting empty, to none. A download may have up to 250 MB and passes through in parts, and an upload up to 20 MB. `--check` checks the rules and the size, and moves nothing.

`delete` removes one file, and only the version that this name saw: the one it read, or the one given with `--expect <fingerprint>`, for example from `list --fingerprint`. If the file changed since, or no fingerprint is known, it gives exit code 4 and deletes nothing. It never removes a folder. It works only in the folders named in `ORGTHREAD_DELETE_ALLOW`, and with that setting empty, in none. There is a way back: a local folder keeps the last version beside the program, as for every write, and Dropbox keeps the file in Deleted files, with its rev in the log line. `--check` checks the rules and the version, and deletes nothing.

`copy` makes a new file with the exact bytes of one file. It never replaces a file: if anything is at the new name, it gives exit code 5 and copies nothing. It makes the missing folders of the new name, and names them in the answer. The source must be open to read, and the new name open to write. A file that is not text may go only to the folders of `ORGTHREAD_BINARY_ALLOW`. `--fingerprint <hash>`, for example from `list --fingerprint`, refuses a source of another version with exit code 4. After the copy, the fingerprint of the new file must be the one of the source. If not, the copy is removed again: exit code 7, or 9 if that fails. With Dropbox, the copy is made inside Dropbox, so no byte passes the server, and there only the names tell whether a file is not text. The log line `copied` has the source as its 8th field.

`move` moves one file to a new name, and a rename is a move in the same folder. It never replaces a file: if anything is at the new name, it gives exit code 5 and moves nothing. It makes the missing folders of the new name, and names them in the answer. Only a file moves: a folder gives exit code 2. Both names must be open to write. When `ORGTHREAD_MOVE_ALLOW` names folders, both names must lie in them, else exit code 2. A file that is not text may go only to the folders of `ORGTHREAD_BINARY_ALLOW`. `--fingerprint <hash>` refuses a file of another version with exit code 4. With it, a file that changed during the move goes back to its old name: exit code 7, or 9 if that fails. When the name of a `.md` file changes, the answer adds a line that wikilinks to the old name break. The log line `moved` has the old name as its 8th field. The way back is a move from the new name to the old one.

The steps of a write: take the lock with an exclusive create, compare the fingerprint, write a temporary file and move it into place, read it back and compare, release the lock. Every refusal has its own exit code, and every attempt leaves one log line. `orgthread --help` lists the codes, and [docs/exit-codes.md](docs/exit-codes.md) says which command gives each one and what to do.

The fingerprint is the content hash that Dropbox reports. Today the tool writes to a local folder, behind one small storage interface.

`orgthread watch` is a screen: the locks held now, the latest writes and refusals, and the reason for each. Press Enter on a write to see what changed in it, and Esc to go back. For this, each write keeps a copy of the old and the new text for 30 days, beside the program and outside the synced folder. `h` or Left goes to the next older change of the same file, and `l` or Right to the next newer one, in the list and in a diff. `r` reverts the newest change of a file: a write or a patch of a file that existed before, or a revert. It shows one question first, for example `Revert notes/plan.md to before 2026-11-10 09:14 (invented chat, -3 +1 lines)? y/n`, and only `y` reverts. The revert is a safe write as any other: it changes nothing if the file changed since, and it says so. The log gets a line with the word `reverted` and the name `silviu (watch)`, so `r` on that line brings the change back. On an older change, `r` says that a newer one exists. A new file, a delete or a move has no revert. Only a key press in this screen reverts: there is no command and no tool for it, so no agent can revert. The title line names the version, the commit and the storage, for example `orgthread 0.2.7 (abc1234) · local folder`. Press `?` for a help panel with the version, the storage, the path of the log and all keys, and `?` or Esc to close it. It shows no address, secret path, token or login hash. `orgthread watch --once` prints the same screen as plain text.

orgthread keeps its own things beside the program, on the Mac and on a server alike: its settings in `.env`, its locks in `.locks`, its log in `state/writes.log`, and, for a door, the logins that outlive a restart in `state/logins.json` (hashes of tokens only, mode 600). To end every login at once, stop the door and delete that file, or change the password. Nothing of it goes into your notes. A chat never deals with a lock. On a server, the Dropbox API takes the place of the folder, for example with `ORGTHREAD_STORAGE=dropbox` in `.env`. It keeps no copies on the disk. There, Enter and `r` in `orgthread watch` get the two versions of a write from the Dropbox history, in the background, and the revert runs in the background too, with a limit of 60 seconds: the screen shows the steps, a spinner and the seconds, and Esc goes back at once. Dropbox keeps old versions for a limited time and lists only the 100 newest of a file, so the screen says which side is gone when one is. On a write that was refused because the file changed, Enter shows what was expected against what was found. The versions stay in memory only, up to 25 MB, and are gone when watch quits. A picture, a PDF or a text over 1 MB gets no diff. After 20 seconds with no answer the screen says so, and Enter tries again. If a write fails its check and finds a text it did not expect, that text stays in `.locks`, and the screen names it.

For a local folder, keep the program on the same disk as your notes, as a write moves its new file into place in one step. On another disk a write fails and changes nothing.

Run the tests with `cargo test`. They use invented files in a temporary folder.

## MCP

Two MCP servers give the same read, write, patch, delete, copy, move, list and search to a chat that has no shell, and `version`, which names the version, the commit, the door, the storage and the tools: `orgthread serve` for a client on the same machine, and `orgthread serve --http <address>`, behind a login, for a chat in the cloud such as claude.ai. The remote server also gives `download` and `upload`: a one-time link for each file, which the code sandbox of the chat uses with `curl`. How to start and add each, and the settings, are in [docs/mcp.md](docs/mcp.md). The tools, the refusals and examples are in [docs/mcp-api.md](docs/mcp-api.md).

To set up the local door on a Mac with a plain folder, such as `~/notes`, with no Dropbox and no server, follow [docs/local-folder.md](docs/local-folder.md).

## Licence

Licensed under either of the [MIT licence](LICENSE-MIT) or the [Apache License, Version 2.0](LICENSE-APACHE), at your option.

Unless you say otherwise, any contribution you submit for inclusion in this work is licensed under both, with no added terms or conditions.
