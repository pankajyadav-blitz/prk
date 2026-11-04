//! `prk init`: set prk up on this machine by answering a few questions.
//!
//! It checks the tools, walks through the Slack app (created from a prefilled
//! manifest, so no scopes or events to click through), joins the channel, finds
//! the reviewer's Slack id from their email, picks the GitHub account, creates
//! the Claude token, and writes a config with only those values, mode 600.
//! Every value is checked as it is entered and saved straight away. Running it
//! again keeps what already works and asks only for what is missing or broken.

use std::io::{IsTerminal, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use crossterm::style::Color;
use serde_json::{Value, json};

use crate::config::{self, Config, GithubCfg};
use crate::exec::Exec;
use crate::setup;
use crate::view::{bold, dim, paint};

const SCOPES: &[&str] = &[
    "channels:history",
    "channels:join",
    "users:read",
    "users:read.email",
];

pub fn run(path: &Path) -> Result<(), String> {
    if !std::io::stdin().is_terminal() {
        // Git Bash's own window (mintty) is not a console that Windows programs
        // can read from; winpty, which ships with Git Bash, bridges it.
        if std::env::var_os("MSYSTEM").is_some() {
            return Err(
                "prk init asks questions, which Git Bash's window cannot pass on. Run: winpty prk init\n\
                 (or run prk init in PowerShell, cmd or Windows Terminal)"
                    .to_string(),
            );
        }
        return Err("prk init asks questions; run it in a terminal".to_string());
    }
    let mut cfg = Draft::open(path)?;
    let name = app_name();
    let channel = cfg.get("slack", "channel");
    let channel = if channel.is_empty() {
        config::DEFAULT_CHANNEL.to_string()
    } else {
        channel
    };
    let claude_bin = config::program(
        &Some(cfg.get("claude", "bin"))
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| "claude".to_string()),
    );

    println!("{}", dim(&format!("config {}", path.display())));

    step(1, "Tools");
    tools(&claude_bin)?;

    step(2, "Slack");
    let app_token = cfg.get("slack", "app_token");
    let bot_token = cfg.get("slack", "bot_token");
    let app_ok = !app_token.is_empty() && crate::socket::open_connection(&app_token).is_ok();
    let bot = if bot_token.is_empty() {
        None
    } else {
        auth_test(&bot_token).ok()
    };
    if !app_ok || bot.is_none() {
        println!(
            "Create your own Slack app (one per person). Open this link, pick the workspace and click {}:\n\n  {}\n",
            bold("Create"),
            manifest_link(&name)?
        );
    }
    if app_ok {
        ok("app token works (kept)");
    } else {
        println!(
            "On the app's {} page, under {}, click {}: any name, scope {}, then {}.",
            bold("Basic Information"),
            bold("App-Level Tokens"),
            bold("Generate Token and Scopes"),
            bold("connections:write"),
            bold("Generate")
        );
        let token = secret_until("App-level token (xapp-...)", |t| {
            if !t.starts_with("xapp-") {
                return Err("that is not an xapp- token".to_string());
            }
            crate::socket::open_connection(t).map(|_| "app token works".to_string())
        })?;
        cfg.set("slack", "app_token", &token)?;
    }
    let (bot_token, bot) = match bot {
        Some(bot) => {
            ok(&format!("bot token works: {} (kept)", bot.describe()));
            (bot_token, bot)
        }
        None => {
            println!(
                "In the left menu, open {}, click {}, then {}. Copy the {}.",
                bold("Install App"),
                bold("Install to Workspace"),
                bold("Allow"),
                bold("Bot User OAuth Token")
            );
            let mut found = None;
            let token = secret_until("Bot token (xoxb-...)", |t| {
                if !t.starts_with("xoxb-") {
                    return Err("that is not an xoxb- bot token".to_string());
                }
                let bot = auth_test(t)?;
                let line = format!("bot token works: {}", bot.describe());
                found = Some(bot);
                Ok(line)
            })?;
            cfg.set("slack", "bot_token", &token)?;
            (token, found.expect("set when the token was accepted"))
        }
    };
    let missing: Vec<&str> = SCOPES
        .iter()
        .copied()
        .filter(|s| !bot.scopes.iter().any(|have| have == s))
        .collect();
    if !missing.is_empty() {
        warn(&format!(
            "the app lacks {}; prk works without them, but add them under OAuth & Permissions \
             and reinstall to skip the manual steps below",
            missing.join(", ")
        ));
    }
    match slack(&bot_token, "conversations.join", &[("channel", &channel)]) {
        Ok(_) => ok(&format!("the app is in channel {channel}")),
        Err(e) => {
            warn(&format!("could not join {channel} ({e})"));
            println!("  In the channel, run: /invite @{}", bot.user);
            pause("Press Enter once it is invited")?;
        }
    }
    let reviewer = reviewer(&bot_token, &cfg.get("slack", "reviewer"))?;
    cfg.set("slack", "reviewer", &reviewer)?;

    step(3, "GitHub");
    let token = github(&cfg.get("github", "token"), &name)?;
    cfg.set("github", "token", &token)?;
    ssh(
        &cfg.get("github", "ssh_key"),
        &cfg.get("github", "ssh_host"),
    )?;

    step(4, "Claude");
    claude(&mut cfg, &claude_bin)?;

    step(5, "Check");
    let loaded = Config::load(path)?;
    let failed = crate::check(&loaded);
    println!();
    if failed == 0 {
        println!("{}", paint("prk is set up.", Color::Green));
    } else {
        println!(
            "{}",
            paint(
                "Something above failed. Fix it and run `prk init` again; working values are kept.",
                Color::Red
            )
        );
    }
    println!(
        "\nNext:\n  \
         prk review <PR url> --dry-run   look a PR up, post nothing\n  \
         prk review <PR url>             a real review; try it on your own PR first\n  \
         prk                             start listening for tags in Slack\n\n\
         Review rules and every other option: {}",
        dim("config.example.toml in the release, or the README's Config reference")
    );
    Ok(())
}

/// The config file's text, updated and saved one value at a time so an
/// interrupted init keeps what was already entered.
struct Draft<'a> {
    path: &'a Path,
    text: String,
    table: toml::Table,
}

impl<'a> Draft<'a> {
    fn open(path: &'a Path) -> Result<Draft<'a>, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let table = toml::from_str(&text).map_err(|e| {
            format!(
                "{}: {e}\nFix it, or move it away to start fresh.",
                path.display()
            )
        })?;
        Ok(Draft { path, text, table })
    }

    fn get(&self, table: &str, key: &str) -> String {
        self.table
            .get(table)
            .and_then(|t| t.get(key))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    }

    fn set(&mut self, table: &str, key: &str, value: &str) -> Result<(), String> {
        if self.get(table, key) == value && setup::has_key(&self.text, table, key) {
            return Ok(());
        }
        if self.text.trim().is_empty() {
            self.text =
                "# prk config, written by `prk init`. Anything not set here takes its default;\n\
                         # config.example.toml lists every option.\n"
                    .to_string();
        }
        self.text = setup::set_key(&self.text, table, key, value);
        self.table = toml::from_str(&self.text).map_err(|e| format!("internal: {e}"))?;
        setup::write_private(self.path, &self.text)
    }
}

fn step(n: u32, title: &str) {
    println!("\n{}", bold(&format!("[{n}/5] {title}")));
}

fn ok(text: &str) {
    println!("{} {text}", paint("✓", Color::Green));
}

fn warn(text: &str) {
    println!("{} {text}", paint("!", Color::Yellow));
}

fn fail(text: &str) {
    println!("{} {}", paint("✗", Color::Red), paint(text, Color::Red));
}

fn read_line(prompt: &str) -> Result<String, String> {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let n = std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Err("stopped (end of input)".to_string());
    }
    Ok(line.trim().to_string())
}

fn ask(question: &str, default: &str) -> Result<String, String> {
    let answer = if default.is_empty() {
        read_line(&format!("{question}: "))?
    } else {
        read_line(&format!("{question} [{default}]: "))?
    };
    Ok(if answer.is_empty() {
        default.to_string()
    } else {
        answer
    })
}

fn confirm(question: &str) -> Result<bool, String> {
    let answer = read_line(&format!("{question} [Y/n]: "))?;
    Ok(!matches!(answer.to_lowercase().as_str(), "n" | "no"))
}

fn pause(question: &str) -> Result<(), String> {
    read_line(&format!("{question}: ")).map(|_| ())
}

/// Ask for a secret (hidden) until `check` accepts it. Empty input gives up.
fn secret_until(
    question: &str,
    mut check: impl FnMut(&str) -> Result<String, String>,
) -> Result<String, String> {
    loop {
        let value = rpassword::prompt_password(format!("{question}, hidden: "))
            .map_err(|e| format!("could not read it: {e}"))?;
        let value = value.trim();
        if value.is_empty() {
            return Err(format!(
                "stopped: no {question} given. Run `prk init` again to continue."
            ));
        }
        match check(value) {
            Ok(line) => {
                ok(&line);
                return Ok(value.to_string());
            }
            Err(e) => fail(&format!("{e}; try again, or press Enter to stop")),
        }
    }
}

/// `prk-<login name>`, used for the Slack app and the GitHub token.
fn app_name() -> String {
    let user: String = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(24)
        .collect();
    if user.is_empty() {
        "prk".to_string()
    } else {
        format!("prk-{user}")
    }
}

fn tools(claude_bin: &str) -> Result<(), String> {
    let tools = [
        ("git", "git", "your package manager"),
        ("gh", "gh", "https://cli.github.com"),
        (
            "claude",
            claude_bin,
            "npm install -g @anthropic-ai/claude-code",
        ),
    ];
    let mut missing = 0;
    for (name, bin, how) in tools {
        let out = Command::new(bin)
            .arg("--version")
            .stdin(Stdio::null())
            .output();
        match out {
            Ok(out) if out.status.success() => {
                let version = String::from_utf8_lossy(&out.stdout);
                ok(&format!(
                    "{name}: {}",
                    version.lines().next().unwrap_or("").trim()
                ))
            }
            _ => {
                missing += 1;
                fail(&format!("{name} is not installed: get it from {how}"));
            }
        }
    }
    if missing > 0 {
        return Err("install the missing tools, then run `prk init` again".to_string());
    }
    Ok(())
}

/// A link that opens Slack's "create app" page with everything prk needs
/// filled in: Socket Mode, the bot scopes and the message.channels event.
fn manifest_link(name: &str) -> Result<String, String> {
    let manifest = json!({
        "display_information": {
            "name": name,
            "description": "Reviews GitHub PRs with Claude Code when its owner is tagged in Slack",
        },
        "features": { "bot_user": { "display_name": name, "always_online": false } },
        "oauth_config": { "scopes": { "bot": SCOPES } },
        "settings": {
            "event_subscriptions": { "bot_events": ["message.channels"] },
            "interactivity": { "is_enabled": false },
            "org_deploy_enabled": false,
            "socket_mode_enabled": true,
            "token_rotation_enabled": false,
        },
    });
    reqwest::Url::parse_with_params(
        "https://api.slack.com/apps",
        &[("new_app", "1"), ("manifest_json", &manifest.to_string())],
    )
    .map(String::from)
    .map_err(|e| e.to_string())
}

/// Call a Slack Web API method; the response when `ok`, else Slack's error.
fn slack(token: &str, method: &str, form: &[(&str, &str)]) -> Result<Value, String> {
    slack_with_headers(token, method, form).map(|(v, _)| v)
}

fn slack_with_headers(
    token: &str,
    method: &str,
    form: &[(&str, &str)],
) -> Result<(Value, reqwest::header::HeaderMap), String> {
    let http = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = http
        .post(format!("https://slack.com/api/{method}"))
        .bearer_auth(token)
        .form(form)
        .send()
        .map_err(|e| format!("could not reach Slack: {e}"))?;
    let headers = resp.headers().clone();
    let data: Value = resp.json().map_err(|e| format!("{method}: {e}"))?;
    if data.get("ok").and_then(Value::as_bool) != Some(true) {
        let err = data
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let needed = data.get("needed").and_then(Value::as_str).unwrap_or("");
        return Err(if needed.is_empty() {
            format!("{method}: {err}")
        } else {
            format!("{method}: {err} (needs {needed})")
        });
    }
    Ok((data, headers))
}

struct Bot {
    user: String,
    team: String,
    scopes: Vec<String>,
}

impl Bot {
    fn describe(&self) -> String {
        format!("@{} in {}", self.user, self.team)
    }
}

fn auth_test(token: &str) -> Result<Bot, String> {
    let (data, headers) = slack_with_headers(token, "auth.test", &[])?;
    let s = |k: &str| {
        data.get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let scopes = headers
        .get("x-oauth-scopes")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    Ok(Bot {
        user: s("user"),
        team: s("team"),
        scopes,
    })
}

fn display_name(user: &Value) -> String {
    let profile = |k: &str| {
        user.pointer(&format!("/profile/{k}"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    [profile("real_name"), profile("display_name")]
        .into_iter()
        .find(|n| !n.is_empty())
        .unwrap_or_else(|| {
            user.get("name")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string()
        })
}

/// The Slack member id whose tag starts a review: kept if already set, else
/// looked up from the email, else pasted.
fn reviewer(bot_token: &str, current: &str) -> Result<String, String> {
    if current.starts_with('U') || current.starts_with('W') {
        let name = slack(bot_token, "users.info", &[("user", current)])
            .ok()
            .and_then(|d| d.get("user").map(display_name))
            .map(|n| format!("{n} ({current})"))
            .unwrap_or_else(|| current.to_string());
        ok(&format!("a tag of {name} starts a review (kept)"));
        return Ok(current.to_string());
    }
    let git_email = Command::new("git")
        .args(["config", "user.email"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    println!("Whose tag should start a review? Usually yours.");
    loop {
        let email = ask("Your Slack email", &git_email)?;
        if email.is_empty() {
            break;
        }
        match slack(bot_token, "users.lookupByEmail", &[("email", &email)]) {
            Ok(data) => {
                let user = data.get("user").cloned().unwrap_or(Value::Null);
                let id = user
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if confirm(&format!("Found {} ({id}). Use it?", display_name(&user)))? {
                    ok(&format!("a tag of {} starts a review", display_name(&user)));
                    return Ok(id);
                }
            }
            Err(e) if e.contains("users_not_found") => fail("no Slack member with that email"),
            Err(e) => {
                fail(&format!("could not look it up ({e})"));
                break;
            }
        }
    }
    println!("Paste the member id instead: Slack profile -> ⋮ -> Copy member ID.");
    loop {
        let id = ask("Member id (U...)", "")?;
        if id.starts_with('U') || id.starts_with('W') {
            return Ok(id);
        }
        fail("a member id starts with U or W");
    }
}

/// The value for github.token: kept if it works, "" for the gh login, or a new
/// fine-grained token.
fn github(current: &str, name: &str) -> Result<String, String> {
    if !current.is_empty() {
        match gh_login(Some(current)) {
            Ok(login) => {
                ok(&format!(
                    "token works; comments are posted as {login} (kept)"
                ));
                return Ok(current.to_string());
            }
            Err(e) => fail(&format!("the saved token does not work ({e})")),
        }
    }
    if let Ok(login) = gh_login(None)
        && confirm(&format!(
            "Post review comments as {login}, with your `gh` login?"
        ))?
    {
        ok(&format!("comments are posted as {login} (gh login)"));
        return Ok(String::new());
    }
    let org = GithubCfg::default().orgs[0].clone();
    let link = reqwest::Url::parse_with_params(
        "https://github.com/settings/personal-access-tokens/new",
        &[
            ("name", name),
            ("description", "prk: posts PR review comments"),
            ("target_name", &org),
            ("pull_requests", "write"),
            ("contents", "read"),
        ],
    )
    .map_err(|e| e.to_string())?;
    println!(
        "Create a fine-grained token:\n\n  {link}\n\n\
         Check: resource owner {org}, repository access: the repos you review (or all),\n\
         permissions Pull requests: Read and write, Contents: Read-only. Then Generate token.\n\
         {}",
        dim("If the org must approve it, it works once an admin approves.")
    );
    secret_until("GitHub token (github_pat_...)", |t| {
        gh_login(Some(t)).map(|login| format!("comments will be posted as {login}"))
    })
}

/// The GitHub login of `token`, or of the gh login when None.
fn gh_login(token: Option<&str>) -> Result<String, String> {
    let mut cmd = Command::new("gh");
    cmd.args(["api", "user", "--jq", ".login"])
        .stdin(Stdio::null())
        .env("GH_PROMPT_DISABLED", "1");
    match token {
        Some(t) => cmd.env("GH_TOKEN", t),
        None => cmd.env_remove("GH_TOKEN").env_remove("GITHUB_TOKEN"),
    };
    let login = crate::output(cmd)?;
    if login.is_empty() {
        return Err("gh api user returned no login".to_string());
    }
    Ok(login)
}

/// git clones over ssh; make sure GitHub accepts this machine's key.
fn ssh(key: &str, host: &str) -> Result<(), String> {
    let mut github = GithubCfg {
        ssh_key: key.to_string(),
        ..GithubCfg::default()
    };
    if !host.is_empty() {
        github.ssh_host = host.to_string();
    }
    let probe_url = format!("git@{}:{}/prk-probe.git", github.ssh_host, github.orgs[0]);
    let host = github.ssh_host.clone();
    let exec = Exec {
        cfg: Config {
            github,
            ..Config::default()
        },
    };
    let probe = || {
        let mut cmd = Command::new("git");
        cmd.args(["ls-remote", "--heads", &probe_url]);
        crate::probe_ssh(exec.env_for(cmd), &host)
    };
    loop {
        match probe() {
            Ok(line) => {
                ok(&format!("git: {line}"));
                return Ok(());
            }
            Err(e) => {
                fail(&format!("git cannot reach GitHub over ssh: {e}"));
                println!(
                    "  Add an ssh key to your GitHub account, for example:\n    \
                     ssh-keygen -t ed25519            # skip if .ssh/id_ed25519 exists in your home folder\n    \
                     gh ssh-key add <home>/.ssh/id_ed25519.pub   # or paste it at https://github.com/settings/keys"
                );
                if !confirm("Check again?")? {
                    warn("skipped; reviews fail to clone until this works");
                    return Ok(());
                }
            }
        }
    }
}

fn claude(cfg: &mut Draft, claude_bin: &str) -> Result<(), String> {
    if !cfg.get("claude", "oauth_token").is_empty() {
        ok("subscription token set (kept; `prk setup-token` replaces it)");
        return Ok(());
    }
    if !cfg.get("claude", "api_key").is_empty() {
        ok("API key set (kept)");
        return Ok(());
    }
    println!(
        "prk runs Claude Code with your Claude subscription. A token from `claude setup-token`\n\
         lasts a year and works in the background; a normal `claude` login expires sooner."
    );
    if confirm("Create the token now (opens a browser)?")? {
        let token = setup::create_token(claude_bin)?;
        cfg.set("claude", "oauth_token", &token)?;
        ok("saved the subscription token");
        return Ok(());
    }
    let status = Command::new(claude_bin)
        .args(["auth", "status"])
        .stdin(Stdio::null())
        .output();
    let logged_in = status
        .ok()
        .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
        .and_then(|v| v.get("loggedIn").and_then(Value::as_bool))
        .unwrap_or(false);
    if logged_in {
        ok("using this machine's `claude` login");
    } else {
        warn("no Claude login: run `claude` once to log in, or `prk setup-token`");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_link_carries_socket_mode_scopes_and_the_event() {
        let link = manifest_link("prk-test").unwrap();
        let url = reqwest::Url::parse(&link).unwrap();
        let manifest = url
            .query_pairs()
            .find(|(k, _)| k == "manifest_json")
            .map(|(_, v)| v.into_owned())
            .unwrap();
        let m: Value = serde_json::from_str(&manifest).unwrap();
        assert_eq!(m["settings"]["socket_mode_enabled"], true);
        assert_eq!(
            m["settings"]["event_subscriptions"]["bot_events"][0],
            "message.channels"
        );
        assert_eq!(
            m["oauth_config"]["scopes"]["bot"].as_array().unwrap().len(),
            4
        );
        assert_eq!(m["display_information"]["name"], "prk-test");
    }

    #[test]
    fn draft_writes_a_private_minimal_config_that_loads() {
        let dir = std::env::temp_dir().join(format!("prk-init-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("prk/config.toml");
        let mut d = Draft::open(&path).unwrap();
        assert_eq!(d.get("slack", "app_token"), "");
        d.set("slack", "app_token", "xapp-1").unwrap();
        d.set("slack", "bot_token", "xoxb-1").unwrap();
        d.set("slack", "reviewer", "U123").unwrap();
        d.set("github", "token", "").unwrap();
        d.set("claude", "oauth_token", "sk-ant-oat01-x").unwrap();
        d.set("slack", "app_token", "xapp-2").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let cfg = Config::load(&path).unwrap();
        cfg.check_private().unwrap();
        assert_eq!(cfg.slack.app_token, "xapp-2");
        assert_eq!(cfg.slack.channel, config::DEFAULT_CHANNEL);
        assert_eq!(cfg.slack.reviewer, "U123");
        assert_eq!(cfg.claude.oauth_token, "sk-ant-oat01-x");
        // reopened, the values come back, and the header is written once
        let d = Draft::open(&path).unwrap();
        assert_eq!(d.get("slack", "bot_token"), "xoxb-1");
        assert_eq!(d.text.matches("written by `prk init`").count(), 1);
        assert_eq!(d.text.matches("[slack]").count(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn draft_keeps_an_existing_configs_rules_and_comments() {
        let dir = std::env::temp_dir().join(format!("prk-init-keep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let example = include_str!("../config.example.toml");
        std::fs::write(&path, example).unwrap();
        let mut d = Draft::open(&path).unwrap();
        d.set("slack", "app_token", "xapp-new").unwrap();
        d.set("slack", "reviewer", "U999").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), example.lines().count());
        assert!(text.contains("[rules.repos.suchna]"));
        assert!(text.contains("# Channel id to watch."));
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.slack.app_token, "xapp-new");
        assert_eq!(cfg.slack.reviewer, "U999");
        assert!(cfg.rules_for("suchna").unwrap().contains("asyncWrapper"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Opt-in: needs a gh login and an ssh key GitHub accepts on this machine.
    /// cargo test -- --ignored live_github
    #[test]
    #[ignore]
    fn live_github_login_ssh_and_bad_tokens() {
        assert!(!gh_login(None).unwrap().is_empty());
        assert!(gh_login(Some("github_pat_not_a_real_token")).is_err());
        ssh("", "").unwrap();
        assert!(auth_test("xoxb-not-a-real-token").is_err());
        assert!(crate::socket::open_connection("xapp-not-a-real-token").is_err());
    }
}
