use crate::{
    agents::tools::terminal::{
        DeviceRow, OPEN_TIMEOUT, TerminalToolError, audit_device_command, device_link,
        resolve_device, response_error,
    },
    db::Db,
    identity::{ResourceOwner, UserId},
    outbound::OutboundCallService,
    realtime::{DeviceHub, DeviceLink},
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

    async fn notify(&self, reason: &str, opening: &str) {
        let Some(outbound) = &self.outbound else {
            return;
        };
        if let Err(err) = outbound
            .initiate_call_for_user(self.owner, reason, opening, None, None)
            .await
        {
            tracing::warn!(?err, "resolve_github_issue callback call failed");
        }
    }

    /// Runs Claude Code on the device and returns the opened PR's URL, or a
    /// message saying why no pull request was confirmed.
    async fn run(
        &self,
        db: &Db,
        device: &DeviceRow,
        link: &DeviceLink,
        issue_number: u64,
        repo_hint: &str,
    ) -> Result<String, String> {
        let prompt = format!(
            "Resolve GitHub issue #{issue_number}: read it with `gh issue view {issue_number}`, implement a fix on a new branch, and open a pull request with `gh pr create --fill`.",
        );
        let command = format!(
            "REPO_DIR=$(find ~ -maxdepth 5 -type d -iname '*{repo_hint}*' -not -path '*/node_modules/*' -not -path '*/.git/*' 2>/dev/null | head -1); \
             if [ -z \"$REPO_DIR\" ]; then echo VOX_REPO_NOT_FOUND; \
             else cd \"$REPO_DIR\" && claude --permission-mode bypassPermissions -p '{prompt}'; \
             CLAUDE_EXIT=$?; PR_URL=$(gh pr view --json url -q .url 2>/dev/null); \
             echo \"VOX_CLAUDE_EXIT:$CLAUDE_EXIT\"; echo \"VOX_PR_URL:${{PR_URL:-none}}\"; fi",
            prompt = prompt.replace('\'', "'\\''"),
        );

        let audit = |outcome: Value| {
            let mut entry = json!({ "tool": "resolve_github_issue", "issue_number": issue_number, "repo_hint": repo_hint });
            if let (Some(entry), Some(outcome)) = (entry.as_object_mut(), outcome.as_object()) {
                entry.extend(outcome.clone());
            }
            audit_device_command(db, self.user_id.0, device.id, entry)
        };

        let response = match link
            .request(
                "run_command",
                json!({ "command": command }),
                CLAUDE_RUN_TIMEOUT,
            )
            .await
        {
            Ok(response) => response,
            Err(err) => {
                audit(json!({ "outcome": "unreachable", "error": err.to_string() })).await;
                return Err(format!("{} could not be reached: {err}", device.label));
            }
        };
        if let Some(err) = response_error(&response) {
            audit(json!({ "outcome": "rejected", "error": err })).await;
            return Err(format!("{} rejected the command: {err}", device.label));
        }

        let output = response.get("output").and_then(Value::as_str).unwrap_or("");
        let exit_code = response.get("exit_code").and_then(Value::as_i64);
        audit(json!({ "outcome": "executed", "exit_code": exit_code })).await;

        if output.contains("VOX_REPO_NOT_FOUND") {
            return Err(format!(
                "no local repo matching '{repo_hint}' was found on {}",
                device.label
            ));
        }
        let claude_exit: Option<i64> =
            last_marker_value(output, "VOX_CLAUDE_EXIT:").and_then(|v| v.parse().ok());
        if claude_exit != Some(0) {
            return Err(format!(
                "Claude Code did not finish successfully (exit code {claude_exit:?})"
            ));
        }
        last_marker_value(output, "VOX_PR_URL:")
            .filter(|v| !v.is_empty() && *v != "none")
            .map(str::to_string)
            .ok_or_else(|| "Claude Code finished but no pull request was found".to_string())
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

        let tool = self.clone();
        let db = db.clone();
        let issue_number = args.issue_number;
        let task_repo_hint = repo_hint.clone();
        let task_device = device.clone();
        tokio::spawn(async move {
            let repo_hint = task_repo_hint;
            let device = task_device;
            match tool.run(&db, &device, &link, issue_number, &repo_hint).await {
                Ok(pr_url) => {
                    tool.notify(
                        &format!("Issue #{issue_number} resolved"),
                        &format!("Let the user know Claude Code finished working on GitHub issue #{issue_number} in {repo_hint} and opened a pull request: {pr_url}."),
                    )
                    .await
                }
                Err(reason) => {
                    tracing::warn!(issue_number, %repo_hint, %reason, "resolve_github_issue failed");
                    tool.notify(
                        &format!("Issue #{issue_number} not resolved"),
                        &format!("Let the user know Claude Code could not open a pull request for GitHub issue #{issue_number} in {repo_hint}: {reason}."),
                    )
                    .await
                }
            }
        });

        Ok(json!({
            "status": "started",
            "device": device.label,
            "issue_number": issue_number,
            "repo_hint": repo_hint,
            "note": "Claude Code is working on this in the background and may take several minutes. The user will get a phone call when the pull request is opened or if it fails.",
        }))
    }
}
