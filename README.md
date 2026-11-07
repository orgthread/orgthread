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

`orgthread watch` is a read-only screen: the locks held now, the latest writes and refusals, and the reason for each. It cannot change a file. `orgthread watch --once` prints the same screen as plain text.

Run the tests with `cargo test`. They use invented files in a temporary folder.

## Licence

Licensed under either of the [MIT licence](LICENSE-MIT) or the [Apache License, Version 2.0](LICENSE-APACHE), at your option.

Unless you say otherwise, any contribution you submit for inclusion in this work is licensed under both, with no added terms or conditions.
