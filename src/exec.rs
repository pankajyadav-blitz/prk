//! Running gh, git and claude with the config's credentials. The tokens are put
//! into each child's environment only, never into arguments or the logs.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::config::Config;
use crate::debug;

pub struct Exec {
    pub cfg: Config,
}

impl Exec {
    /// Credentials from the config, handed only to the processes that need them.
    pub fn with_env(&self, cmd: &mut Command) {
        let gh = &self.cfg.github;
        if !gh.token.is_empty() {
            cmd.env("GH_TOKEN", &gh.token);
        }
        let mut ssh = "ssh -o BatchMode=yes -o StrictHostKeyChecking=accept-new".to_string();
        if !gh.ssh_key.is_empty() {
            // git runs this through sh, Git for Windows too, which wants / in paths.
            let key = crate::config::expand(&gh.ssh_key)
                .to_string_lossy()
                .replace('\\', "/");
            ssh.push_str(&format!(" -o IdentitiesOnly=yes -i '{key}'"));
        }
        cmd.env("GIT_SSH_COMMAND", ssh)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GH_PROMPT_DISABLED", "1");

        // Claude Code prefers an API key over an OAuth token, so drop whichever
        // one the config did not choose in case the shell exported it.
        let claude = &self.cfg.claude;
        if !claude.oauth_token.is_empty() {
            cmd.env("CLAUDE_CODE_OAUTH_TOKEN", &claude.oauth_token)
                .env_remove("ANTHROPIC_API_KEY");
        } else if !claude.api_key.is_empty() {
            cmd.env("ANTHROPIC_API_KEY", &claude.api_key)
                .env_remove("CLAUDE_CODE_OAUTH_TOKEN");
        }
    }

    /// `cmd` with the credentials set.
    pub fn env_for(&self, mut cmd: Command) -> Command {
        self.with_env(&mut cmd);
        cmd
    }

    /// Run to completion; stdout on success, stderr in the error.
    pub fn run(&self, program: &str, args: &[&str], cwd: Option<&Path>) -> Result<String, String> {
        let mut cmd = Command::new(program);
        cmd.args(args).stdin(Stdio::null());
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        self.with_env(&mut cmd);
        debug!("$ {program} {}", args.join(" "));
        let out = cmd.output().map_err(|e| format!("{program}: {e}"))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(format!(
                "{program} {}: {}",
                args.first().unwrap_or(&""),
                stderr.trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Like `run`, but only says whether it succeeded.
    pub fn ok(&self, program: &str, args: &[&str], cwd: Option<&Path>) -> bool {
        self.run(program, args, cwd).is_ok()
    }
}
