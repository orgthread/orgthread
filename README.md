# orgthread

Not ready. Do not use it on real files yet.

The first block: one command that writes a plain text file the same way every time.

```
orgthread fingerprint <file>
orgthread write <file> --expect <fingerprint> --who <name>   < new-text
orgthread write <file> --new --who <name>                    < new-text
orgthread watch
```

The steps of a write: take the lock with an exclusive create, compare the fingerprint, write a temporary file and move it into place, read it back and compare, release the lock. Every refusal has its own exit code, and every attempt leaves one log line. `orgthread --help` lists them.

The fingerprint is the content hash that Dropbox reports. Today the tool writes to a local folder, behind one small storage interface.

`orgthread watch` is a read-only screen: the locks held now, the latest writes and refusals, and the reason for each. It cannot change a file. `orgthread watch --once` prints the same screen as plain text.

Run the tests with `cargo test`. They use invented files in a temporary folder.
