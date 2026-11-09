# Exit codes

Each command ends with an exit code. 0 means done. Every other code names one kind of refusal or failure, and the message on the error output says more. The codes are the same on the command line and in the MCP tools.

## The codes

| Code | Meaning | Given by | What to do |
|---|---|---|---|
| 0 | Done. For `write`, `patch` and `upload`: written, logged, and the lock is free again. For `delete`: deleted and logged, or with `--check`, all rules hold. For `copy`: copied, checked and logged | All commands and tools | Nothing |
| 1 | Failed, and the file is unchanged. For example, the storage gave an error, the note of a read could not be kept, or the output could not be handed over. For `copy`: nothing was copied, or Dropbox gave no answer and the look after it found no copy, or something else at the new name that stays | All commands and tools, but not `password-hash` and the `version` tool | Read the reason. Try again when the cause is gone |
| 2 | Wrong use, or not allowed. For example: an unknown command or option, a missing setting, a path outside the root, a path that is closed or read only for this door, a text that is empty, not UTF-8 or holds a NUL sign, a file over the size limit, a file that is not text outside the folders of `ORGTHREAD_BINARY_ALLOW`, a delete outside the folders of `ORGTHREAD_DELETE_ALLOW` or of a folder, a copy of a folder, an address that `serve --http` may not listen on | All commands and tools, but not the `version` tool | Read the reason, and change the call |
| 3 | Busy: another writer holds the lock of this file. For `copy`: of the new name | `write`, `patch`, `upload`, `delete`, `copy` | Try again in a moment |
| 4 | Changed since this name read it. For `patch`: changed during the patch. For `delete`: the file is not the version this name read or `--expect` gave, or no fingerprint is known. Nothing is deleted. For `copy`: the source is not the version of `--fingerprint`. Nothing is copied | `write`, `patch`, `upload`, `delete`, `copy` | Read it again and build the change from that text. For `patch`: send the same patch again. For `delete`: read it, look at it, and delete again only if it should still go. For `copy`: list it again, and copy only if this version should go |
| 5 | Exists, and this name has not read it. For `read --to` and `download --to`: the copy exists already. For `copy`: a file or folder is at the new name, and it stays | `write`, `patch`, `upload`, `read --to`, `download --to`, `copy` | Read it first with the same name. For `--to` and `copy`: name a new copy |
| 6 | Missing: there is no such file or folder, or it is gone since the read | `read`, `download`, `list`, `patch`, `write`, `upload`, `delete`, `copy`, `fingerprint` | Look for the file. A new file is made with `write`, not with `patch` |
| 7 | The check after the write failed, and the old state is back. For `copy`: the new file differed from the source, and it is removed again, with the log word `removed` | `write`, `patch`, `upload`, `copy` | Read the reason. The file holds what it held before. For `copy`: try again |
| 8 | Written, but the log line failed. For `download`: sent, but the log line failed. For `delete`: deleted, but the log line failed. For `copy`: copied, but the log line failed | `write`, `patch`, `upload`, `download`, `delete`, `copy` | The change is done. Look at the log file and its folder |
| 9 | The check after the write failed, and the old state is NOT back. For `delete`: the storage removed the path, but reports other bytes than expected. For `copy`: the new file differs from the source, and it could not be removed, with the log word `not-removed` | `write`, `patch`, `upload`, `delete`, `copy` | Look at the file now. Its old version is in the Dropbox history. For `delete`: the message names the rev in Deleted files, or where the file is kept. For `copy`: delete the new file once you looked at it |
| 10 | Written, but the clean-up did not finish, or another writer may have been at work | `write`, `patch`, `upload`, `delete`, `copy` | Read the message. If it names another writer, check the file. If not, there is nothing to do: the file is free again within 15 minutes |
| 11 | An edit of a patch does not fit: its old text is not in the file, it occurs more than once with no fitting `line`, two edits overlap, or the new text drops the newline of the old text. Nothing is written | `patch` | Read the message. It names the edit and the lines. Fix the edit and send the patch again. No new read is needed |

So a code other than 0 does not always mean that nothing changed. With 8 and 10, the change is written, the file is deleted, or the copy is made. With 9, the file may be damaged.

The other commands give few codes. `watch` and `serve` give 0, 1 or 2. `fingerprint` gives 0, 1, 2 or 6. `password-hash` gives 0 or 2. `--help` and `--version` give 0.

## Several files or folders

A `list` with several folders goes on after a folder that fails. That folder gets a line with the reason in its own group. The exit code is the code of the first folder that failed.

The `copy` tool takes up to 25 pairs. It runs `copy` for each pair, in order. A pair that fails gets the line `refused, exit code N` and the reason, and the next pairs still run. The pairs that passed stay copied. The answer starts with the code of the first pair that failed.

The `download` and `upload` tools take up to 25 files. They check each file with `download --check` or `upload --check`, and make a link for each file that passes. A file that fails gets the line `refused, exit code N` and the reason. The answer starts with the code of the first file that failed. So these tools give 1, 2 or 6 for a download, and 1, 2 or 5 for an upload. The other codes can come later, when the link is used.

## In the MCP tools

A refusal is a tool result with `isError` set to true. Its text starts with `exit code N`, followed by what the command said:

```
exit code 4
orgthread: refused, nothing written. notes/a.md changed since you read it. Read it again and build the change from that text.
```

If the command was stopped before it could end, the text starts with `the command was stopped` and has no code.

A missing field or an unknown tool is no refusal. It gets a JSON-RPC error with the code -32602, and the server goes on.

How each tool is called is in [mcp-api.md](mcp-api.md).

## The answers of a link

A link from the `download` or `upload` tool answers with an HTTP status. `curl` shows the text of the answer.

| Status | When |
|---|---|
| 200 | Done. A download gives the bytes. An upload gives the same `written:` line as `write` |
| 404 | No such link, or it was used already. The answer is empty, as for any wrong address |
| 405 | The link is for the other direction, or the method is not `GET` or `PUT` |
| 409 | Refused. The text starts with `exit code N`, with a code from the table above |
| 410 | The link waited more than 5 minutes. Ask the tool for a new one |
| 411 | An upload without a `Content-Length`. `curl -T` gives one |
| 500 | The command could not start, or it was stopped |

A download sends the bytes in chunks. If it fails halfway, the last chunk never comes, so `curl` reports an error instead of a short file.

If `curl` says `CONNECT tunnel failed, response 403`, the request never reached orgthread. The 403 comes from the proxy of the claude.ai sandbox: the host, such as `files.example.com`, is not on the list of allowed websites of claude.ai. See [mcp.md](mcp.md).
