//! Review GitHub PRs with Claude Code when someone tags the reviewer in Slack.
//!
//! Listens to one channel over Socket Mode. A message there that tags
//! `slack.reviewer` and links PRs from `github.orgs` queues those PRs; a worker
//! reviews them one at a time with `/code-review --comment`, so the findings land
//! on the PR as inline comments from the account in `github.token`. Everything --
//! credentials, review settings, per-repo rules -- comes from one config.toml.
//!
//!     prk                      # listen (ctrl-c to stop)
//!     prk --dry-run -v         # log what would be reviewed
//!     prk check                # prove every credential works
//!     prk review <PR url>      # review one PR now

mod config;
mod log;
mod review;
mod slack;
mod socket;
mod state;
mod stop;

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use clap::{Parser, Subcommand};

use crate::config::Config;
use crate::review::{Failure, Outcome, Reviewer};
use crate::slack::PrRef;
use crate::stop::Stop;

/// How long to wait before retrying once the Claude account is out of usage.
const USAGE_LIMIT_PAUSE_SECONDS: f64 = 30.0 * 60.0;

#[derive(Parser)]
#[command(
    name = "prk",
    version,
    about = "Review GitHub PRs with Claude Code when someone tags the reviewer in Slack"
)]
struct Cli {
    /// config.toml to read (default: $PRK_CONFIG, then ~/.config/prk/config.toml)
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    /// log what would be reviewed; run nothing, post nothing
    #[arg(long = "dry-run", global = true)]
    dry_run: bool,

    #[arg(long, short, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Check the config and that Slack, GitHub, git and Claude all work
    Check,
    /// Review one PR now, without Slack
    Review {
        /// e.g. https://github.com/Blitzscale-19/suchna/pull/826
        url: String,
        /// review even if this head commit was reviewed before
        #[arg(long)]
        force: bool,
    },
}

fn die(message: impl AsRef<str>) -> ! {
    eprintln!("{}", message.as_ref());
    std::process::exit(1)
}

fn main() {
    let cli = Cli::parse();
    log::set_verbose(cli.verbose);
    let path = cli.config.clone().unwrap_or_else(config::default_path);
    let cfg = Config::load(&path).unwrap_or_else(|e| die(e));
    if let Err(e) = cfg.check_private() {
        die(e);
    }
    debug!("config: {}", path.display());

    match cli.command {
        Some(Cmd::Check) => std::process::exit(check(&cfg)),
        Some(Cmd::Review { url, force }) => {
            let pr =
                PrRef::parse(&url).unwrap_or_else(|| die(format!("not a GitHub PR url: {url}")));
            let reviewer = Reviewer::new(cfg, cli.dry_run).unwrap_or_else(|e| die(e));
            let stop = Arc::new(Stop::default());
            install_signal_handlers(stop.clone());
            if !review_one(&reviewer, &pr, force, &stop) {
                std::process::exit(1);
            }
        }
        None => listen(cfg, cli.dry_run),
    }
}

/// Review one PR and log the outcome. True unless it failed.
fn review_one(reviewer: &Reviewer, pr: &PrRef, force: bool, stop: &Stop) -> bool {
    match reviewer.review(pr, force, stop) {
        Ok(Outcome::Reviewed { log }) => {
            info!("reviewed {}; output in {}", pr.key(), log.display());
            true
        }
        Ok(Outcome::Skipped(why)) => {
            info!("skipped {}: {why}", pr.key());
            true
        }
        Ok(Outcome::DryRun) => {
            info!("dry run: would review {} now", pr.url());
            true
        }
        Err(Failure::UsageLimit(why)) => {
            error!("{}: Claude usage limit reached: {why}", pr.key());
            false
        }
        Err(Failure::Failed(why)) => {
            error!("{} failed: {why}", pr.key());
            false
        }
    }
}

fn listen(cfg: Config, dry_run: bool) {
    let channel = cfg.slack.channel.clone();
    let reviewer_id = cfg.slack.reviewer.clone();
    let orgs = cfg.github.orgs.clone();
    let app_token = cfg.slack.app_token.clone();
    if dry_run {
        warn!("DRY RUN -- PRs are looked up but not reviewed");
    }
    let reviewer = Reviewer::new(cfg, dry_run).unwrap_or_else(|e| die(e));

    let stop = Arc::new(Stop::default());
    install_signal_handlers(stop.clone());

    // Reviews take minutes, so they run on their own thread and the socket only
    // queues them; one at a time keeps the Claude usage predictable.
    let (queue, jobs) = mpsc::channel::<PrRef>();
    let worker = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            while !stop.is_set() {
                let Ok(pr) = jobs.recv_timeout(Duration::from_secs(1)) else {
                    continue;
                };
                loop {
                    match reviewer.review(&pr, false, &stop) {
                        Err(Failure::UsageLimit(why)) if !stop.is_set() => {
                            warn!(
                                "{}: Claude usage limit reached ({why}); pausing reviews for {} minutes",
                                pr.key(),
                                USAGE_LIMIT_PAUSE_SECONDS / 60.0
                            );
                            stop.wait(USAGE_LIMIT_PAUSE_SECONDS);
                        }
                        other => {
                            match other {
                                Ok(Outcome::Reviewed { log }) => {
                                    info!("reviewed {}; output in {}", pr.key(), log.display())
                                }
                                Ok(Outcome::Skipped(why)) => info!("skipped {}: {why}", pr.key()),
                                Ok(Outcome::DryRun) => info!("dry run: would review {}", pr.url()),
                                Err(Failure::Failed(why)) | Err(Failure::UsageLimit(why)) => {
                                    error!("{} failed: {why}", pr.key())
                                }
                            }
                            break;
                        }
                    }
                }
            }
        })
    };

    info!("watching {channel} for tags of {reviewer_id} (ctrl-c to stop)");
    let on_event = move |event: &serde_json::Value| {
        let Some(request) = slack::request(event, &channel, &reviewer_id, &orgs) else {
            return;
        };
        for pr in request.prs {
            info!(
                "{} asked for a review of {} (message {})",
                request.user,
                pr.key(),
                request.ts
            );
            let _ = queue.send(pr);
        }
    };
    let listener = {
        let stop = stop.clone();
        std::thread::spawn(move || socket::run(&app_token, &on_event, &stop))
    };
    stop.wait_forever();
    info!("stopping");
    let _ = worker.join();
    // The listener may sit in a 45s socket read; the process is exiting anyway.
    drop(listener);
}

/// Prove each credential, printing one line per check. Exit status 1 if any fail.
fn check(cfg: &Config) -> i32 {
    let mut failed = 0;
    let mut report = |name: &str, result: Result<String, String>| match result {
        Ok(detail) => println!("ok    {name:<8} {detail}"),
        Err(why) => {
            failed += 1;
            println!("FAIL  {name:<8} {why}");
        }
    };

    println!("config   {}", cfg.source.display());
    report(
        "slack",
        socket::open_connection(&cfg.slack.app_token).map(|_| {
            format!(
                "app token works; watching {} for <@{}>",
                cfg.slack.channel, cfg.slack.reviewer
            )
        }),
    );

    let reviewer = Reviewer::new(cfg.clone(), true).unwrap_or_else(|e| die(e));
    let mut gh = Command::new("gh");
    gh.args(["api", "user", "--jq", ".login"]);
    report(
        "github",
        output(reviewer.env_for(gh)).map(|login| format!("comments will be posted as {login}")),
    );

    let mut ssh = Command::new("git");
    ssh.args([
        "ls-remote",
        "--heads",
        &format!(
            "git@{}:{}/prk-probe.git",
            cfg.github.ssh_host, cfg.github.orgs[0]
        ),
    ]);
    report(
        "git ssh",
        probe_ssh(reviewer.env_for(ssh), &cfg.github.ssh_host),
    );

    let mut claude = Command::new(&cfg.claude.bin);
    claude.arg("--version");
    let auth = if !cfg.claude.oauth_token.is_empty() {
        "oauth_token (subscription)"
    } else if !cfg.claude.api_key.is_empty() {
        "api_key"
    } else {
        "this machine's claude login"
    };
    report(
        "claude",
        output(reviewer.env_for(claude)).map(|v| format!("{} via {auth}", v.trim())),
    );

    let repos: Vec<&String> = cfg.rules.repos.keys().collect();
    println!(
        "rules    all: {}; repos: {}",
        if cfg.rules.all.trim().is_empty() {
            "none"
        } else {
            "set"
        },
        if repos.is_empty() {
            "none".to_string()
        } else {
            repos
                .iter()
                .map(|r| r.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        }
    );
    if failed > 0 { 1 } else { 0 }
}

fn output(mut cmd: Command) -> Result<String, String> {
    let out = cmd.output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Reaching GitHub over ssh is what matters here: a missing probe repo answers
/// "Repository not found" only after authentication succeeded.
fn probe_ssh(mut cmd: Command, host: &str) -> Result<String, String> {
    let out = cmd.output().map_err(|e| e.to_string())?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    if out.status.success() || stderr.contains("Repository not found") {
        Ok(format!("ssh to {host} works"))
    } else {
        Err(stderr.trim().to_string())
    }
}

fn install_signal_handlers(stop: Arc<Stop>) {
    use signal_hook::consts::{SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;
    match Signals::new([SIGINT, SIGTERM]) {
        Ok(mut signals) => {
            std::thread::spawn(move || {
                for _ in signals.forever() {
                    stop.set();
                }
            });
        }
        Err(e) => warn!("could not install signal handlers ({e})"),
    }
}
