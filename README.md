# prk

`prk` (short for *parikshak*, examiner) reviews GitHub pull requests with Claude Code
when someone tags you in a Slack channel.

Someone posts in `#peer-review`:

> @you please review: https://github.com/Blitzscale-19/suchna/pull/826

`prk` sees the tag, checks out the PR, reviews it with Claude Code's `/code-review`,
and posts the findings on the PR as inline comments **from your GitHub account**.
When the author pushes fixes and tags you again, `prk` runs a **follow-up**: it checks
each earlier comment, replies *fixed* / *still present* / *partly fixed* in its thread,
and reviews only what changed.

It also:
- **catches up** on start: every request posted in the channel **today** is picked up
- **keeps a queue** that survives restarts, and retries failed reviews
- **skips** what was already reviewed, by `prk` or by you on GitHub
- shows a **live dashboard**, a **history** (`prk -H`) and the **comments** each
  review posted (`prk -c`), all in the terminal

Everything it needs (your Slack id, your GitHub token, your Claude token, review rules
per repo) lives in one config file, so anyone can run their own copy on their own
machine or on a server.

---

## Contents

1. [Install](#1-install)
2. [Set up: `prk init`](#2-set-up-prk-init)
3. [Check and run](#3-check-and-run)
4. [Run it as a service](#4-run-it-as-a-service)
- [Commands](#commands)
- [The dashboard](#the-dashboard)
- [History and comments](#history-and-comments)
- [How a review runs](#how-a-review-runs)
- [Follow-up reviews](#follow-up-reviews)
- [Queue, catch-up and edge cases](#queue-catch-up-and-edge-cases)
- [Review rules](#review-rules)
- [Config reference](#config-reference)
- [Files and paths](#files-and-paths)
- [Troubleshooting](#troubleshooting)

---

## 1. Install

You need these on the machine that runs `prk` (Linux, macOS, WSL, or Windows in PowerShell, cmd or Git Bash):

| Tool | Check | Install |
|---|---|---|
| curl, git | `git --version` | your package manager |
| GitHub CLI | `gh --version` | https://cli.github.com (`gh auth login` is optional: `prk init` can use it, or a token instead) |
| Claude Code | `claude --version` | `npm install -g @anthropic-ai/claude-code` |

On Windows, install Claude Code with its native installer, `irm https://claude.ai/install.ps1 | iex`
in PowerShell. The npm version (`claude.cmd`) works too. Git for Windows (https://git-scm.com)
gives you `git`, `ssh` and Git Bash.

Install the prebuilt `prk` binary (no Rust needed).

**Linux, macOS, WSL, Git Bash:**

```bash
curl -fsSL https://github.com/pankajyadav-blitz/prk/releases/latest/download/install.sh | sh
```

This puts the binary at `~/.local/bin/prk`. If that folder is not on your `PATH`, the
installer prints the line to add to `~/.zshrc` / `~/.bashrc`. Then:

```bash
prk --version
prk init          # next step
```

- **Update** later with `prk update` (downloads the latest release, checks its sha256, replaces the binary).
- Other folder: `curl -fsSL .../install.sh | PRK_INSTALL_DIR=/usr/local/bin sh`. A fixed version: `PRK_VERSION=v0.1.1`.
- Direct downloads (one archive per platform, with a `.sha256`): https://github.com/pankajyadav-blitz/prk/releases/latest

**Windows PowerShell:**

```powershell
irm https://github.com/pankajyadav-blitz/prk/releases/latest/download/install.ps1 | iex
```

**Windows cmd:**

```bat
powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://github.com/pankajyadav-blitz/prk/releases/latest/download/install.ps1 | iex"
```

On Windows, both installers put `prk.exe` in `%USERPROFILE%\.local\bin` and add that folder to your
user `PATH`; open a new window afterwards. The config lives at
`%USERPROFILE%\.config\prk\config.toml`, the same place for PowerShell, cmd and Git Bash.

> **Git Bash:** its own window (mintty) cannot pass typed input to Windows programs, so run
> `winpty prk init` there, or run `prk init` in PowerShell, cmd or Windows Terminal. Everything
> else (`prk`, `prk -H`, `prk review`, ...) works as is.

**From source instead** (needs Rust: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`):

```bash
git clone https://github.com/pankajyadav-blitz/prk.git ~/prk
cd ~/prk && cargo install --path .     # installs to ~/.cargo/bin/prk
```

**Releasing** (maintainers): bump `version` in `Cargo.toml`, commit, then
`git tag vX.Y.Z && git push origin main vX.Y.Z`. GitHub Actions builds Linux (x86_64,
arm64, static musl) and macOS (Intel, Apple Silicon) binaries and publishes the release.

---

## 2. Set up: `prk init`

```bash
prk init
```

It asks a few questions, checks each answer as you give it, and writes
`~/.config/prk/config.toml` (mode 600). Run it again any time: whatever already works
is kept, and it asks only for what is missing or broken. Stopping half-way keeps what
you entered so far.

What it walks you through:

1. **Tools.** Checks that `git`, `gh` and `claude` are installed.
2. **Slack.** Prints a link that opens Slack's *Create app* page with everything
   filled in (Socket Mode, the bot scopes, the `message.channels` event). Click
   **Create**, then:
   - **Basic Information → App-Level Tokens → Generate Token and Scopes**, scope
     `connections:write`. Paste the `xapp-...` token.
   - **Install App → Install to Workspace**. Paste the **Bot User OAuth Token** (`xoxb-...`).

   `prk` then joins `#peer-review` itself and finds **your member id** from your
   Slack email (it offers your `git config user.email`). Confirm the name it shows.
3. **GitHub.** If `gh` is logged in, it offers to post comments as that account. Or
   it opens a prefilled page to create a fine-grained token (owner `Blitzscale-19`,
   Pull requests: Read and write, Contents: Read-only) and you paste it. Either way it
   shows the account comments will be posted as. Then it checks that git can reach
   GitHub over ssh, and tells you how to add a key if not.
4. **Claude.** Runs `claude setup-token` (browser login with your Claude
   subscription) and saves the long-lived token. Skip it to use this machine's
   `claude` login instead, which expires sooner.
5. **Check.** Runs [`prk check`](#3-check-and-run).

The config it writes holds only these values; everything else takes its default:

```toml
[slack]
app_token = "xapp-..."
bot_token = "xoxb-..."
reviewer  = "U0..."

[github]
token = ""          # empty = your gh login; or github_pat_...

[claude]
oauth_token = "sk-ant-oat01-..."
```

Add [review rules](#review-rules) or change any [other setting](#config-reference) by
editing the file; [config.example.toml](config.example.toml) lists every option. The
config lives at `~/.config/prk/config.toml`; set `PRK_CONFIG=/path` or pass
`--config /path` to use another (works with `prk init` too).

> **One running `prk` per Slack app.** Slack spreads events across every connection
> that uses the same app token, so two processes on one app each see only some of the
> messages. Give each person's `prk` its own app, and don't share one with another bot.

**Other Claude options.** `prk setup-token` replaces the subscription token later
(`--token sk-ant-oat01-...` saves one without the browser). An API key from
https://console.anthropic.com goes in `claude.api_key` instead and is billed per
token. With a subscription, reviews share your plan's limits with your own Claude Code
use; when the limit is reached, `prk` pauses the queue for 30 minutes and retries.

**Private channel.** Set `slack.channel` to its id, add the `groups:history` scope
and the `message.groups` event to the app, reinstall it, and `/invite` the app.

**Org approval.** If the org must approve fine-grained tokens, a new token works once
an org admin approves it.

---

## 3. Check and run

```bash
prk check
```

Every line should have a ✓:

```
config /home/you/.config/prk/config.toml
✓ slack     app token works; watching CPWCB54G2 for <@U0XXXXXXX>
✓ catch-up  reads the channel; 12 message(s) today, caught up on each start
✓ github    comments will be posted as your-github-login
✓ git ssh   ssh to github.com works
✓ claude    2.x (Claude Code) via oauth_token (subscription)
· rules     all: set; repos: suchna
```

Make sure the `github` line names **your** account.

Try one review by hand before turning on the listener. Use a PR of your own first,
since this posts real comments:

```bash
prk review https://github.com/Blitzscale-19/<repo>/pull/<n> --dry-run   # look up only
prk review https://github.com/Blitzscale-19/<repo>/pull/<n>             # real review
```

Then start listening:

```bash
prk
```

In a terminal this opens the [dashboard](#the-dashboard). Under systemd or with
output redirected, it prints plain log lines instead.

---

## 4. Run it as a service

To keep `prk` running in the background and start it on login (or on boot for a
server), use the systemd user service in this repo:

```bash
mkdir -p ~/.config/systemd/user
curl -fsSL -o ~/.config/systemd/user/prk.service \
  https://github.com/pankajyadav-blitz/prk/releases/latest/download/prk.service
systemctl --user daemon-reload
systemctl --user enable --now prk
loginctl enable-linger "$USER"     # servers: keep running after you log out
```

```bash
systemctl --user status prk        # is it running?
journalctl --user -u prk -f        # live log
systemctl --user restart prk       # after editing the config
```

**Windows:** there is no systemd. Leave `prk` running in a Windows Terminal tab, or start it at
login with Task Scheduler (it opens a console window you can minimize):

```bat
schtasks /Create /TN prk /SC ONLOGON /TR "\"%USERPROFILE%\.local\bin\prk.exe\""
schtasks /Run /TN prk        &REM start it now
schtasks /Delete /TN prk /F  &REM remove it
```

After `prk update`, close the running `prk` and start it again.

While the service runs, `prk -H`, `prk -c` and `prk --stats` still work from any
terminal: they read the same local database.

`prk.service` runs `~/.local/bin/prk` (where the installer puts it). If you built from
source, change `ExecStart=` to `%h/.cargo/bin/prk`. It sets a `PATH` with `~/.local/bin`,
`~/.cargo/bin` and `~/.bun/bin`; if `gh`, `git` or `claude` is installed somewhere else,
add that folder to the `Environment=PATH=` line. After `prk update`, run
`systemctl --user restart prk`.

---

## Commands

| Command | What it does |
|---|---|
| `prk` | Listen for review requests. In a terminal: the live dashboard. |
| `prk -H` | Review history (a scrollable screen in a terminal, a table when piped) |
| `prk -H <PR url>` | Every attempt for one PR, with result, who asked, time taken and log path |
| `prk -H --today` | Only today. Also `--since 12h` / `7d` / `2w`, `-r <repo>`, `-s <status>`, `-n <count>`, `-a` (no limit) |
| `prk -c` | The comments the most recent review posted |
| `prk -c <PR url>` | The comments the latest review of that PR posted, with follow-up verdicts |
| `prk -c <PR url> -a` | The comments of every review of that PR |
| `prk --stats` | Totals: reviewed today / 7 days, skipped, failed, average time, comments, verdicts, by repo |
| `prk review <PR url>` | Review one PR now, showing each step as it runs, then its comments |
| `prk review <PR url> -f` | A full review even if this commit was already reviewed (no follow-up) |
| `prk init` | Set prk up on this machine: Slack app, GitHub, Claude, config file. Re-run any time |
| `prk check` | Check the config and that Slack, catch-up, GitHub, ssh and Claude work |
| `prk setup-token` | Create a Claude subscription token and save it in the config |
| `prk update` | Update `prk` to the latest release |
| `prk clear` | Delete the local database (queue, history, saved comments), logs and leftover clones. Asks first; `--yes` skips the question. The config is kept. Stop a running `prk` first. |
| `-d`, `--dry-run` | Look PRs up and say what would happen; review and post nothing |
| `-v`, `--verbose` | Debug logging, including every git/gh command |
| `--config <path>` | Use a different config file |

Statuses: `waiting` (queued), `running`, `reviewed` (shown as `follow-up` for a
follow-up), `skipped` (with the reason), `failed` (after 3 attempts).

---

## The dashboard

`prk` in a terminal:

```
╭──────────────────────────────────────────────────────────────────────────────────────────╮
│prk  watching CPWCB54G2 for <@U0ANSHUMAN>             ● connected  caught up: 2 new  up 2h│
╰──────────────────────────────────────────────────────────────────────────────────────────╯
╭ Reviewing ───────────────────────────────────────────────────────────────────────────────╮
│⠹ naarad#285  admin report download  @c854526  hitesh                                     │
│  reviewing           ▰▰▰▰▰▰▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱  4m10s  step 3m58s                        │
╰──────────────────────────────────────────────────────────────────────────────────────────╯
╭ Queue (2) ───────────────────────────────────────────────────────────────────────────────╮
│• soochi_dash#870        queued today 11:41     U0BTNU87 (slack)                          │
│• otp_service#176        queued today 11:44     U0BTNU87 (catch-up)                       │
╰──────────────────────────────────────────────────────────────────────────────────────────╯
╭ Recent ◂ ────────────────────────────────────────────────────────────────────────────────╮
│✗ vartalap#504           failed      30m0s  today 11:05     timed out (attempt 1/3)       │
│↷ suchna#826             skipped        1s  today 10:12     PR is merged                  │
│✓ alfred#2014            follow-up   2m40s  today 09:58     follow-up: 1/2 fixed · 1 open │
│✓ suchna#819             reviewed    3m52s  today 09:30     3 comments                    │
╰──────────────────────────────────────────────────────────────────────────────────────────╯
╭ Log ─────────────────────────────────────────────────────────────────────────────────────╮
│11:44:02 queued otp_service#176 (asked by U0BTNU87, catch-up)                             │
╰──────────────────────────────────────────────────────────────────────────────────────────╯
 q quit  ↑↓ select  tab queue/recent  enter details  o open PR  R re-review  x drop  p pause  H history
```

| Key | Does |
|---|---|
| `↑` `↓` / `j` `k` | Move the selection |
| `tab` | Switch between Queue and Recent |
| `enter` | Details of the selected review and the comments it posted |
| `o` | Open the PR in the browser |
| `a` | Add PR links: paste one or more, `enter` queues them (same as `prk review <url>`, without leaving the dashboard). Pasting onto the dashboard opens the input by itself. `esc` cancels. |
| `R` | Review the selected PR again (a full review; asks first) |
| `x` | Drop the selected PR from the queue |
| `p` | Pause / resume the queue (the review in progress finishes) |
| `H` | Switch to the history screen (`esc` back) |
| `q` / `ctrl-c` | Quit. A review in progress is stopped and queued again for the next start. |

A finished review flashes green (or red) in Recent for a few seconds.

---

## History and comments

`prk -H` in a terminal opens a scrollable history:

| Key | Does |
|---|---|
| `↑` `↓`, `PgUp` `PgDn`, `g` `G` | Scroll |
| `/` | Search by repo, PR, title, author, status or reason (`esc` clears it) |
| `enter` | Details: result, who asked and when, time taken, attempts, log path, and every comment it posted with its follow-up verdict |
| `o` | Open the PR in the browser |
| `R` | Review the PR again. The running `prk` picks it up from the queue. |

Piped (`prk -H | less`) it prints a plain table instead.

`prk -c <PR url>` prints the comments grouped by file:

```
Blitzscale-19/suchna#823 · follow-up 916a2e0→a1b2c3d  today 11:42 · 3 comments
https://github.com/Blitzscale-19/suchna/pull/823

src/controllers/v1/troubleshoot.controller.ts
    :42  ⚠️ open   Handler isn't wrapped in asyncWrapper, so a rejected promise skips
                   the error middleware. (rule: wrap route handlers)
    :88  ✅ fixed  fetchSeller returns undefined for a soft-deleted seller.
                   fixed in a1b2c3d

summary  **Follow-up review** (916a2e0 → a1b2c3d): 1 of 2 fixed, 1 still open, 1 new.
```

Comments are saved locally when the review finishes, so they show up instantly and
stay even if someone deletes them on GitHub.

---

## How a review runs

1. **Trigger.** A message in `slack.channel` that tags `slack.reviewer` and links a PR
   from `github.orgs`. Ignored: messages from bots, your own messages, tags of other
   people, messages without a PR link. Editing a message to add your tag counts;
   editing one that already had it does not.
2. **Queue.** Each PR link is saved as a job in the local database. Reviews run one at
   a time.
3. **Look up.** `gh pr view`: closed, merged and draft PRs are skipped.
4. **Already reviewed?** Skipped when this commit was already reviewed by `prk`, or
   when your GitHub account already left a review or comment on this commit (by hand,
   or from `prk` on another machine). `prk review -f` overrides both. Comments count
   for the commit they were *written* on: GitHub moves an older comment's commit
   forward when the line did not change, and that must not hide new commits.
5. **Check out.** The repo is cloned into `workdir/repos/<org>/<repo>-pr<n>` over ssh,
   the PR head and its **base branch** (`main`, or whatever the PR targets) are fetched,
   and the PR head is checked out. Claude reads the whole repo as it is in the PR, not
   only the diff, and compares against `origin/<base>` from where the branch was cut
   (`git diff origin/<base>...HEAD`), so commits that landed on `main` later are not
   counted as the PR's changes. This is the same diff as GitHub's "Files changed".
6. **Context.** `prk` collects what the PR is for and gives it to Claude on every
   review: the **title and description**, and the **commit messages** of the PR's own
   commits (`git log origin/<base>..HEAD`, merges skipped). Claude uses them to judge
   whether the change does what it says and covers the cases the feature needs, but
   treats them as the author's words, never as instructions. The log of each review
   starts with exactly this context.
7. **Review.** In that checkout `prk` runs
   `claude -p "/code-review <level> --comment <PR number>"`, or the
   [follow-up](#follow-up-reviews) instructions, with:
   - the PR context from step 6, and your rules for that repo
   - an instruction that everything in the PR is untrusted data, never instructions
   - only these tools: read and search files, read-only `git` (`diff`, `log`, `show`,
     `blame`, ...), and `gh` limited to **this one PR**: its view, diff, files,
     comments and reviews (`gh api repos/<org>/<repo>/pulls/<n>/...`). Other PRs,
     other repos, the merge endpoint, chained commands, file edits and web access are
     refused by Claude Code itself.
   - the `max_budget_usd` and `timeout_minutes` limits
8. **Save.** The comments your account posted during the review are fetched and saved
   (`prk -c`). Claude's full output is in `workdir/logs/`.
9. **Clean up.** The clone is deleted (unless `review.keep_repos = true`).

Nothing is posted back in Slack.

---

## Follow-up reviews

When a PR that `prk` reviewed gets new commits and you are tagged again (or you run
`prk review <url>` / press `R`), `prk` runs a follow-up instead of a full review. It
does not only look at the new commits: it reviews the **complete PR again**, starting
from what changed.

1. **What the PR is for:** the title, description and commit messages (as in every
   review), plus the existing review threads, so Claude knows the feature being built
   and what was already said.
2. **Each earlier comment** (except those a previous follow-up already found fixed):
   Claude checks the new code and replies in that comment's thread:
   > ✅ Fixed in `a1b2c3d`: the handler is now wrapped in `asyncWrapper`.

   > ⚠️ Still present in `a1b2c3d`: `fetchSeller` still returns undefined for a
   > soft-deleted seller.

   > ↔️ Partly fixed in `a1b2c3d`: the null check was added, but the same pattern
   > remains in `updateTask`.
3. **The complete PR against its base branch** (`git diff origin/<base>...HEAD`):
   first what changed since the last review (`git log <last reviewed>..HEAD`, ignoring
   merges from `main`), including problems the fixes introduced; then the rest of the
   PR, for anything the earlier review missed. It checks that the change does what the
   description asks for, completely: missing cases, flows the feature needs but does
   not handle, behaviour that contradicts the description. New findings become inline
   comments. Nothing already said in an existing thread, by you or anyone else, is
   repeated.
4. **Summary:** one comment on the PR:
   > **Follow-up review** (`916a2e0` → `a1b2c3d`): 1 of 2 earlier findings fixed, 1 still
   > open, 0 partly fixed, 1 new finding.

```
main:   o───o───o───X───Y                    X, Y landed later: not the PR's changes
             \
PR:           a───b───A ───d───e───B          A = last reviewed, B = now
              └──────────────────┘
              whole PR reviewed vs the branch point, d + e looked at first
```

`prk` records each verdict on the saved comment, so `prk -c` and the details screen
show ✅ / ⚠️ / ↔️ next to it. Replies use the same `.../pulls/<n>/comments` endpoint
(with `in_reply_to`), so a follow-up needs no extra GitHub access. Threads are not
marked resolved; you or the author do that.

If the earlier commit is gone (the branch was force-pushed or rebased), `prk` does a
full review instead. Set `review.follow_up = false` to always do full reviews, or
`review.once_per_pr = true` to review each PR only once.

---

## Queue, catch-up and edge cases

| Case | What `prk` does |
|---|---|
| `prk` was off or started late | On start it reads **today's** messages (from 00:00, machine time) and queues every request that tags you (needs `slack.bot_token`) |
| Requests from before today | Not picked up on start. Ask again, or run `prk review <url>`. |
| Restarted several times today | Each start rescans the whole day; a message already turned into a job is recognised and not queued again |
| Same message seen live and by catch-up | Queued once (each Slack message becomes at most one job per PR) |
| Same PR tagged in several messages | Queued once while it is waiting or running |
| Commit already reviewed by `prk` | Skipped: `already reviewed at <commit>` |
| You already reviewed it on GitHub at this commit | Skipped: `already reviewed on GitHub` |
| New commits after a review | A [follow-up](#follow-up-reviews) |
| PR merged, closed or turned draft before its turn | Skipped with the reason |
| A review fails on a passing problem (network, gh, git, Claude error) | Retried after 5, then 10 minutes; `failed` after 3 attempts |
| A review times out, or hits `max_budget_usd` | `failed` at once, with a hint: a retry would end the same way and cost the same again |
| Claude posted some comments, then failed or was stopped | The posted comments are saved and the job is `reviewed` with "(cut short: ...)", so a retry never posts them twice |
| Claude usage limit | The queue pauses for 30 minutes; the PR waits without losing an attempt |
| `prk` stopped or crashed mid-review | Claude and everything it started are stopped; the job is queued again for the next start |
| ctrl-c during `prk review <url>` | Cancelled: Claude is stopped and the job is marked `skipped: cancelled`, not queued again |
| Request posted as a thread reply | Queued, live or by catch-up. Only a reply that tags you counts (a tagged parent does not cover untagged replies). The review note goes back into that thread. |
| Thread started before today, reply today | Caught up too: threads of the last 7 days are checked for replies from today (one `conversations.replies` call per active thread) |
| A PR link pasted in the dashboard | Queued like `prk review <url>`; see [the dashboard](#the-dashboard) |

---

## Review rules

Rules tell Claude what your team cares about. A violation in the changed code becomes
a finding, and the comment names the rule it breaks.

```toml
[rules]
all = """
- Applies to every repo.
"""

[rules.repos.suchna]          # the GitHub repo name
rules = """
- Every route handler is wrapped in asyncWrapper(...).
- Only DAOs touch Sequelize models.
"""

[rules.repos.alfred]
rules = """
- ...
"""
```

- A PR in `suchna` gets the `all` rules plus `[rules.repos.suchna]`.
- A repo with no table of its own gets only `all`.
- Repo names are matched without regard to case.
- Rules live only in your config, so a PR cannot change the rules it is reviewed against.

`config.example.toml` has a starting rule set for suchna, taken from its `CLAUDE.md`.

---

## Config reference

Top-level keys (like `workdir`) must come **before** the first `[table]`, or TOML files
them under that table. Unknown keys are an error, so a typo is caught instead of
being ignored.

| Key | Required | Default | What it is |
|---|---|---|---|
| `workdir` | | `~/.local/share/prk` | Clones, logs and the database (queue, history, comments) |
| **`[slack]`** | | | |
| `app_token` | ✅ | | Slack app-level token, `xapp-...`, scope `connections:write` |
| `channel` | | `CPWCB54G2` (`#peer-review`) | Channel id to watch |
| `reviewer` | ✅ | | Your Slack member id (`U0...`). Tags of this id start a review. |
| `bot_token` | | | Reads the channel for catch-up: the app's `xoxb-` token (`channels:history`), or an `xoxp-` user token |
| `catch_up` | | `true` | On every start, pick up today's review requests (from 00:00, machine time) |
| **`[github]`** | | | |
| `token` | | gh login | Fine-grained token; comments are posted as this account |
| `ssh_key` | | ssh setup | Private key for `git clone`/`fetch` |
| `ssh_host` | | `github.com` | Host in `git@<host>:org/repo.git`; an `~/.ssh/config` alias works |
| `orgs` | | `["Blitzscale-19"]` | PR links from other owners are ignored |
| **`[claude]`** | | | |
| `oauth_token` | | | Subscription token; `prk setup-token` fills it in |
| `api_key` | | | From console.anthropic.com (pay per token). Not with `oauth_token`. |
| `bin` | | `claude` | The Claude Code executable |
| `model` | | Claude Code default | Model for reviews |
| **`[review]`** | | | |
| `level` | | `medium` | `/code-review` depth: `low`, `medium`, `high`, `xhigh` |
| `max_budget_usd` | | `2` | Stop one review at this estimated cost. `0` = no cap. With an API key this is real money; with a subscription it caps review size. |
| `timeout_minutes` | | `30` | Kill a review still running after this long |
| `drafts` | | `false` | Review draft PRs too |
| `keep_repos` | | `false` | Keep clones between reviews (faster) instead of deleting each one (saves disk) |
| `follow_up` | | `true` | Follow-up reviews after new commits, instead of full reviews |
| `once_per_pr` | | `false` | Review each PR only once |
| `allowed_tools` | | built-in list | Tools Claude may use; `{owner}`, `{repo}`, `{number}` are filled in per PR |
| **`[rules]`** | | | |
| `all` | | | Rules for every repo |
| `[rules.repos.<repo>]` `rules` | | | Rules for one repo |

The full annotated example is `config.example.toml`.

---

## Files and paths

| Path | What |
|---|---|
| `~/.local/bin/prk` | The binary (install script; `~/.cargo/bin/prk` if built from source) |
| `~/.config/prk/config.toml` | Your config (`chmod 600`). Override with `PRK_CONFIG` or `--config`. |
| `~/.local/share/prk/state.sqlite3` | The queue, the history and the saved comments |
| `~/.local/share/prk/logs/<org>-<repo>-<n>-<commit>-<time>.log` | Claude's output for each review |
| `~/.local/share/prk/repos/<org>/<repo>-pr<n>` | The clone while a review runs; deleted afterwards |
| `~/.config/systemd/user/prk.service` | The service, if installed |

The `~/.local/share/prk` paths move with `workdir`. A full clone of a repo like suchna
is about 16 MB and takes around 10 seconds.

---

## Troubleshooting

| Problem | Fix |
|---|---|
| `holds tokens but other users can read it` | `chmod 600 ~/.config/prk/config.toml` |
| `unknown field ...` | A typo, or a top-level key placed after a `[table]` |
| `✗ slack ... invalid_auth` | `slack.app_token` must be the `xapp-` app-level token, not the `xoxb-` bot token |
| `✗ catch-up ... not_in_channel` | Run `prk init` again (it joins the channel), or `/invite @<your app>` in the channel |
| `✗ catch-up ... missing_scope` | Add the `channels:history` bot scope and reinstall the app |
| `could not reply in Slack: ... missing_scope` | Add the `chat:write` bot scope and reinstall the app (prk replies in the request thread when a review leaves comments) |
| Tagged, but nothing happens | Run `prk -d -v`. Check the app is invited to the channel, `message.channels` is subscribed, and `slack.reviewer` is your member id. Make sure no other process uses the same app token. |
| `✗ github` | Token expired, or not approved by the org yet. `prk check` names the account. |
| `✗ git ssh` | `ssh -T git@github.com` must greet you; set `github.ssh_key` |
| A review failed | `prk -H <PR url>` shows the reason and the log path. A refused `gh` command in the log means a tool was missing; add it to `review.allowed_tools`. |
| `Claude usage limit` | The plan's limit is used up; the queue resumes by itself after 30 minutes |
| `prk: command not found` | Add `~/.local/bin` (or `~/.cargo/bin` if built from source) to `PATH`, or rerun the install script |
