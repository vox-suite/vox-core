use crate::{
    agents::tools::terminal::{
        DeviceRow, MAX_OUTPUT_CHARS, OPEN_TIMEOUT, TerminalToolError, audit_device_command,
        device_link, resolve_device, response_error,
    },
    db::Db,
    identity::{ResourceOwner, UserId},
    outbound::OutboundCallService,
    realtime::DeviceHub,
};
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

const CLAUDE_RUN_TIMEOUT: Duration = Duration::from_secs(25 * 60);

fn is_safe_repo_hint(hint: &str) -> bool {
    !hint.is_empty()
        && hint.len() <= 64
        && hint
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ResolveGithubIssueArgs {
    pub issue_number: u64,
    pub repo_hint: String,
    pub device_hint: Option<String>,
}

#[derive(Clone)]
pub struct ResolveGithubIssue {
    db: Option<Db>,
    user_id: UserId,
    owner: ResourceOwner,
    hub: DeviceHub,
    outbound: Option<Arc<OutboundCallService>>,
}

impl ResolveGithubIssue {
    pub fn new(
        db: Option<Db>,
        user_id: UserId,
        owner: ResourceOwner,
        hub: DeviceHub,
        outbound: Option<Arc<OutboundCallService>>,
    ) -> Self {
        Self {
            db,
            user_id,
            owner,
            hub,
            outbound,
        }
    }

    async fn notify_done(&self, issue_number: u64, repo_hint: &str, pr_url: &str) {
        let Some(outbound) = &self.outbound else {
            return;
        };
        let reason = format!("Issue #{issue_number} resolved");
        let opening = format!(
            "Let the user know Claude Code finished working on GitHub issue #{issue_number} in {repo_hint} and opened a pull request: {pr_url}."
        );
        let _ = outbound
            .initiate_call_for_user(self.owner, &reason, &opening, None, None)
            .await;
    }
}

fn last_marker_value<'a>(output: &'a str, marker: &str) -> Option<&'a str> {
    output
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix(marker))
}

impl Tool for ResolveGithubIssue {
    const NAME: &'static str = "resolve_github_issue";
    type Args = ResolveGithubIssueArgs;
    type Output = Value;
    type Error = TerminalToolError;

    fn description(&self) -> String {
        "Resolve a GitHub issue by number: finds the matching local repo on the user's registered computer, \
         runs Claude Code there to implement a fix and open a pull request, then calls the user once it's done. \
         This can take several minutes."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "issue_number": {
                    "type": "integer",
                    "description": "The GitHub issue number to resolve, e.g. 42."
                },
                "repo_hint": {
                    "type": "string",
                    "description": "Name (or part of the name) of the local repo directory this issue belongs to, e.g. 'vox-core'."
                },
                "device_hint": {
                    "type": "string",
                    "description": "Optional hint identifying which device to use when the user has more than one registered."
                }
            },
            "required": ["issue_number", "repo_hint"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let db = self.db.as_ref().ok_or(TerminalToolError::NotConfigured)?;
        let repo_hint = args.repo_hint.trim().to_string();
        if !is_safe_repo_hint(&repo_hint) {
            return Err(TerminalToolError::InvalidInput(
                "repo_hint must be a short name using only letters, numbers, '-', '_' or '.'"
                    .into(),
            ));
        }

        let device: DeviceRow =
            resolve_device(db, self.user_id.0, args.device_hint.as_deref()).await?;
        let link = device_link(&self.hub, &device)?;
        let _ = link.request("open_shell", json!({}), OPEN_TIMEOUT).await;

        let prompt = format!(
            "Resolve GitHub issue #{issue}: read it with `gh issue view {issue}`, implement a fix on a new branch, and open a pull request with `gh pr create --fill`.",
            issue = args.issue_number,
        );
        let command = format!(
            "REPO_DIR=$(find ~ -maxdepth 5 -type d -iname '*{repo_hint}*' -not -path '*/node_modules/*' -not -path '*/.git/*' 2>/dev/null | head -1); \
             if [ -z \"$REPO_DIR\" ]; then echo VOX_REPO_NOT_FOUND; \
             else cd \"$REPO_DIR\" && claude --permission-mode bypassPermissions -p '{prompt}'; \
             CLAUDE_EXIT=$?; PR_URL=$(gh pr view --json url -q .url 2>/dev/null); \
             echo \"VOX_CLAUDE_EXIT:$CLAUDE_EXIT\"; echo \"VOX_PR_URL:${{PR_URL:-none}}\"; fi",
            repo_hint = repo_hint,
            prompt = prompt.replace('\'', "'\\''"),
        );

        let response = link
            .request(
                "run_command",
                json!({ "command": command }),
                CLAUDE_RUN_TIMEOUT,
            )
            .await;
        let response = match response {
            Ok(response) => response,
            Err(err) => {
                audit_device_command(
                    db,
                    self.user_id.0,
                    device.id,
                    json!({ "tool": "resolve_github_issue", "issue_number": args.issue_number, "repo_hint": repo_hint, "outcome": "unreachable", "error": err.to_string() }),
                )
                .await;
                return Err(err.into());
            }
        };
        if let Some(err) = response_error(&response) {
            audit_device_command(
                db,
                self.user_id.0,
                device.id,
                json!({ "tool": "resolve_github_issue", "issue_number": args.issue_number, "repo_hint": repo_hint, "outcome": "rejected", "error": err }),
            )
            .await;
            return Err(TerminalToolError::DeviceUnavailable(err));
        }

        let mut output = response
            .get("output")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if output.chars().count() > MAX_OUTPUT_CHARS {
            output = output.chars().take(MAX_OUTPUT_CHARS).collect();
            output.push_str("\n… output truncated");
        }
        let exit_code = response.get("exit_code").and_then(Value::as_i64);

        audit_device_command(
            db,
            self.user_id.0,
            device.id,
            json!({ "tool": "resolve_github_issue", "issue_number": args.issue_number, "repo_hint": repo_hint, "outcome": "executed", "exit_code": exit_code }),
        )
        .await;

        if output.contains("VOX_REPO_NOT_FOUND") {
            return Err(TerminalToolError::InvalidInput(format!(
                "No local repo matching '{repo_hint}' was found on {}.",
                device.label
            )));
        }

        let claude_exit: Option<i64> = last_marker_value(&output, "VOX_CLAUDE_EXIT:")
            .and_then(|v| v.parse().ok());
        let pr_url = last_marker_value(&output, "VOX_PR_URL:")
            .filter(|v| !v.is_empty() && *v != "none")
            .map(str::to_string);

        if claude_exit != Some(0) {
            return Err(TerminalToolError::DeviceUnavailable(format!(
                "Claude Code did not finish successfully on issue #{} (exit code {:?}). No pull request was confirmed, so the user was not called.",
                args.issue_number, claude_exit
            )));
        }

        match &pr_url {
            Some(url) => self.notify_done(args.issue_number, &repo_hint, url).await,
            None => {
                return Ok(json!({
                    "device": device.label,
                    "issue_number": args.issue_number,
                    "repo_hint": repo_hint,
                    "pr_url": Value::Null,
                    "output": output,
                    "note": "Claude Code finished but no pull request was found for the branch; the user was not called.",
                }));
            }
        }

        Ok(json!({
            "device": device.label,
            "issue_number": args.issue_number,
            "repo_hint": repo_hint,
            "pr_url": pr_url,
            "output": output,
        }))
    }
}
