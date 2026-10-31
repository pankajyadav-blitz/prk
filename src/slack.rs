//! Which Slack messages ask for a review, and which PRs they point at.

use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrRef {
    pub owner: String,
    pub repo: String,
    pub number: u64,
}

impl PrRef {
    pub fn url(&self) -> String {
        format!(
            "https://github.com/{}/{}/pull/{}",
            self.owner, self.repo, self.number
        )
    }

    /// `owner/repo#n`, the key reviews are deduped on.
    pub fn key(&self) -> String {
        format!("{}/{}#{}", self.owner, self.repo, self.number)
    }

    /// Parse a PR URL as given on the command line.
    pub fn parse(url: &str) -> Option<PrRef> {
        let c = pr_regex().captures(url)?;
        Some(PrRef {
            owner: c[1].to_string(),
            repo: c[2].to_string(),
            number: c[3].parse().ok()?,
        })
    }
}

fn pr_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"https?://github\.com/([A-Za-z0-9_.-]+)/([A-Za-z0-9_.-]+)/pull/(\d+)").unwrap()
    })
}

/// Every PR link in `text` whose owner is in `orgs`, each once, in order.
pub fn pr_links(text: &str, orgs: &[String]) -> Vec<PrRef> {
    let mut found: Vec<PrRef> = Vec::new();
    for c in pr_regex().captures_iter(text) {
        let Ok(number) = c[3].parse() else { continue };
        let pr = PrRef {
            owner: c[1].to_string(),
            repo: c[2].to_string(),
            number,
        };
        if orgs.iter().any(|o| o.eq_ignore_ascii_case(&pr.owner)) && !found.contains(&pr) {
            found.push(pr);
        }
    }
    found
}

/// A message that asks the reviewer for a review.
#[derive(Debug, PartialEq, Eq)]
pub struct Request {
    pub ts: String,
    pub user: String,
    pub prs: Vec<PrRef>,
}

/// Slack writes a mention as `<@U123>` or `<@U123|name>`.
fn mentions(text: &str, reviewer: &str) -> bool {
    text.contains(&format!("<@{reviewer}>")) || text.contains(&format!("<@{reviewer}|"))
}

fn text_of(message: &Value) -> &str {
    message.get("text").and_then(Value::as_str).unwrap_or("")
}

/// The review request in a Slack `message` event, if it is one: posted by a person
/// in `channel`, tagging `reviewer`, with at least one PR link from `orgs`. A
/// message edited to add the tag counts; an edit to one that already had it does
/// not, so fixing a typo does not queue the same PRs again.
pub fn request(event: &Value, channel: &str, reviewer: &str, orgs: &[String]) -> Option<Request> {
    if event.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    if event.get("channel").and_then(Value::as_str) != Some(channel) {
        return None;
    }
    let message = match event.get("subtype").and_then(Value::as_str) {
        None | Some("thread_broadcast") | Some("file_share") => event,
        Some("message_changed") => {
            let before = event.get("previous_message").map(text_of).unwrap_or("");
            if mentions(before, reviewer) {
                return None;
            }
            event.get("message")?
        }
        Some(_) => return None,
    };
    if message.get("bot_id").is_some() {
        return None;
    }
    let user = message.get("user").and_then(Value::as_str)?;
    // The reviewer's own posts ask others to review their PRs.
    if user == reviewer {
        return None;
    }
    let text = text_of(message);
    if !mentions(text, reviewer) {
        return None;
    }
    let prs = pr_links(text, orgs);
    if prs.is_empty() {
        return None;
    }
    Some(Request {
        ts: message
            .get("ts")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        user: user.to_string(),
        prs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn orgs() -> Vec<String> {
        vec!["Blitzscale-19".to_string()]
    }

    fn msg(text: &str) -> Value {
        json!({"type": "message", "channel": "C1", "user": "U9", "ts": "1.0", "text": text})
    }

    #[test]
    fn finds_slack_formatted_links_once_and_only_from_allowed_orgs() {
        let text = "<@U1> please review: <https://github.com/Blitzscale-19/suchna/pull/826|github.com/Blitzscale-19/suchna/pull/826> \
                    <https://github.com/Blitzscale-19/alfred/pull/2014/changes|x> \
                    https://github.com/someone-else/repo/pull/3";
        let links = pr_links(text, &orgs());
        assert_eq!(
            links.iter().map(PrRef::key).collect::<Vec<_>>(),
            vec!["Blitzscale-19/suchna#826", "Blitzscale-19/alfred#2014"]
        );
    }

    #[test]
    fn a_tag_with_a_pr_link_is_a_request() {
        let req = request(
            &msg("<@U1> please review: <https://github.com/Blitzscale-19/suchna/pull/826|x>"),
            "C1",
            "U1",
            &orgs(),
        )
        .unwrap();
        assert_eq!(req.prs[0].number, 826);
        assert_eq!(req.user, "U9");
    }

    #[test]
    fn other_tags_channels_bots_and_link_less_messages_are_ignored() {
        let link = "<https://github.com/Blitzscale-19/suchna/pull/1|x>";
        assert!(request(&msg(&format!("<@U2> review {link}")), "C1", "U1", &orgs()).is_none());
        assert!(request(&msg("<@U1> please review my change"), "C1", "U1", &orgs()).is_none());
        assert!(request(&msg(&format!("<@U1> {link}")), "C2", "U1", &orgs()).is_none());
        let mut bot = msg(&format!("<@U1> {link}"));
        bot["bot_id"] = json!("B1");
        assert!(request(&bot, "C1", "U1", &orgs()).is_none());
        let mut own = msg(&format!("<@U1> {link}"));
        own["user"] = json!("U1");
        assert!(request(&own, "C1", "U1", &orgs()).is_none());
    }

    #[test]
    fn an_edit_counts_only_when_it_adds_the_tag() {
        let link = "<https://github.com/Blitzscale-19/suchna/pull/1|x>";
        let edit = |before: &str, after: &str| {
            json!({
                "type": "message", "subtype": "message_changed", "channel": "C1",
                "message": {"user": "U9", "ts": "1.0", "text": after},
                "previous_message": {"user": "U9", "ts": "1.0", "text": before},
            })
        };
        let added = edit(&format!("review {link}"), &format!("<@U1> review {link}"));
        assert!(request(&added, "C1", "U1", &orgs()).is_some());
        let retyped = edit(
            &format!("<@U1> reveiw {link}"),
            &format!("<@U1> review {link}"),
        );
        assert!(request(&retyped, "C1", "U1", &orgs()).is_none());
    }

    #[test]
    fn parses_a_command_line_url() {
        let pr = PrRef::parse("https://github.com/Blitzscale-19/suchna/pull/826/files").unwrap();
        assert_eq!(pr.key(), "Blitzscale-19/suchna#826");
        assert!(PrRef::parse("https://github.com/Blitzscale-19/suchna").is_none());
    }
}
