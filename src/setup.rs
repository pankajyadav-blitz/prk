//! `prk setup-token`: create a Claude subscription token with `claude setup-token`
//! and save it as `claude.oauth_token` in config.toml, without hand-editing.

use std::io::Write;
use std::path::Path;
use std::process::Command;

/// Run `claude setup-token`, ask for the token it printed, and write it into the
/// config. `token` skips both steps and saves the given token.
pub fn run(config: &Path, claude_bin: &str, token: Option<String>) -> Result<(), String> {
    let text = std::fs::read_to_string(config).map_err(|e| {
        format!(
            "{}: cannot be read ({e}). Create the config first (README, step 5), then run this again.",
            config.display()
        )
    })?;

    let token = match token {
        Some(token) => token,
        None => {
            println!(
                "Running `{claude_bin} setup-token`: log in with your Claude subscription in the browser.\n"
            );
            let status = Command::new(claude_bin)
                .arg("setup-token")
                .status()
                .map_err(|e| format!("{claude_bin}: {e}"))?;
            if !status.success() {
                return Err(format!("`{claude_bin} setup-token` failed ({status})"));
            }
            println!();
            let _ = std::io::stdout().flush();
            rpassword::prompt_password("Paste the token it printed (sk-ant-oat01-..., hidden): ")
                .map_err(|e| format!("could not read the token: {e}"))?
        }
    };
    let token = token.trim();
    if !token.starts_with("sk-ant-oat") {
        return Err(
            "that is not a Claude subscription token (it starts with sk-ant-oat01-)".to_string(),
        );
    }

    let updated = set_oauth_token(&text, token);
    std::fs::write(config, updated).map_err(|e| format!("{}: {e}", config.display()))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(config, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("{}: {e}", config.display()))?;
    println!(
        "Saved claude.oauth_token in {} (mode 600). Run `prk check` to confirm.",
        config.display()
    );
    Ok(())
}

/// The config text with `oauth_token` set in `[claude]` and `api_key` emptied
/// (the two are exclusive). Every other line, comments included, is kept.
fn set_oauth_token(text: &str, token: &str) -> String {
    let line = format!("oauth_token = \"{token}\"");
    let mut out: Vec<String> = Vec::new();
    let mut in_claude = false;
    let mut found_table = false;
    let mut written = false;

    for raw in text.lines() {
        let trimmed = raw.trim_start();
        if trimmed.starts_with('[') {
            // Leaving [claude] without having seen oauth_token: add it at the end.
            if in_claude && !written {
                insert_before_blank_tail(&mut out, line.clone());
                written = true;
            }
            in_claude = trimmed.starts_with("[claude]");
            found_table |= in_claude;
        } else if in_claude {
            if key_of(trimmed) == Some("oauth_token") {
                out.push(line.clone());
                written = true;
                continue;
            }
            if key_of(trimmed) == Some("api_key") {
                out.push("api_key = \"\"".to_string());
                continue;
            }
        }
        out.push(raw.to_string());
    }
    if in_claude && !written {
        insert_before_blank_tail(&mut out, line.clone());
        written = true;
    }
    if !found_table && !written {
        if out.last().is_some_and(|l| !l.trim().is_empty()) {
            out.push(String::new());
        }
        out.push("[claude]".to_string());
        out.push(line);
    }
    let mut result = out.join("\n");
    result.push('\n');
    result
}

/// The key of a `key = value` line, ignoring comments.
fn key_of(line: &str) -> Option<&str> {
    if line.starts_with('#') {
        return None;
    }
    line.split_once('=').map(|(k, _)| k.trim())
}

/// Add `line` above any blank lines that end the section, so it stays with it.
fn insert_before_blank_tail(out: &mut Vec<String>, line: String) {
    let at = out
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map_or(0, |i| i + 1);
    out.insert(at, line);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_the_existing_token_and_clears_api_key() {
        let text = "[slack]\nchannel = \"C1\"\n\n[claude]\n# comment\noauth_token = \"\"\napi_key = \"sk-ant-api03-x\"\nmodel = \"\"\n\n[review]\nlevel = \"medium\"\n";
        let out = set_oauth_token(text, "sk-ant-oat01-new");
        assert!(out.contains("[claude]\n# comment\noauth_token = \"sk-ant-oat01-new\"\napi_key = \"\"\nmodel = \"\"\n\n[review]"));
        assert!(out.contains("channel = \"C1\""));
    }

    #[test]
    fn adds_the_key_to_a_claude_table_without_one() {
        let out = set_oauth_token(
            "[claude]\nmodel = \"\"\n\n[review]\nlevel = \"low\"\n",
            "sk-ant-oat01-t",
        );
        assert_eq!(
            out,
            "[claude]\nmodel = \"\"\noauth_token = \"sk-ant-oat01-t\"\n\n[review]\nlevel = \"low\"\n"
        );
        let out = set_oauth_token("[slack]\nchannel = \"C1\"\n[claude]\n", "sk-ant-oat01-t");
        assert!(out.ends_with("[claude]\noauth_token = \"sk-ant-oat01-t\"\n"));
    }

    #[test]
    fn adds_a_claude_table_when_there_is_none() {
        let out = set_oauth_token("[slack]\nchannel = \"C1\"\n", "sk-ant-oat01-t");
        assert_eq!(
            out,
            "[slack]\nchannel = \"C1\"\n\n[claude]\noauth_token = \"sk-ant-oat01-t\"\n"
        );
    }

    #[test]
    fn leaves_other_tables_alone() {
        let text = "[rules.repos.suchna]\nrules = \"\"\"\n- oauth_token = nope\n\"\"\"\n[claude]\noauth_token = \"old\"\n";
        let out = set_oauth_token(text, "sk-ant-oat01-t");
        assert!(out.contains("- oauth_token = nope"));
        assert!(out.contains("oauth_token = \"sk-ant-oat01-t\""));
        assert!(!out.contains("\"old\""));
    }
}
