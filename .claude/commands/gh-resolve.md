---
description: Triage and clear GitHub PR review threads with gh-resolve
allowed-tools: Read, Write, Edit, Glob, Grep, Bash
---

# PR review triage: $ARGUMENTS

Work the unresolved review threads on **$ARGUMENTS** (PR number or URL; if empty,
ask which PR) using `gh-resolve` (`~/.local/bin/gh-resolve`).

## Command surface

```
~/.local/bin/gh-resolve [list] <PR>            # unresolved threads, full bodies
~/.local/bin/gh-resolve -f <PR>                # entire conversation of each thread
~/.local/bin/gh-resolve --resolved <PR>        # audit what was already closed
~/.local/bin/gh-resolve -o <PR>                # outdated threads only
~/.local/bin/gh-resolve agent <PR>             # self-contained bundle: hunks + threads + line ranges
~/.local/bin/gh-resolve agent -j <PR>          # same, JSON
~/.local/bin/gh-resolve suggestions <PR>       # review ```suggestion blocks
~/.local/bin/gh-resolve suggestions --apply <PR>       # splice them into the local checkout
~/.local/bin/gh-resolve suggestions --apply -n <PR>    # dry run: show the splice plan
~/.local/bin/gh-resolve checks <PR>            # CI status, failures first
~/.local/bin/gh-resolve reply <ID> -m "text"   # comment in a thread; -m - reads stdin
~/.local/bin/gh-resolve reply <ID> -m "text" --resolve
~/.local/bin/gh-resolve resolve <ID> | --all <PR> | --outdated <PR>
~/.local/bin/gh-resolve unresolve <ID>
```

Invoke it by that absolute path — `~/.local/bin` is not on the PATH of a
non-interactive shell.

Any command takes `-r owner/repo`, `-j`, `-q`, `-n` (dry run).

## Related: gh-comment

`gh-resolve` operates on GitHub's own thread-resolution state (GraphQL). For
marking one specific comment's TEXT as addressed/moot — useful when a thread
has several comments and only one needs flagging, or the repo doesn't lean on
thread resolution — use `~/.local/bin/gh-comment` instead:

```
~/.local/bin/gh-comment mark <COMMENT_URL> COMPLETED    # green banner, addressed
~/.local/bin/gh-comment mark <COMMENT_URL> CANCELED     # red banner, struck through
~/.local/bin/gh-comment list <PR>                       # every comment, grouped by author
```

`COMMENT_URL` is the `#issuecomment-N` or `#discussion_r-N` link (not a
gh-resolve thread ID). Takes `-m "note"`, `-n` (dry run), `-j`, `-q` too.
`gh-comment list` is the fastest way to see the whole conversation — including
bot reviews — broken down by who said what before diving into `gh-resolve agent`.

## Workflow

1. `~/.local/bin/gh-resolve checks <PR>` — CI failures usually explain half the
   review comments.
2. `~/.local/bin/gh-resolve agent <PR>` — read the bundle. It pins line numbers to the head
   commit and gives each thread's ID, diff hunk, and full conversation.
3. Check out the PR branch (`gh pr checkout <PR>`) before editing. Line numbers
   are only valid at the PR head.
4. For each thread, decide: fix, push back, or defer. Fix the code yourself —
   the bundle is context, not a patch to apply blindly.
5. `~/.local/bin/gh-resolve reply <ID> -m "..." --resolve` once a fix is committed.

## Rules

- **Judge every suggestion before adopting it.** Bot suggestions (gemini,
  copilot, coderabbit) are frequently right about the problem and wrong about
  the fix. `--apply` splices text at line numbers; it does not typecheck.
- **Never resolve a thread you did not address.** `resolve --all` is for
  sweeping threads already made moot by later commits, not for silencing review.
- Outdated threads (`📝`, `L?`) point at code that has since changed. Re-read the
  current file before acting; their line numbers are stale.
- `suggestions --apply` refuses to run unless local HEAD matches the PR head and
  the target files are clean. Do not reach for `--force` to get past that —
  check out the right commit instead.
- Reply in the author's voice: say what changed and where, not that an assistant
  did it. Push back when the comment is wrong, with the reasoning.
- Bulk mutations sleep 0.5s apart and stop on secondary rate limits. If a run
  reports threads left unprocessed, re-run rather than tightening the loop.
