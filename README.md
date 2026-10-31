# prk

`prk` (short for *parikshak*, examiner) reviews GitHub pull requests with Claude Code
when someone tags you in a Slack channel.

Someone posts in `#peer-review`:

> @you please review: https://github.com/Blitzscale-19/suchna/pull/826

`prk` sees the tag, checks out the PR, reviews it with Claude Code's `/code-review`,
and posts the findings on the PR as inline comments **from your GitHub account**.

Everything it needs (your Slack id, your GitHub token, your Claude token, review rules
per repo) lives in one config file, so anyone can run their own copy on their own
machine or on a server.

---

## Contents

1. [Install](#1-install)
2. [Create the Slack app](#2-create-the-slack-app)
3. [Create the GitHub token](#3-create-the-github-token)
4. [Get a Claude token](#4-get-a-claude-token)
5. [Create the config file](#5-create-the-config-file)
6. [Check and run](#6-check-and-run)
7. [Run it as a service](#7-run-it-as-a-service)
8. [Config reference](#config-reference)
9. [Review rules](#review-rules)
10. [How a review runs](#how-a-review-runs)
11. [Files and paths](#files-and-paths)
12. [Troubleshooting](#troubleshooting)

---

## 1. Install

You need these on the machine that runs `prk`:

| Tool | Check | Install |
|---|---|---|
| Rust (cargo) | `cargo --version` | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| git | `git --version` | your package manager |
| GitHub CLI | `gh --version` | https://cli.github.com |
| Claude Code | `claude --version` | `npm install -g @anthropic-ai/claude-code` |

Then build and install `prk`:

```bash
git clone <this repo> ~/prk
cd ~/prk
cargo install --path .
```

This puts the binary at `~/.cargo/bin/prk`. With `~/.cargo/bin` on your `PATH` (rustup
adds it), you can run `prk` from any directory, the same way as `sts`:

```bash
prk --help
```

To update later: `git pull && cargo install --path .`

---

## 2. Create the Slack app

`prk` listens to Slack through **Socket Mode**, which needs a Slack app.

> **One running `prk` per Slack app.** Slack spreads events across every connection
> that uses the same app token, so two processes on one app each see only some of the
> messages. Give each person's `prk` its own app, and don't share one with another bot.

1. Go to https://api.slack.com/apps → **Create New App** → **From scratch**. Name it,
   for example `prk-<yourname>`, and pick the workspace.
2. **Socket Mode** (left menu) → turn **Enable Socket Mode** on. When asked for an
   app-level token, name it `prk` and keep the `connections:write` scope. Copy the
   token: it starts with `xapp-`. This is `slack.app_token`.
3. **OAuth & Permissions** → **Bot Token Scopes** → add `channels:history`
   (and `groups:history` if the channel is private).
4. **Event Subscriptions** → turn **Enable Events** on → **Subscribe to bot events** →
   add `message.channels` (and `message.groups` for a private channel) → **Save Changes**.
5. **Install App** → **Install to Workspace** → Allow. Reinstall whenever Slack asks
   after a scope change.
6. In Slack, open the channel (`#peer-review`) and run:
   ```
   /invite @prk-<yourname>
   ```
7. Get the channel id: open the channel → click its name at the top → the id
   (`C0...`) is at the bottom of the About tab. For `#peer-review` it is `CPWCB54G2`.
   This is `slack.channel`.
8. Get **your own** member id: click your profile picture → **Profile** → **⋮** →
   **Copy member ID** (`U0...`). This is `slack.reviewer`. Messages that tag this id
   start a review.

---

## 3. Create the GitHub token

Review comments are posted with this token, so they appear as **your** account.

1. https://github.com/settings/personal-access-tokens → **Generate new token**
   (fine-grained).
2. **Resource owner**: `Blitzscale-19`. **Repository access**: All repositories, or
   the ones you review.
3. **Permissions → Repository permissions**:
   - **Pull requests: Read and write** (read the PR, post review comments)
   - **Contents: Read-only** (so the token can never merge or push)
   - Metadata: Read-only (added automatically)
4. Generate and copy it (`github_pat_...`). This is `github.token`.

If the org requires approval for fine-grained tokens, the token works once an org
admin approves it.

**SSH key** for cloning: any key already added to your GitHub account
(https://github.com/settings/keys) works. Put its path in `github.ssh_key`, for
example `~/.ssh/id_ed25519`. Leave it empty to use your normal ssh setup. Check it
with `ssh -T git@github.com`.

---

## 4. Get a Claude token

Pick one:

| Option | How | Cost |
|---|---|---|
| **Subscription token** (recommended) | Run `claude setup-token`, log in, copy the `sk-ant-oat01-...` token | Uses your Claude Pro/Max/Team plan's usage limits. No per-review charge. |
| API key | https://console.anthropic.com → API Keys (`sk-ant-api03-...`) | Billed per token used |
| Nothing | Leave both empty | Uses whoever is logged in with `claude` on this machine |

Put the token in `claude.oauth_token` **or** the key in `claude.api_key`, not both.

With a subscription, reviews share your plan's limits with your own Claude Code use.
When the limit is reached, `prk` pauses for 30 minutes and retries the same PR.

---

## 5. Create the config file

The config lives at `~/.config/prk/config.toml`. To keep it somewhere else, set
`PRK_CONFIG=/path/to/config.toml` or pass `--config /path/to/config.toml`.

**Option A: copy the full example** (all settings, comments, and a starting rule set
for suchna):

```bash
mkdir -p ~/.config/prk
cp ~/prk/config.example.toml ~/.config/prk/config.toml
chmod 600 ~/.config/prk/config.toml
nano ~/.config/prk/config.toml
```

**Option B: create a minimal one** with only the required fields. Replace every value
in `<...>`:

```bash
mkdir -p ~/.config/prk
cat > ~/.config/prk/config.toml <<'EOF'
[slack]
app_token = "<xapp-... from step 2>"
channel   = "CPWCB54G2"
reviewer  = "<your Slack member id, U0...>"

[github]
token   = "<github_pat_... from step 3>"
ssh_key = "~/.ssh/id_ed25519"

[claude]
oauth_token = "<sk-ant-oat01-... from step 4>"

[rules]
all = """
- Flag any secret, token, password or private key committed in code or config.
"""
EOF
chmod 600 ~/.config/prk/config.toml
```

`chmod 600` is required: the file holds tokens, and `prk` refuses to start while other
users can read it.

---

## 6. Check and run

```bash
prk check
```

Every line should say `ok`:

```
config   /home/you/.config/prk/config.toml
ok    slack    app token works; watching CPWCB54G2 for <@U0XXXXXXX>
ok    github   comments will be posted as your-github-login
ok    git ssh  ssh to github.com works
ok    claude   2.x (Claude Code) via oauth_token (subscription)
rules    all: set; repos: suchna
```

Make sure the `github` line names **your** account.

Then try one review by hand before turning on the listener. Use a PR of your own
first, since this posts real comments:

```bash
prk review https://github.com/Blitzscale-19/<repo>/pull/<n> --dry-run   # look up only
prk review https://github.com/Blitzscale-19/<repo>/pull/<n>             # real review
```

Start listening:

```bash
prk                    # listen; ctrl-c to stop
prk --dry-run -v       # log what would be reviewed, review nothing
```

All commands:

| Command | What it does |
|---|---|
| `prk` | Listen in the Slack channel and review PRs when you are tagged |
| `prk check` | Check the config and that Slack, GitHub, ssh and Claude work |
| `prk review <PR url>` | Review one PR now, without Slack |
| `prk review <PR url> --force` | Review again even if this commit was already reviewed |
| `--dry-run` | Look PRs up and log what would happen; run and post nothing |
| `-v`, `--verbose` | Debug logging, including every git/gh command |
| `--config <path>` | Use a different config file |

---

## 7. Run it as a service

To keep `prk` running in the background and start it on login (or on boot for a
server), use the systemd user service in this repo:

```bash
mkdir -p ~/.config/systemd/user
cp ~/prk/prk.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now prk
loginctl enable-linger "$USER"     # servers: keep running after you log out
```

```bash
systemctl --user status prk        # is it running?
journalctl --user -u prk -f        # live log
systemctl --user restart prk       # after editing the config
```

`prk.service` sets a `PATH` with `~/.cargo/bin`, `~/.local/bin` and `~/.bun/bin`.
If `gh`, `git` or `claude` is installed somewhere else, add that folder to the
`Environment=PATH=` line.

---

## Config reference

Top-level keys (like `workdir`) must come **before** the first `[table]`, or TOML files
them under that table. Unknown keys are an error, so a typo is caught instead of
being ignored.

| Key | Required | Default | What it is |
|---|---|---|---|
| `workdir` | | `~/.local/share/prk` | Repo clones, logs and the state database |
| **`[slack]`** | | | |
| `app_token` | ✅ | | Slack app-level token, `xapp-...`, scope `connections:write` |
| `channel` | ✅ | | Channel id to watch, e.g. `CPWCB54G2` for `#peer-review` |
| `reviewer` | ✅ | | Your Slack member id (`U0...`). Tags of this id start a review. |
| **`[github]`** | | | |
| `token` | | gh login | Fine-grained token; comments are posted as this account |
| `ssh_key` | | ssh setup | Private key for `git clone`/`fetch` |
| `ssh_host` | | `github.com` | Host in `git@<host>:org/repo.git`; an `~/.ssh/config` alias works |
| `orgs` | | `["Blitzscale-19"]` | PR links from other owners are ignored |
| **`[claude]`** | | | |
| `oauth_token` | | | From `claude setup-token` (subscription) |
| `api_key` | | | From console.anthropic.com (pay per token). Not with `oauth_token`. |
| `bin` | | `claude` | The Claude Code executable |
| `model` | | Claude Code default | Model for reviews |
| **`[review]`** | | | |
| `level` | | `medium` | `/code-review` depth: `low`, `medium`, `high`, `xhigh` |
| `max_budget_usd` | | `2` | Stop one review at this estimated cost. `0` = no cap. With an API key this is real money; with a subscription it caps review size. |
| `timeout_minutes` | | `30` | Kill a review still running after this long |
| `drafts` | | `false` | Review draft PRs too |
| `allowed_tools` | | built-in list | Tools Claude may use; see [How a review runs](#how-a-review-runs) |
| **`[rules]`** | | | |
| `all` | | | Rules for every repo |
| `[rules.repos.<repo>]` `rules` | | | Rules for one repo |

The full annotated example is `config.example.toml`.

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

## How a review runs

1. **Trigger.** A message in `slack.channel` that tags `slack.reviewer` and links a PR
   from `github.orgs`. Ignored: messages from bots, your own messages, tags of other
   people, messages without a PR link. Editing a message to add your tag counts;
   editing one that already had it does not.
2. **Queue.** Every PR link in the message is queued. Reviews run one at a time.
3. **Look up.** `gh pr view`: closed, merged and draft PRs are skipped. A commit that
   was already reviewed is skipped; after a new push the PR is reviewed again.
4. **Check out.** The repo is cloned once into `workdir/repos/<org>/<repo>` over ssh.
   Each review fetches the PR head and its base branch and checks out the PR head, so
   Claude reads the whole repo as it is in the PR, not only the diff.
5. **Review.** In that checkout `prk` runs
   `claude -p "/code-review <level> --comment <PR number>"` with:
   - your rules for that repo
   - an instruction that everything in the PR is untrusted data, never instructions
   - only these tools: read and search files, read-only `git` (`diff`, `log`, `show`,
     `blame`, ...), and `gh` limited to **this one PR**: its view, diff, files,
     comments and reviews (`gh api repos/<org>/<repo>/pulls/<n>/...`). Other PRs,
     other repos, the merge endpoint, file edits and web access are refused.
   - the `max_budget_usd` and `timeout_minutes` limits
6. **Result.** Findings appear on the PR as inline comments from your account.
   Claude's full output goes to `workdir/logs/`. A failed review is not marked done,
   so the next tag retries it.

Nothing is posted back in Slack.

---

## Files and paths

| Path | What |
|---|---|
| `~/.cargo/bin/prk` | The binary (`cargo install --path .`) |
| `~/.config/prk/config.toml` | Your config (`chmod 600`). Override with `PRK_CONFIG` or `--config`. |
| `~/.local/share/prk/repos/<org>/<repo>` | Kept clones, reused between reviews |
| `~/.local/share/prk/logs/<org>-<repo>-<n>-<commit>.log` | Claude's output for each review |
| `~/.local/share/prk/state.sqlite3` | Which PR commits were already reviewed |
| `~/.config/systemd/user/prk.service` | The service, if installed |

The `~/.local/share/prk` paths move with `workdir`.

---

## Troubleshooting

| Problem | Fix |
|---|---|
| `holds tokens but other users can read it` | `chmod 600 ~/.config/prk/config.toml` |
| `unknown field ...` | A typo, or a top-level key placed after a `[table]` |
| `FAIL slack ... invalid_auth` | `slack.app_token` must be the `xapp-` app-level token, not the `xoxb-` bot token |
| Tagged, but nothing happens | Run `prk --dry-run -v`. Check the app is invited to the channel, `message.channels` is subscribed, and `slack.reviewer` is your member id. Make sure no other process uses the same app token. |
| `FAIL github` | Token expired, or not approved by the org yet. `prk check` names the account. |
| `FAIL git ssh` | `ssh -T git@github.com` must greet you; set `github.ssh_key` |
| Review failed | Open the newest file in `~/.local/share/prk/logs/`. A refused `gh` command there means `/code-review` needed a tool that isn't allowed; add it to `review.allowed_tools`. |
| `usage limit reached` | The Claude plan's limit is used up; `prk` retries after 30 minutes |
| `prk: command not found` | Add `~/.cargo/bin` to `PATH`, or run `cargo install --path .` again |
