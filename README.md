# orgthread

Not ready. Do not use it on real files yet.

The first block: an agent reads and writes a plain text file, and it deals with nothing else.

```
orgthread read <file> --who <name>
orgthread write <file> --who <name>   < the whole new text
orgthread watch
```

To change a file, read it first with the same name. A new file needs no read. orgthread remembers what each name read, and it refuses a write when the file changed since.

The steps of a write: take the lock with an exclusive create, compare the fingerprint, write a temporary file and move it into place, read it back and compare, release the lock. Every refusal has its own exit code, and every attempt leaves one log line. `orgthread --help` lists them.

The fingerprint is the content hash that Dropbox reports. Today the tool writes to a local folder, behind one small storage interface.

`orgthread watch` is a read-only screen: the locks held now, the latest writes and refusals, and the reason for each. It cannot change a file. Press Enter on a write to see what changed in it, and Esc to go back. For this, each write keeps a copy of the old and the new text for 30 days, beside the program and outside the synced folder. `orgthread watch --once` prints the same screen as plain text.

`orgthread serve` is the same read and write for a chat that has no shell. It speaks MCP over standard input and output, with two tools: `read` (file, who) and `write` (file, who, text). Each call starts the command itself, so the rules and the refusals are the same.

orgthread keeps its own things beside the program, on the Mac and on a server alike: its settings in `.env`, its locks in `.locks`, its log in `state/writes.log`, and, for a door, the logins that outlive a restart in `state/logins.json` (hashes of tokens only, mode 600). To end every login at once, stop the door and delete that file, or change the password. Nothing of it goes into your notes. A chat never deals with a lock. On a server, the Dropbox API takes the place of the folder, for example with `ORGTHREAD_STORAGE=dropbox` in `.env`. It keeps no copies, so `orgthread watch` works there with no web call and shows no diff. If a write fails its check and finds a text it did not expect, that text stays in `.locks`, and the screen names it.

For a local folder, keep the program on the same disk as your notes, as a write moves its new file into place in one step. On another disk a write fails and changes nothing.

Run the tests with `cargo test`. They use invented files in a temporary folder.

## Licence

Licensed under either of the [MIT licence](LICENSE-MIT) or the [Apache License, Version 2.0](LICENSE-APACHE), at your option.

Unless you say otherwise, any contribution you submit for inclusion in this work is licensed under both, with no added terms or conditions.
