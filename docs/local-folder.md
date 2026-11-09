# Use orgthread with a plain folder on a Mac

Not ready. Do not use it on real files yet. If you try it on your notes all the same, do step 7 first.

This guide sets up the local door only: Claude Code on the same Mac calls `orgthread serve`, and the notes are a plain folder, such as `~/notes`. There is no Dropbox and no server. The examples use `~/notes` for the notes and `~/Applications/orgthread` for the program. Change both to your own paths.

## 1. Get the program

Find the chip of the Mac:

```
uname -m
```

It gives `arm64` (Apple silicon) or `x86_64` (Intel).

**Way 1: build it.** This needs no admin rights for Rust. It needs the Command Line Tools of Apple, for `git` and the linker. `xcode-select -p` prints a path when they are there. If they are not, `xcode-select --install` installs them, and this can ask for an admin password.

Install Rust with rustup. It goes into your home folder:

```
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Open a new terminal, so that `cargo` is on the path. Then build the tagged version:

```
git clone https://github.com/orgthread/orgthread.git
cd orgthread
git checkout v0.2.7
cargo build --release
```

The program is `target/release/orgthread`. A program that you build on the Mac itself is not blocked by macOS, so a company Mac usually lets it run.

**Way 2: copy it from another Mac.** Copy the program of a Mac with the same chip, as `uname -m` gives. A copy that came by download, mail or AirDrop has a quarantine mark, and macOS can refuse to open it. Remove the mark:

```
xattr -d com.apple.quarantine ~/Applications/orgthread/orgthread
```

If the file has no mark, this says `No such xattr`, and that does no harm. The program has no signature from a known developer. A company Mac can block such a program all the same. Then use way 1, or ask your IT team.

## 2. Give the program a folder of its own

The program keeps its own things beside itself: its settings in `.env`, its locks in `.locks`, and its log and notes in `state/`. So give it a folder of its own. Keep that folder on the same disk as your notes, because a write moves its new file into place in one step. On another disk, a write fails and changes nothing.

```
mkdir -p ~/Applications/orgthread
cp target/release/orgthread ~/Applications/orgthread/
```

## 3. Tell it where the notes are

Write a file `.env` beside the program. The default root is a Dropbox folder, so you must set it. Give the full path, with no `~` and no quotes:

```
ORGTHREAD_ROOT=/Users/you/notes
```

Two settings are optional. Each takes paths from the root, separated by commas:

```
ORGTHREAD_READ_ONLY=archive
ORGTHREAD_DENY=private
```

With these, an agent may read `archive` but not change it, and it cannot see `private` at all. All settings are in [mcp.md](mcp.md#settings).

## 4. Check it

Give the full path of each file, or run the commands from inside the notes folder. A path outside the root is refused with exit code 2.

```
cd ~/notes
~/Applications/orgthread/orgthread --version
~/Applications/orgthread/orgthread list ~/notes
printf 'A first test.\n' | ~/Applications/orgthread/orgthread write ~/notes/test.md --who me
~/Applications/orgthread/orgthread read ~/notes/test.md --who me
~/Applications/orgthread/orgthread watch --once
```

What you see:

- `--version` names the version and the commit, such as `orgthread 0.2.7 (5405a53)`.
- `list` shows the entries of the folder. A read-only folder gets `(read only)`, and a closed one is not shown.
- `write` answers `written: test.md, 14 bytes`.
- `read` prints `A first test.`
- `watch --once` shows the write, with the title line `orgthread 0.2.7 (5405a53) · local folder`.

Then delete `test.md`.

## 5. Connect Claude Code

Start the commands in the notes folder. With `--scope project`, Claude Code writes the server into a file `.mcp.json` in that folder:

```
cd ~/notes
claude mcp add --scope project orgthread -- /Users/you/Applications/orgthread/orgthread serve
```

Without `--scope project`, the server is for you alone, in this folder, and Claude Code keeps it in its own settings.

Start a new Claude Code session in `~/notes`. Say yes when it asks whether you trust the folder. Then type `/mcp`: `orgthread` must show as connected. Ask the chat to call the orgthread `version` tool. It answers with the version, the door `local`, the storage and the tools.

## 6. Make agents use the door

A rule alone does not stop an agent. So give it a rule, and take the other ways away.

Write a file `CLAUDE.md` in the notes folder:

```
Read and write files in this folder only with the orgthread tools: read, write, patch, list. Never edit them directly.
Give the full path of each file. Use the name `claude-code` as `who`.
```

Write a file `.claude/settings.json` in the notes folder:

```json
{
  "permissions": {
    "allow": ["mcp__orgthread__*"],
    "deny": ["Edit", "Write", "NotebookEdit", "Edit(/**)"]
  },
  "enabledMcpjsonServers": ["orgthread"]
}
```

What each part does:

- `"Edit"`, `"Write"` and `"NotebookEdit"` take the file tools of Claude Code away. Each must be named. With `"Edit"` alone, the Write tool still works.
- `"Edit(/**)"` stops the shell commands that Claude Code knows to change files, such as `sed -i` and `>>`, for every file in this folder. A script that opens a file itself, for example in Python, is not stopped.
- `"mcp__orgthread__*"` lets the orgthread tools run with no question. The name after `mcp__` is the name you gave in `claude mcp add`. Claude Code uses this line only when you trust the folder.
- `"enabledMcpjsonServers"` approves the server of `.mcp.json`. Without it, Claude Code asks once.

A deny holds in every permission mode. Claude Code reads the file when a session starts in this folder, so start your sessions here.

## 7. Keep a safety net

orgthread has no undo command yet. So keep the notes folder in git:

```
cd ~/notes
git init
git add -A
git commit -m "First state of my notes"
```

Commit again at the end of each day. To see what changed since the last commit, run `git diff`.

## What is different from the server

- The local door has no `download` and `upload`. It gives `read`, `write`, `patch`, `delete`, `list` and `version`. `delete` works only in the folders of `ORGTHREAD_DELETE_ALLOW`, and keeps the last version in `state/versions` first. These tools work on text only.
- `orgthread watch` shows the diff of each write. For this, each write keeps a copy of the old and the new text for 30 days in `state/versions`, beside the program. That folder is not in your notes and not in git. Delete it if the copies must not stay on the Mac.
- The log is `state/writes.log`, beside the program.
