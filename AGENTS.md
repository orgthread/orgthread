# Rules for assistants

## Writing

These hold for the docs, the help text, the messages and the commit messages.

- Use simple language and short sentences.
- Write no em dashes and no contractions: "do not", not "don't".
- Use metric units.
- Write the name in lower case: orgthread.
- Avoid starting a sentence with "orgthread" where another start reads just as well. This is a recommendation, not a rule. Commands and examples may start with it.
- The repository is public. Use invented files in examples, and `example.com` for an address. Write no real path, server address or personal data.
- Promise nothing that is not built. Test each command in the docs before you write it down.

## Versions and tags

Not every build gets a new version. The version line also shows the commit, so that tells any two builds apart.

Raise the version in `Cargo.toml` and add a tag when a build matters to someone who runs it:

- Raise the middle number, such as 0.2.0 to 0.3.0, when something that used to work now stops or works differently, or when a setting or a stored file changes in a way that needs action.
- Raise the last number, such as 0.2.0 to 0.2.1, for a new feature or an important fix that needs no action.
- Change nothing for docs, tests, tidying or a small fix.

The steps:

1. Change the version in `Cargo.toml`, and run `cargo build` so that `Cargo.lock` follows.
2. Run `cargo test`. All tests must pass.
3. Commit, then tag that commit: `git tag -a v0.2.0 -m "what changed for a user"`.
4. Push the commit and the tag together: `git push --atomic origin main v0.2.0`.

Deploy a tagged build where you can, so that the version on a server names a known state.
