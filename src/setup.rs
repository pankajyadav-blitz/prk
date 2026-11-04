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
            "{}: cannot be read ({e}). Run `prk init` to create it.",
            config.display()
        )
    })?;
    let token = match token {
        Some(token) => check_token(&token)?,
        None => create_token(claude_bin)?,
    };
    write_private(config, &set_oauth_token(&text, &token))?;
    println!(
        "Saved claude.oauth_token in {}. Run `prk check` to confirm.",
        config.display()
    );
    Ok(())
}

/// Run `claude setup-token` and read back the token it printed.
pub fn create_token(claude_bin: &str) -> Result<String, String> {
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
    let token =
        rpassword::prompt_password("Paste the token it printed (sk-ant-oat01-..., hidden): ")
            .map_err(|e| format!("could not read the token: {e}"))?;
    check_token(&token)
}

fn check_token(token: &str) -> Result<String, String> {
    let token = token.trim();
    if !token.starts_with("sk-ant-oat") {
        return Err(
            "that is not a Claude subscription token (it starts with sk-ant-oat01-)".to_string(),
        );
    }
    Ok(token.to_string())
}

/// Write the config readable by its owner only, creating its folder if needed.
/// (On Windows the user's profile folder is already private to them.)
pub fn write_private(config: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = config.parent()
        && !dir.as_os_str().is_empty()
        && !dir.exists()
    {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
    }
    std::fs::write(config, text).map_err(|e| format!("{}: {e}", config.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(config, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("{}: {e}", config.display()))?;
    }
    Ok(())
}

/// The config text with `oauth_token` set in `[claude]` and `api_key` emptied
/// (the two are exclusive).
fn set_oauth_token(text: &str, token: &str) -> String {
    let text = set_key(text, "claude", "oauth_token", token);
    if has_key(&text, "claude", "api_key") {
        set_key(&text, "claude", "api_key", "")
    } else {
        text
    }
}

/// Whether `[table]` has a `key = ...` line.
pub fn has_key(text: &str, table: &str, key: &str) -> bool {
    let mut current = String::new();
    lines(text).any(|(line, in_string)| {
        if in_string {
            return false;
        }
        if let Some(name) = header(line) {
            current = name.to_string();
            return false;
        }
        current == table && key_of(line.trim_start()) == Some(key)
    })
}

/// The config text with `key` in `[table]` set to the string `value`. The line
/// is replaced where it is, added to the end of the table, or added in a new
/// table at the end. Every other line, comments included, is kept.
pub fn set_key(text: &str, table: &str, key: &str, value: &str) -> String {
    let line = format!("{key} = {}", toml::Value::String(value.to_string()));
    let mut out: Vec<String> = Vec::new();
    let mut in_table = false;
    let mut found_table = false;
    let mut written = false;

    for (raw, in_string) in lines(text) {
        if !in_string {
            if let Some(name) = header(raw) {
                // Leaving the table without having seen the key: add it at the end.
                if in_table && !written {
                    insert_before_blank_tail(&mut out, line.clone());
                    written = true;
                }
                in_table = name == table;
                found_table |= in_table;
            } else if in_table && !written && key_of(raw.trim_start()) == Some(key) {
                out.push(line.clone());
                written = true;
                continue;
            }
        }
        out.push(raw.to_string());
    }
    if in_table && !written {
        insert_before_blank_tail(&mut out, line.clone());
        written = true;
    }
    if !found_table && !written {
        if out.last().is_some_and(|l| !l.trim().is_empty()) {
            out.push(String::new());
        }
        out.push(format!("[{table}]"));
        out.push(line);
    }
    let mut result = out.join("\n");
    result.push('\n');
    result
}

/// Each line, and whether it sits inside a multi-line string (where a `[` or a
/// `key =` is text, not TOML).
fn lines(text: &str) -> impl Iterator<Item = (&str, bool)> {
    let mut open = false;
    text.lines().map(move |line| {
        let starts_inside = open;
        if line.matches("\"\"\"").count() % 2 == 1 || line.matches("'''").count() % 2 == 1 {
            open = !open;
        }
        // The opening line itself is still TOML (`rules = """`).
        (line, starts_inside)
    })
}

/// The table name of a `[table]` line.
fn header(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix('[')?;
    if rest.starts_with('[') {
        return None;
    }
    rest.split_once(']').map(|(name, _)| name.trim())
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
    fn set_key_edits_any_table_and_skips_multiline_strings() {
        let text = "[slack]\napp_token = \"old\"\n\n[rules]\nall = \"\"\"\n[slack]\nbot_token = x\n\"\"\"\n";
        let out = set_key(text, "slack", "bot_token", "xoxb-1");
        assert!(out.starts_with("[slack]\napp_token = \"old\"\nbot_token = \"xoxb-1\"\n\n[rules]"));
        assert!(out.contains("[slack]\nbot_token = x\n\"\"\"")); // the rule text is untouched
        assert!(has_key(&out, "slack", "bot_token"));
        assert!(!has_key(text, "slack", "bot_token"));
        assert!(!has_key(text, "rules.repos.x", "rules"));
        let out = set_key("", "github", "token", "");
        assert_eq!(out, "[github]\ntoken = \"\"\n");
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
