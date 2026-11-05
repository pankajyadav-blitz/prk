//! prk (parikshak): review GitHub PRs with Claude Code when someone tags the
//! reviewer in Slack.
//!
//! Listens to one channel over Socket Mode; on start it also catches up on the
//! requests posted while it was not running. A message that tags
//! `slack.reviewer` and links PRs from `github.orgs` queues those PRs in the
//! local database, and a worker reviews them one at a time. Findings land on
//! the PR as inline comments from the account in `github.token`. Everything --
//! credentials, review settings, per-repo rules -- comes from one config.toml.
//!
//!     prk                      # listen (live dashboard in a terminal)
//!     prk -H                   # history of reviews
//!     prk -c <PR url>          # the comments a review posted
//!     prk review <PR url>      # review one PR now
//!     prk init                 # set prk up on this machine
//!     prk check                # prove every credential works

mod activity;
mod catchup;
mod config;
mod db;
mod exec;
mod github;
mod init;
mod log;
mod review;
mod setup;
mod slack;
mod socket;
mod stop;
mod tui;
mod update;
mod view;

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use clap::{Args, Parser, Subcommand};
use crossterm::style::Color;

use crate::config::Config;
use crate::db::{Db, Enqueued, HistoryFilter};
use crate::exec::Exec;
use crate::review::{End, Reviewer};
use crate::slack::PrRef;
use crate::stop::Stop;
use crate::view::{bold, dim, paint};

#[derive(Parser)]
#[command(
    name = "prk",
    version,
    about = "Review GitHub PRs with Claude Code when someone tags you in Slack",
    after_help = "Examples:\n  \
        prk                  listen for review requests (live dashboard in a terminal)\n  \
        prk -H               recent reviews        prk -H <PR url>   every attempt for one PR\n  \
        prk -H --today       today's reviews       prk -H --status failed\n  \
        prk -c <PR url>      comments the latest review of that PR posted\n  \
        prk --stats          totals: reviewed, skipped, failed, follow-up verdicts\n  \
        prk review <PR url>  review one PR now (--force to review an already reviewed commit)\n  \
        prk init             set prk up on this machine (asks a few questions)\n  \
        prk check            check Slack, GitHub, ssh and Claude\n  \
        prk setup-token      create a Claude subscription token and save it in the config\n  \
        prk update           update prk to the latest release"
)]
struct Cli {
    /// config.toml to read (default: $PRK_CONFIG, then ~/.config/prk/config.toml)
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    /// look PRs up and log what would happen; review and post nothing
    #[arg(short = 'd', long = "dry-run", global = true)]
    dry_run: bool,

    #[arg(short, long, global = true)]
    verbose: bool,

    /// show review history, or every attempt for one PR
    #[arg(short = 'H', long, num_args = 0..=1, value_name = "PR_URL", conflicts_with_all = ["comments", "stats"])]
    history: Option<Option<String>>,

    /// show the comments a review posted (the latest review, or the latest of one PR)
    #[arg(short = 'c', long, num_args = 0..=1, value_name = "PR_URL", conflicts_with = "stats")]
    comments: Option<Option<String>>,

    /// totals from the history
    #[arg(long)]
    stats: bool,

    #[command(flatten)]
    filter: FilterArgs,

    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Args)]
struct FilterArgs {
    /// with -H: how many to show
    #[arg(short = 'n', long, default_value_t = 20)]
    limit: usize,
    /// with -H: only today
    #[arg(long)]
    today: bool,
    /// with -H: only this recent, e.g. 12h, 7d, 2w
    #[arg(long, value_name = "AGE")]
    since: Option<String>,
    /// with -H: only this repo
    #[arg(short = 'r', long)]
    repo: Option<String>,
    /// with -H: only this status (waiting, running, reviewed, skipped, failed)
    #[arg(short = 's', long)]
    status: Option<String>,
    /// with -c: every review of the PR, not only the latest; with -H: no limit
    #[arg(short = 'a', long)]
    all: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Set prk up on this machine: Slack app, GitHub, Claude, config file
    Init,
    /// Check the config and that Slack, GitHub, git and Claude all work
    Check,
    /// Review one PR now, without Slack
    Review {
        /// e.g. https://github.com/Blitzscale-19/suchna/pull/826
        url: String,
        /// a full review even if this commit was reviewed before (no follow-up)
        #[arg(short, long)]
        force: bool,
    },
    /// Create a Claude subscription token (`claude setup-token`) and save it in the config
    SetupToken {
        /// save this token instead of running `claude setup-token`
        #[arg(long, value_name = "TOKEN")]
        token: Option<String>,
    },
    /// Update prk to the latest GitHub release
    Update,
}

fn die(message: impl AsRef<str>) -> ! {
    eprintln!("{}", message.as_ref());
    std::process::exit(1)
}

fn main() {
    // Before parsing: --version and --help exit inside it.
    update::remove_old_binary();
    let cli = Cli::parse();
    log::set_verbose(cli.verbose);
    let path = cli.config.clone().unwrap_or_else(config::default_path);
    // Before loading: the config may still have placeholders the loader refuses.
    if let Some(Cmd::SetupToken { token }) = &cli.command {
        if let Err(e) = setup::run(&path, &config::program("claude"), token.clone()) {
            die(e);
        }
        return;
    }
    if let Some(Cmd::Init) = &cli.command {
        if let Err(e) = init::run(&path) {
            die(e);
        }
        return;
    }
    if let Some(Cmd::Update) = &cli.command {
        if let Err(e) = update::self_update() {
            die(format!("update failed: {e}"));
        }
        return;
    }
    let cfg = Config::load(&path).unwrap_or_else(|e| die(e));
    if let Err(e) = cfg.check_private() {
        die(e);
    }
    debug!("config: {}", path.display());
    let db = Arc::new(Db::open(&cfg.workdir().join("state.sqlite3")).unwrap_or_else(|e| die(e)));

    if let Some(pr) = &cli.history {
        return show_history(&db, pr.as_deref(), &cli.filter);
    }
    if let Some(pr) = &cli.comments {
        return show_comments(&db, pr.as_deref(), cli.filter.all);
    }
    if cli.stats {
        return view::stats(&db);
    }
    match cli.command {
        Some(Cmd::Check) => std::process::exit(check(&cfg)),
        Some(Cmd::Review { url, force }) => review_now(cfg, db, &url, force, cli.dry_run),
        Some(Cmd::SetupToken { .. }) | Some(Cmd::Update) | Some(Cmd::Init) => {
            unreachable!("handled before the config is loaded")
        }
        None => listen(cfg, db, cli.dry_run),
    }
}

/// gh, git and claude must be installed before anything is queued or run: a
/// missing one would only show up mid-review, as a failure no retry can fix.
/// gh needs no login; the token in the config is enough.
fn require_tools(cfg: &Config) {
    let tools = [
        (
            "gh",
            "gh",
            "https://cli.github.com (no `gh auth login` needed: github.token is used)",
        ),
        ("git", "git", "your package manager"),
        (
            "claude",
            cfg.claude.bin.as_str(),
            "npm install -g @anthropic-ai/claude-code",
        ),
    ];
    let missing: Vec<String> = tools
        .iter()
        .filter(|(_, bin, _)| {
            Command::new(bin)
                .arg("--version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_err()
        })
        .map(|(name, bin, how)| format!("  {name} ({bin}): install from {how}"))
        .collect();
    if !missing.is_empty() {
        die(format!("prk needs these on PATH:\n{}", missing.join("\n")));
    }
}

fn parse_pr(url: &str) -> PrRef {
    PrRef::parse(url).unwrap_or_else(|| die(format!("not a GitHub PR url: {url}")))
}

fn history_filter(f: &FilterArgs) -> HistoryFilter {
    let since = if f.today {
        Some(
            chrono::Local::now()
                .date_naive()
                .and_hms_opt(0, 0, 0)
                .and_then(|t| t.and_local_timezone(chrono::Local).earliest())
                .map(|t| t.timestamp())
                .unwrap_or(0),
        )
    } else {
        f.since.as_deref().map(|s| {
            db::now()
                - view::parse_since(s).unwrap_or_else(|| {
                    die(format!(
                        "--since {s}: use a number and m, h, d or w, e.g. 7d"
                    ))
                })
        })
    };
    HistoryFilter {
        limit: if f.all { 0 } else { f.limit },
        since,
        repo: f.repo.clone(),
        status: f.status.clone(),
        pr: None,
    }
}

fn show_history(db: &Arc<Db>, pr: Option<&str>, f: &FilterArgs) {
    let mut filter = history_filter(f);
    if let Some(url) = pr {
        let pr = parse_pr(url);
        filter.pr = Some((pr.owner, pr.repo, pr.number));
        filter.limit = 0;
        return view::history_of_pr(db, &db.history(&filter));
    }
    if std::io::stdout().is_terminal() {
        filter.limit = 0;
        if let Err(e) = tui::history(db.clone(), filter) {
            die(format!("history screen: {e}"));
        }
        return;
    }
    view::history(&db.history(&filter));
}

fn show_comments(db: &Db, pr: Option<&str>, all: bool) {
    let mut filter = HistoryFilter {
        status: Some(db::REVIEWED.into()),
        limit: if all { 0 } else { 1 },
        ..Default::default()
    };
    if let Some(url) = pr {
        let pr = parse_pr(url);
        filter.pr = Some((pr.owner, pr.repo, pr.number));
    }
    let mut jobs = db.history(&filter);
    if jobs.is_empty() {
        println!(
            "No finished reviews{} yet.",
            if pr.is_some() { " of that PR" } else { "" }
        );
        return;
    }
    jobs.reverse();
    for (i, job) in jobs.iter().enumerate() {
        if i > 0 {
            println!("\n{}\n", dim(&"─".repeat(40)));
        }
        view::comments(job, &db.comments(job.id));
    }
}

/// `prk review <url>`: queue the PR and review it here, showing each step.
fn review_now(cfg: Config, db: Arc<Db>, url: &str, force: bool, dry_run: bool) {
    let pr = parse_pr(url);
    require_tools(&cfg);
    let id = match db
        .enqueue(&pr.owner, &pr.repo, pr.number, "", "", "", "manual", force)
        .unwrap_or_else(|e| die(e))
    {
        Enqueued::New(id) | Enqueued::Seen(id) => id,
        Enqueued::Pending(id, status) if status == db::WAITING => id,
        Enqueued::Pending(_, _) => die(format!(
            "{} is being reviewed right now (see `prk -H {url}`)",
            pr.key()
        )),
    };
    if !db.claim(id).unwrap_or_else(|e| die(e)) {
        die(format!(
            "{} was just taken by the running listener",
            pr.key()
        ));
    }
    let job = db
        .job(id)
        .ok()
        .flatten()
        .unwrap_or_else(|| die("job vanished"));
    let stop = Arc::new(Stop::default());
    install_signal_handlers(stop.clone());
    let reviewer = Reviewer::new(cfg, db.clone(), dry_run);

    println!("{} {}", bold(&pr.key()), dim(&pr.url()));
    // Log lines would break into the step lines; the outcome is printed below.
    // With -v they stay, for debugging.
    if !log::verbose() {
        activity::get().set_tui(true);
    }
    let worker = {
        let stop = stop.clone();
        let job = job.clone();
        std::thread::spawn(move || reviewer.process(&job, &stop))
    };
    follow_steps(&worker);
    let end = worker
        .join()
        .unwrap_or(End::Failed("review thread crashed".into()));
    activity::get().set_tui(false);
    let job = db.job(id).ok().flatten().unwrap_or(job);
    let (glyph, color) = view::status_look(&job.status);
    let summary = match &end {
        End::Reviewed(why) => format!("{} {}", view::status_word(&job), why),
        End::Skipped(why) => format!("skipped: {why}"),
        End::Retry(why) => format!(
            "failed, retried {} by a running prk: {why}",
            view::when(job.not_before)
        ),
        End::Failed(why) => format!("failed: {why}"),
        End::UsageLimit(why) => format!("Claude usage limit: {why}"),
        End::Stopped => "cancelled".to_string(),
    };
    println!(
        "{} {}  {}",
        paint(glyph, color),
        paint(&summary, color),
        dim(&view::took(job.took()))
    );
    if matches!(end, End::Reviewed(_)) {
        println!();
        view::comments(&job, &db.comments(job.id));
    }
    if !job.log.is_empty() {
        println!("{}", dim(&format!("log: {}", job.log)));
    }
    if matches!(end, End::Retry(_) | End::Failed(_) | End::UsageLimit(_)) {
        std::process::exit(1);
    }
}

/// Print each step of the review as it changes; in a terminal the current step
/// carries a spinner and its elapsed time.
fn follow_steps(worker: &std::thread::JoinHandle<End>) {
    const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let tty = std::io::stdout().is_terminal();
    let mut current: Option<(String, i64)> = None;
    let mut tick = 0usize;
    let finish = |step: &str, since: i64| {
        let line = format!(
            "{} {:<20} {}",
            paint("✓", Color::Green),
            step,
            dim(&view::took(db::now() - since))
        );
        if tty {
            print!("\r\x1b[2K{line}\n");
        } else {
            println!("{line}");
        }
    };
    loop {
        let step = activity::get().step().map(|(_, s, since)| (s, since));
        if step.as_ref().map(|s| &s.0) != current.as_ref().map(|s| &s.0) {
            if let Some((s, since)) = &current {
                finish(s, *since);
            }
            current = step;
        }
        if worker.is_finished() {
            if let Some((s, since)) = &current {
                finish(s, *since);
            }
            break;
        }
        if tty && let Some((s, since)) = &current {
            print!(
                "\r\x1b[2K{} {:<20} {}",
                paint(SPIN[tick % SPIN.len()], Color::Cyan),
                s,
                dim(&view::took(db::now() - since))
            );
            let _ = std::io::stdout().flush();
        }
        tick += 1;
        std::thread::sleep(std::time::Duration::from_millis(120));
    }
}

/// The listener: Slack events and catch-up queue jobs, a worker reviews them.
fn listen(cfg: Config, db: Arc<Db>, dry_run: bool) {
    require_tools(&cfg);
    let tty = std::io::stdout().is_terminal();
    let channel = cfg.slack.channel.clone();
    let reviewer_id = cfg.slack.reviewer.clone();
    let orgs = cfg.github.orgs.clone();
    let app_token = cfg.slack.app_token.clone();
    let bot_token = cfg.slack.bot_token.clone();
    let catch_up = cfg.slack.catch_up;
    let timeout_minutes = cfg.review.timeout_minutes;
    if tty {
        activity::get().set_tui(true);
    }
    if dry_run {
        warn!("DRY RUN -- PRs are looked up but not reviewed");
    }

    let recovered = db.recover();
    if recovered > 0 {
        info!("{recovered} interrupted review(s) queued again");
    }
    let reviewer = Reviewer::new(cfg.clone(), db.clone(), dry_run);
    if db.running().is_empty() {
        reviewer.clean_repos();
    }
    let stop = Arc::new(Stop::default());
    install_signal_handlers(stop.clone());

    let queue = {
        let db = db.clone();
        let channel = channel.clone();
        let reviewer_id = reviewer_id.clone();
        let bot_token = bot_token.clone();
        move |event: &serde_json::Value, source: &str| -> bool {
            let Some(request) = slack::request(event, &channel, &reviewer_id, &orgs) else {
                return false;
            };
            let mut queued = false;
            for pr in &request.prs {
                match db.enqueue(
                    &pr.owner,
                    &pr.repo,
                    pr.number,
                    &request.user,
                    &request.ts,
                    &request.thread,
                    source,
                    false,
                ) {
                    Ok(Enqueued::New(id)) => {
                        let who = slack::user_name(&bot_token, &request.user);
                        if let Some(name) = &who {
                            db.set_requested_name(id, name);
                        }
                        info!(
                            "queued {} (asked by {}, {source})",
                            pr.key(),
                            who.as_deref().unwrap_or(&request.user)
                        );
                        queued = true;
                    }
                    Ok(Enqueued::Pending(_, status)) => info!("{} is already {status}", pr.key()),
                    Ok(Enqueued::Seen(_)) => debug!(
                        "{} from message {} already queued once",
                        pr.key(),
                        request.ts
                    ),
                    Err(e) => error!("could not queue {}: {e}", pr.key()),
                }
            }
            queued
        }
    };
    let queue = Arc::new(queue);

    let listener = {
        let stop = stop.clone();
        let queue = queue.clone();
        std::thread::spawn(move || {
            socket::run(
                &app_token,
                &|event| {
                    queue(event, "slack");
                },
                &stop,
            )
        })
    };

    if bot_token.is_empty() || !catch_up {
        if bot_token.is_empty() {
            info!("no slack.bot_token, so no catch-up on requests posted while prk was off");
        }
    } else {
        let queue = queue.clone();
        let channel = channel.clone();
        let oldest = catchup::today_start();
        std::thread::spawn(
            move || match catchup::messages(&bot_token, &channel, oldest) {
                Ok(messages) => {
                    let n = messages.iter().filter(|m| queue(m, "catch-up")).count();
                    info!(
                        "caught up on today's {} message(s): {n} new request(s)",
                        messages.len()
                    );
                    activity::get().set_caught_up(Ok(n));
                }
                Err(e) => {
                    warn!("catch-up failed: {e}");
                    activity::get().set_caught_up(Err(e));
                }
            },
        );
    }

    let worker = {
        let stop = stop.clone();
        let db = db.clone();
        std::thread::spawn(move || {
            while !stop.is_set() {
                let a = activity::get();
                if a.paused() || a.held().is_some() {
                    stop.wait(1.0);
                    continue;
                }
                let job = match db.claim_next() {
                    Ok(Some(job)) => job,
                    Ok(None) => {
                        stop.wait(1.0);
                        continue;
                    }
                    Err(e) => {
                        error!("queue: {e}");
                        stop.wait(5.0);
                        continue;
                    }
                };
                let end = reviewer.process(&job, &stop);
                // The kind is decided while the job runs, so read it back.
                let kind = db
                    .job(job.id)
                    .ok()
                    .flatten()
                    .map(|j| j.kind)
                    .unwrap_or_default();
                match end {
                    End::Reviewed(why) => info!(
                        "{} {}: {why}",
                        if kind == "follow-up" {
                            "followed up"
                        } else {
                            "reviewed"
                        },
                        job.key()
                    ),
                    End::Skipped(why) => info!("skipped {}: {why}", job.key()),
                    End::Retry(why) => warn!("{} failed, will retry: {why}", job.key()),
                    End::Failed(why) => error!("{} failed: {why}", job.key()),
                    End::UsageLimit(why) => {
                        warn!("Claude usage limit reached ({why}); pausing reviews for 30 minutes");
                        a.pause_until(
                            db::now() + review::USAGE_LIMIT_SECONDS,
                            "Claude usage limit",
                        );
                    }
                    End::Stopped => {}
                }
            }
        })
    };

    info!("watching {channel} for tags of {reviewer_id}");
    if tty {
        if let Err(e) = tui::dashboard(db, &channel, &reviewer_id, timeout_minutes, stop.clone()) {
            activity::get().set_tui(false);
            error!("dashboard: {e}");
        }
        stop.set();
        activity::get().set_tui(false);
        eprintln!("stopping; an interrupted review is queued again for the next start");
    } else {
        stop.wait_forever();
        info!("stopping");
    }
    let _ = worker.join();
    // The listener may sit in a 45s socket read; the process is exiting anyway.
    drop(listener);
}

/// Prove each credential, printing one line per check. Exit status 1 if any fail.
fn check(cfg: &Config) -> i32 {
    let mut failed = 0;
    let mut report = |name: &str, result: Result<String, String>| match result {
        Ok(detail) => println!("{} {:<9} {detail}", paint("✓", Color::Green), name),
        Err(why) => {
            failed += 1;
            println!(
                "{} {:<9} {}",
                paint("✗", Color::Red),
                name,
                paint(&why, Color::Red)
            );
        }
    };

    println!("{}", dim(&format!("config {}", cfg.source.display())));
    report(
        "slack",
        socket::open_connection(&cfg.slack.app_token).map(|_| {
            format!(
                "app token works; watching {} for <@{}>",
                cfg.slack.channel, cfg.slack.reviewer
            )
        }),
    );
    report(
        "catch-up",
        if cfg.slack.bot_token.is_empty() {
            Ok("off (no slack.bot_token): requests posted while prk is off are missed".into())
        } else if !cfg.slack.catch_up {
            Ok("off (slack.catch_up = false)".into())
        } else {
            catchup::messages(
                &cfg.slack.bot_token,
                &cfg.slack.channel,
                catchup::today_start(),
            )
            .map(|today| {
                format!(
                    "reads the channel; {} message(s) today, caught up on each start",
                    today.len()
                )
            })
        },
    );

    let exec = Exec { cfg: cfg.clone() };
    report(
        "github",
        github::login(&exec).map(|login| format!("comments will be posted as {login}")),
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
        probe_ssh(exec.env_for(ssh), &cfg.github.ssh_host),
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
        output(exec.env_for(claude)).map(|v| format!("{} via {auth}", v.trim())),
    );

    let mut repos: Vec<&String> = cfg.rules.repos.keys().collect();
    repos.sort();
    println!(
        "{} {:<9} all: {}; repos: {}",
        dim("·"),
        "rules",
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

/// Ctrl-C, Ctrl-Break and closing the console window all stop prk cleanly.
#[cfg(windows)]
fn install_signal_handlers(stop: Arc<Stop>) {
    if let Err(e) = ctrlc::set_handler(move || stop.set()) {
        warn!("could not install the ctrl-c handler ({e})");
    }
}

#[cfg(unix)]
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
