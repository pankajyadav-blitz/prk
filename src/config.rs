//! config.toml: credentials, review settings and per-repo review rules, all in
//! one file so anyone can run the bot by filling in their own copy.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub slack: SlackCfg,
    pub github: GithubCfg,
    pub claude: ClaudeCfg,
    pub review: ReviewCfg,
    pub rules: RulesCfg,
    /// Repo clones, logs and the state database.
    pub workdir: String,
    #[serde(skip)]
    pub source: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SlackCfg {
    /// xapp- app-level token with connections:write (Socket Mode).
    pub app_token: String,
    /// Channel id to watch, e.g. C0123ABCDEF.
    pub channel: String,
    /// Slack member id whose tag triggers a review.
    pub reviewer: String,
    /// xoxb- bot token with channels:history. With it, a start catches up on
    /// requests posted while prk was not running. Empty = no catch-up.
    pub bot_token: String,
    /// How far back a start may catch up, in hours. 0 = no catch-up.
    pub catch_up_hours: u64,
}

impl Default for SlackCfg {
    fn default() -> Self {
        SlackCfg {
            app_token: String::new(),
            channel: String::new(),
            reviewer: String::new(),
            bot_token: String::new(),
            catch_up_hours: 24,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GithubCfg {
    /// Token `gh` posts the review comments with. Empty = the machine's gh login.
    pub token: String,
    /// Private key git clones and fetches with. Empty = the machine's ssh setup.
    pub ssh_key: String,
    /// Host in the clone URL; an ~/.ssh/config alias works too.
    pub ssh_host: String,
    /// Links to PRs outside these owners are ignored.
    pub orgs: Vec<String>,
}

impl Default for GithubCfg {
    fn default() -> Self {
        GithubCfg {
            token: String::new(),
            ssh_key: String::new(),
            ssh_host: "github.com".to_string(),
            orgs: vec!["Blitzscale-19".to_string()],
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClaudeCfg {
    /// From `claude setup-token`; uses that person's subscription.
    pub oauth_token: String,
    /// Or a console.anthropic.com API key, billed per token.
    pub api_key: String,
    /// The claude executable.
    pub bin: String,
    /// Empty = Claude Code's default model.
    pub model: String,
}

impl Default for ClaudeCfg {
    fn default() -> Self {
        ClaudeCfg {
            oauth_token: String::new(),
            api_key: String::new(),
            bin: "claude".to_string(),
            model: String::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReviewCfg {
    /// /code-review level: low, medium, high or xhigh.
    pub level: String,
    /// Stop a review once its estimated cost passes this. 0 = no cap.
    pub max_budget_usd: f64,
    pub timeout_minutes: u64,
    /// Review draft PRs too.
    pub drafts: bool,
    /// Keep each repo's clone between reviews (faster, uses disk). Off: every
    /// review clones fresh and deletes the clone when it finishes.
    pub keep_repos: bool,
    /// After new commits on a reviewed PR, check the earlier comments and review
    /// only what changed, instead of a full review again.
    pub follow_up: bool,
    /// Review each PR once only; later tags after new commits are skipped.
    pub once_per_pr: bool,
    /// Tools the review may use. Empty = the built-in read-and-comment list.
    pub allowed_tools: Vec<String>,
}

impl Default for ReviewCfg {
    fn default() -> Self {
        ReviewCfg {
            level: "medium".to_string(),
            max_budget_usd: 2.0,
            timeout_minutes: 30,
            drafts: false,
            keep_repos: false,
            follow_up: true,
            once_per_pr: false,
            allowed_tools: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct RulesCfg {
    /// Applied to every repo.
    pub all: String,
    /// Keyed by GitHub repo name, e.g. [rules.repos.suchna].
    pub repos: HashMap<String, RepoRules>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct RepoRules {
    pub rules: String,
}

/// What `review.allowed_tools` falls back to: read and search the checkout, read
/// git history, and read and comment on the one PR under review through gh. No
/// edits, no web. `{owner}`, `{repo}` and `{number}` are filled in per review, so
/// gh can reach only that PR's view, diff, files, comments and reviews -- not
/// other PRs, other repos, or the merge endpoint.
pub const DEFAULT_ALLOWED_TOOLS: &[&str] = &[
    "Read",
    "Grep",
    "Glob",
    "Agent",
    "Skill",
    "ReportFindings",
    "Bash(gh pr view {number}:*)",
    "Bash(gh pr diff {number}:*)",
    "Bash(gh pr comment {number}:*)",
    "Bash(gh api repos/{owner}/{repo}/pulls/{number}/files:*)",
    "Bash(gh api repos/{owner}/{repo}/pulls/{number}/comments:*)",
    "Bash(gh api repos/{owner}/{repo}/pulls/{number}/reviews:*)",
    "Bash(git diff:*)",
    "Bash(git log:*)",
    "Bash(git show:*)",
    "Bash(git status:*)",
    "Bash(git blame:*)",
    "Bash(git rev-parse:*)",
    "Bash(git merge-base:*)",
    "Bash(git branch:*)",
];

const LEVELS: &[&str] = &["low", "medium", "high", "xhigh"];

/// `$PRK_CONFIG`, else ~/.config/prk/config.toml.
pub fn default_path() -> PathBuf {
    if let Ok(path) = std::env::var("PRK_CONFIG")
        && !path.is_empty()
    {
        return PathBuf::from(path);
    }
    home().join(".config/prk/config.toml")
}

fn home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// `~/x` -> `$HOME/x`; anything else unchanged.
pub fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None if path == "~" => home(),
        None => PathBuf::from(path),
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            format!(
                "{}: cannot be read ({e}). Copy config.example.toml there and fill it in, \
                 or pass --config PATH.",
                path.display()
            )
        })?;
        let mut cfg: Config =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        cfg.source = path.to_path_buf();
        if cfg.workdir.is_empty() {
            cfg.workdir = "~/.local/share/prk".to_string();
        }
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), String> {
        let at = self.source.display();
        if !self.slack.app_token.starts_with("xapp-") {
            return Err(format!(
                "{at}: slack.app_token must be an xapp- app-level token"
            ));
        }
        if self.slack.channel.is_empty() {
            return Err(format!(
                "{at}: slack.channel is empty (a channel id like C0123ABCDEF)"
            ));
        }
        if !(self.slack.reviewer.starts_with('U') || self.slack.reviewer.starts_with('W')) {
            return Err(format!(
                "{at}: slack.reviewer must be a Slack member id (U...), from profile -> ⋮ -> Copy member ID"
            ));
        }
        if !self.slack.bot_token.is_empty() && !self.slack.bot_token.starts_with("xoxb-") {
            return Err(format!(
                "{at}: slack.bot_token must be the app's xoxb- bot token (or empty)"
            ));
        }
        if self.github.orgs.is_empty() {
            return Err(format!(
                "{at}: github.orgs is empty, so no PR link would ever match"
            ));
        }
        if !LEVELS.contains(&self.review.level.as_str()) {
            return Err(format!(
                "{at}: review.level must be one of {}",
                LEVELS.join(", ")
            ));
        }
        if !self.claude.oauth_token.is_empty() && !self.claude.api_key.is_empty() {
            return Err(format!(
                "{at}: set claude.oauth_token or claude.api_key, not both"
            ));
        }
        if self.review.timeout_minutes == 0 {
            return Err(format!("{at}: review.timeout_minutes must be at least 1"));
        }
        Ok(())
    }

    /// The file holds tokens, so refuse to run while other users can read it.
    pub fn check_private(&self) -> Result<(), String> {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&self.source)
            .map_err(|e| format!("{}: {e}", self.source.display()))?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            return Err(format!(
                "{} holds tokens but other users can read it (mode {:o}). Run: chmod 600 {}",
                self.source.display(),
                mode & 0o777,
                self.source.display()
            ));
        }
        Ok(())
    }

    pub fn workdir(&self) -> PathBuf {
        expand(&self.workdir)
    }

    /// The tools for reviewing `owner/repo#number`, placeholders filled in.
    pub fn allowed_tools(&self, owner: &str, repo: &str, number: u64) -> Vec<String> {
        let tools: Vec<String> = if self.review.allowed_tools.is_empty() {
            DEFAULT_ALLOWED_TOOLS
                .iter()
                .map(|t| t.to_string())
                .collect()
        } else {
            self.review.allowed_tools.clone()
        };
        tools
            .into_iter()
            .map(|t| {
                t.replace("{owner}", owner)
                    .replace("{repo}", repo)
                    .replace("{number}", &number.to_string())
            })
            .collect()
    }

    /// The `all` rules followed by this repo's own, or None when neither is set.
    pub fn rules_for(&self, repo: &str) -> Option<String> {
        let own = self
            .rules
            .repos
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(repo))
            .map(|(_, r)| r.rules.trim())
            .filter(|r| !r.is_empty());
        let all = Some(self.rules.all.trim()).filter(|r| !r.is_empty());
        match (all, own) {
            (None, None) => None,
            (Some(a), None) => Some(a.to_string()),
            (None, Some(o)) => Some(o.to_string()),
            (Some(a), Some(o)) => Some(format!("{a}\n{o}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = "[slack]\napp_token = \"xapp-1\"\nchannel = \"C1\"\nreviewer = \"U1\"\n";

    fn parse(text: &str) -> Result<Config, String> {
        let mut cfg: Config = toml::from_str(text).map_err(|e| e.to_string())?;
        cfg.source = PathBuf::from("test.toml");
        cfg.validate()?;
        Ok(cfg)
    }

    #[test]
    fn minimal_config_takes_the_defaults() {
        let cfg = parse(MINIMAL).unwrap();
        assert_eq!(cfg.review.level, "medium");
        assert_eq!(cfg.review.max_budget_usd, 2.0);
        assert_eq!(cfg.github.orgs, vec!["Blitzscale-19"]);
        assert_eq!(cfg.github.ssh_host, "github.com");
        assert_eq!(cfg.claude.bin, "claude");
        assert!(!cfg.review.keep_repos);
        assert!(cfg.review.follow_up);
        assert!(!cfg.review.once_per_pr);
        assert_eq!(cfg.slack.catch_up_hours, 24);
        assert!(cfg.slack.bot_token.is_empty());
        let tools = cfg.allowed_tools("Blitzscale-19", "suchna", 823);
        assert!(tools.contains(&"Bash(gh pr diff 823:*)".to_string()));
        assert!(
            tools.contains(
                &"Bash(gh api repos/Blitzscale-19/suchna/pulls/823/comments:*)".to_string()
            )
        );
        // gh reaches only this PR: no open-ended api access, no merge, no gh pr review
        let gh: Vec<&String> = tools.iter().filter(|t| t.starts_with("Bash(gh")).collect();
        assert!(gh.iter().all(|t| t.contains("823")));
        assert!(
            !tools
                .iter()
                .any(|t| t.contains("merge") && t.starts_with("Bash(gh"))
        );
        assert!(!tools.iter().any(|t| t == "Bash(gh api:*)"));
        assert!(!tools.iter().any(|t| t.starts_with("Bash(gh pr review")));
        assert!(!tools.iter().any(|t| t == "Edit" || t == "Write"));
    }

    #[test]
    fn rules_combine_all_with_the_repos_own() {
        let cfg = parse(&format!(
            "{MINIMAL}[rules]\nall = \"- global\"\n[rules.repos.suchna]\nrules = \"\"\"\n- wrap handlers\n\"\"\"\n"
        ))
        .unwrap();
        assert_eq!(
            cfg.rules_for("suchna").unwrap(),
            "- global\n- wrap handlers"
        );
        assert_eq!(
            cfg.rules_for("Suchna").unwrap(),
            "- global\n- wrap handlers"
        );
        assert_eq!(cfg.rules_for("alfred").unwrap(), "- global");
    }

    #[test]
    fn no_rules_means_none() {
        assert_eq!(parse(MINIMAL).unwrap().rules_for("suchna"), None);
    }

    #[test]
    fn bad_values_are_refused() {
        assert!(
            parse("[slack]\napp_token = \"xoxb-1\"\nchannel = \"C1\"\nreviewer = \"U1\"\n")
                .is_err()
        );
        assert!(
            parse("[slack]\napp_token = \"xapp-1\"\nchannel = \"C1\"\nreviewer = \"anshuman\"\n")
                .is_err()
        );
        assert!(parse(&format!("{MINIMAL}[review]\nlevel = \"max\"\n")).is_err());
        assert!(parse(&MINIMAL.replace("reviewer", "bot_token = \"xapp-2\"\nreviewer")).is_err());
        assert!(
            parse(&format!(
                "{MINIMAL}[claude]\noauth_token = \"a\"\napi_key = \"b\"\n"
            ))
            .is_err()
        );
        // a typo in a key is an error, not a silently ignored setting
        assert!(parse(&format!("{MINIMAL}[review]\nlevle = \"low\"\n")).is_err());
    }
}
