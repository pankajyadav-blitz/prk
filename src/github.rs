//! What prk itself asks GitHub, through gh with the config's token: the PR's
//! state, who the token belongs to, whether that account already reviewed a
//! commit, and the comments it posted. Claude never runs these; its gh access
//! is limited to the one PR it reviews.

use std::sync::OnceLock;

use serde_json::Value;

use crate::db::Comment;
use crate::exec::Exec;
use crate::slack::PrRef;

pub struct PrInfo {
    pub state: String,
    pub draft: bool,
    pub head: String,
    pub base: String,
    pub title: String,
    /// The PR description, as the author wrote it.
    pub body: String,
    pub author: String,
}

pub fn pr_info(exec: &Exec, pr: &PrRef) -> Result<PrInfo, String> {
    let repo = format!("{}/{}", pr.owner, pr.repo);
    let number = pr.number.to_string();
    let out = exec.run(
        "gh",
        &[
            "pr",
            "view",
            &number,
            "--repo",
            &repo,
            "--json",
            "state,isDraft,headRefOid,baseRefName,title,body,author",
        ],
        None,
    )?;
    let v: Value = serde_json::from_str(&out).map_err(|e| format!("gh pr view: {e}"))?;
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    Ok(PrInfo {
        state: s("state"),
        draft: v.get("isDraft").and_then(Value::as_bool).unwrap_or(false),
        head: s("headRefOid"),
        base: s("baseRefName"),
        title: s("title"),
        body: s("body"),
        author: v
            .pointer("/author/login")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

/// The GitHub login the token (or gh login) belongs to, asked once.
pub fn login(exec: &Exec) -> Result<String, String> {
    static LOGIN: OnceLock<String> = OnceLock::new();
    if let Some(login) = LOGIN.get() {
        return Ok(login.clone());
    }
    let login = exec
        .run("gh", &["api", "user", "--jq", ".login"], None)?
        .trim()
        .to_string();
    if login.is_empty() {
        return Err("gh api user returned no login".to_string());
    }
    Ok(LOGIN.get_or_init(|| login).clone())
}

/// Each element of a paginated JSON array endpoint, as gh prints them with
/// `--jq '.[]'`: one compact object per line.
fn list(exec: &Exec, path: &str) -> Result<Vec<Value>, String> {
    let out = exec.run("gh", &["api", path, "--paginate", "--jq", ".[]"], None)?;
    Ok(out
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect())
}

fn by(item: &Value, login: &str) -> bool {
    item.pointer("/user/login").and_then(Value::as_str) == Some(login)
}

fn s(item: &Value, key: &str) -> String {
    item.get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn n(item: &Value, key: &str) -> i64 {
    item.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn pulls(pr: &PrRef, what: &str) -> String {
    format!("repos/{}/{}/pulls/{}/{what}", pr.owner, pr.repo, pr.number)
}

/// Whether `login` already left a review or a review comment on commit `head`,
/// by prk or by hand, here or on another machine.
pub fn reviewed_at(exec: &Exec, pr: &PrRef, login: &str, head: &str) -> Result<bool, String> {
    let reviewed = list(exec, &pulls(pr, "reviews"))?
        .iter()
        .any(|r| by(r, login) && s(r, "commit_id") == head);
    if reviewed {
        return Ok(true);
    }
    Ok(list(exec, &pulls(pr, "comments"))?
        .iter()
        .any(|c| by(c, login) && (s(c, "commit_id") == head || s(c, "original_commit_id") == head)))
}

/// `login`'s inline review comments on the PR that start a thread (not replies).
pub fn top_comments(exec: &Exec, pr: &PrRef, login: &str) -> Result<Vec<Comment>, String> {
    Ok(list(exec, &pulls(pr, "comments"))?
        .iter()
        .filter(|c| by(c, login) && n(c, "in_reply_to_id") == 0)
        .map(|c| inline(c, 0))
        .collect())
}

fn inline(c: &Value, job_id: i64) -> Comment {
    let line = match n(c, "line") {
        0 => n(c, "original_line"),
        line => line,
    };
    let reply_to = n(c, "in_reply_to_id");
    Comment {
        job_id,
        gh_id: n(c, "id"),
        kind: if reply_to == 0 { "inline" } else { "reply" }.to_string(),
        path: s(c, "path"),
        line,
        body: s(c, "body"),
        diff_hunk: s(c, "diff_hunk"),
        url: s(c, "html_url"),
        in_reply_to: reply_to,
        commit: s(c, "original_commit_id"),
        created_at: s(c, "created_at"),
        ..Default::default()
    }
}

/// Everything `login` posted on the PR at or after `since` (RFC 3339): inline
/// comments and replies, review bodies, and conversation comments.
pub fn posted_since(
    exec: &Exec,
    pr: &PrRef,
    login: &str,
    since: &str,
    job_id: i64,
) -> Result<Vec<Comment>, String> {
    let mut found: Vec<Comment> = list(exec, &pulls(pr, "comments"))?
        .iter()
        .filter(|c| by(c, login) && s(c, "created_at").as_str() >= since)
        .map(|c| inline(c, job_id))
        .collect();
    for r in list(exec, &pulls(pr, "reviews"))? {
        if by(&r, login)
            && s(&r, "submitted_at").as_str() >= since
            && !s(&r, "body").trim().is_empty()
        {
            found.push(Comment {
                job_id,
                gh_id: n(&r, "id"),
                kind: "summary".to_string(),
                body: s(&r, "body"),
                url: s(&r, "html_url"),
                commit: s(&r, "commit_id"),
                created_at: s(&r, "submitted_at"),
                ..Default::default()
            });
        }
    }
    let issue = format!(
        "repos/{}/{}/issues/{}/comments",
        pr.owner, pr.repo, pr.number
    );
    for c in list(exec, &issue)? {
        if by(&c, login) && s(&c, "created_at").as_str() >= since {
            found.push(Comment {
                job_id,
                gh_id: n(&c, "id"),
                kind: "summary".to_string(),
                body: s(&c, "body"),
                url: s(&c, "html_url"),
                created_at: s(&c, "created_at"),
                ..Default::default()
            });
        }
    }
    Ok(found)
}

/// A follow-up reply's verdict, from the marker it starts with.
pub fn verdict_of(reply: &str) -> Option<&'static str> {
    let text = reply.trim_start();
    if text.starts_with('✅') {
        Some("fixed")
    } else if text.starts_with("⚠") {
        Some("open")
    } else if text.starts_with('↔') {
        Some("partly")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_verdict_markers() {
        assert_eq!(verdict_of("✅ Fixed in abc: wrapped"), Some("fixed"));
        assert_eq!(verdict_of("⚠️ Still present in abc"), Some("open"));
        assert_eq!(verdict_of(" ↔️ Partly fixed"), Some("partly"));
        assert_eq!(verdict_of("Looks fine"), None);
    }

    #[test]
    fn inline_comments_fall_back_to_the_original_line_and_mark_replies() {
        let c = inline(
            &json!({"id": 5, "line": null, "original_line": 42, "path": "a.ts", "body": "x",
                    "in_reply_to_id": 3, "user": {"login": "me"}}),
            9,
        );
        assert_eq!(
            (c.line, c.kind.as_str(), c.in_reply_to, c.job_id),
            (42, "reply", 3, 9)
        );
    }
}
