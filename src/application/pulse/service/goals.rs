use super::{PulseError, PulseService};
use crate::{
    application::pulse::measurements::{measurement_catalog, validate_definition},
    domain::{identity::Actor, pulse::*, pulse_goals::*},
};
use crate::{
    domain::spaces::{SpaceNode, SpaceState},
    storage::spaces::SpaceRepository,
};
use chrono::{Datelike, Duration, NaiveDate, Utc};
use uuid::Uuid;

const MAX_GOALS: i64 = 50;

fn today_in(timezone: &str) -> Result<NaiveDate, PulseError> {
    let tz: chrono_tz::Tz = timezone
        .parse()
        .map_err(|_| PulseError::Invalid("Choose a valid timezone".into()))?;
    Ok(Utc::now().with_timezone(&tz).date_naive())
}

/// First and last day of the goal's current period; non-recurring goals count from their start date.
fn period_bounds(
    period: Option<GoalPeriod>,
    starts_on: NaiveDate,
    today: NaiveDate,
) -> (NaiveDate, Option<NaiveDate>) {
    match period {
        Some(GoalPeriod::Week) => {
            let start = today - Duration::days(i64::from(today.weekday().num_days_from_monday()));
            (start, Some(start + Duration::days(6)))
        }
        Some(GoalPeriod::Month) => {
            let start = today.with_day(1).unwrap_or(today);
            let next = if start.month() == 12 {
                NaiveDate::from_ymd_opt(start.year() + 1, 1, 1)
            } else {
                NaiveDate::from_ymd_opt(start.year(), start.month() + 1, 1)
            };
            (start, next.map(|n| n - Duration::days(1)))
        }
        None => (starts_on.min(today), None),
    }
}

/// Builds the Pulse definition that measures a metric goal over its current window.
fn window_definition(
    base: &PulseDefinition,
    period: Option<GoalPeriod>,
    starts_on: NaiveDate,
    today: NaiveDate,
    buckets: &[Bucket],
) -> Result<PulseDefinition, String> {
    let (start, _) = period_bounds(period, starts_on, today);
    let days = ((today - start).num_days() + 1).clamp(1, 3650) as u16;
    let pick = |wanted: &[Bucket]| wanted.iter().find(|b| buckets.contains(b)).cloned();
    let bucket = match period {
        Some(GoalPeriod::Week) => pick(&[Bucket::Week]),
        Some(GoalPeriod::Month) => pick(&[Bucket::Month]),
        None if days <= 365 => pick(&[Bucket::Day, Bucket::Week, Bucket::Month]),
        None => pick(&[Bucket::Week, Bucket::Month]),
    }
    .ok_or("This measurement cannot be tracked over that period")?;
    Ok(PulseDefinition {
        version: 2,
        measurement_id: base.measurement_id.clone(),
        bucket: Some(bucket),
        dimension: None,
        period_days: days,
        offset_days: 0,
        top_n: None,
        timezone: base.timezone.clone(),
        chart_type: crate::domain::charts::ChartType::Stat,
    })
}

fn view(g: &GoalRow, current: f64, today: NaiveDate, error: Option<String>) -> GoalView {
    let (start, end) = period_bounds(g.period, g.starts_on, today);
    let horizon_end = end.or(g.deadline);
    let remaining = (g.target - current).max(0.0);
    let percent = (current / g.target * 100.0).clamp(0.0, 999.0);
    let days_left = horizon_end.map(|e| (e - today).num_days().max(0));
    let status = if error.is_some() {
        "unavailable"
    } else if g.direction == GoalDirection::AtMost {
        if current > g.target {
            "over_limit"
        } else {
            "within_limit"
        }
    } else if current >= g.target {
        "done"
    } else if let Some(end) = horizon_end {
        let total = ((end - start).num_days() + 1).max(1) as f64;
        // Days fully behind us, so a goal that starts today is not "behind" yet.
        let elapsed = ((today - start).num_days().clamp(0, total as i64)) as f64;
        let expected = g.target * elapsed / total;
        if today > end || current < expected * 0.95 {
            "behind"
        } else if current > expected * 1.05 {
            "ahead"
        } else {
            "on_track"
        }
    } else {
        "in_progress"
    };
    let open = g.direction == GoalDirection::AtLeast && current < g.target && error.is_none();
    let per_week_needed = (open && days_left.is_some_and(|d| d > 0))
        .then(|| remaining / days_left.unwrap_or(1) as f64 * 7.0);
    let elapsed_days = ((today - g.starts_on).num_days() + 1).max(1) as f64;
    let projected_on = (open && g.period.is_none() && current > 0.0 && elapsed_days >= 7.0)
        .then(|| today + Duration::days((remaining / (current / elapsed_days)).ceil() as i64));
    GoalView {
        id: g.id,
        title: g.title.clone(),
        kind: g.kind,
        direction: g.direction,
        period: g.period,
        target: g.target,
        unit: g.unit.clone(),
        current,
        percent,
        status: status.into(),
        remaining,
        starts_on: g.starts_on,
        deadline: g.deadline,
        period_ends_on: end,
        days_left,
        per_week_needed,
        projected_on,
        error,
    }
}

/// Turns a model- or agent-written suggestion into a draft; the server validates it afterwards.
pub(super) fn draft_from_suggestion(raw: GoalSuggestion, timezone: &str) -> GoalDraft {
    let definition = match (raw.kind, raw.measurement_id) {
        (GoalKind::Metric, Some(id)) => Some(PulseDefinition {
            version: 2,
            measurement_id: id,
            bucket: Some(Bucket::Week),
            dimension: None,
            period_days: 30,
            offset_days: 0,
            top_n: None,
            timezone: timezone.to_string(),
            chart_type: crate::domain::charts::ChartType::Stat,
        }),
        _ => None,
    };
    GoalDraft {
        timezone: timezone.to_string(),
        title: raw.title,
        kind: raw.kind,
        direction: raw.direction,
        period: raw.period,
        target: raw.target,
        unit: raw.unit.unwrap_or_default(),
        definition,
        deadline: raw.deadline,
    }
}

fn row_from_draft(draft: &GoalDraft, today: NaiveDate) -> GoalRow {
    GoalRow {
        id: Uuid::nil(),
        title: draft.title.trim().into(),
        kind: draft.kind,
        direction: draft.direction,
        period: draft.period,
        target: draft.target,
        unit: draft.unit.trim().into(),
        definition: draft.definition.clone(),
        starts_on: today,
        deadline: draft.deadline,
        created_at: Utc::now(),
        saved: 0.0,
    }
}

impl PulseService {
    async fn goal_views(
        &self,
        actor: &Actor,
        rows: Vec<GoalRow>,
        timezone: &str,
    ) -> Result<Vec<GoalView>, PulseError> {
        let today = today_in(timezone)?;
        let mut views: Vec<Option<GoalView>> = vec![None; rows.len()];
        let mut inputs = vec![];
        let mut slots = vec![];
        if rows.iter().any(|g| g.kind == GoalKind::Metric) {
            let meta = self.repo.metadata(actor.user_id, None).await?;
            let inventory = self.inventory(actor, &meta, false).await?;
            let catalog = measurement_catalog(&inventory.profiles);
            for (index, g) in rows.iter().enumerate() {
                if g.kind != GoalKind::Metric {
                    continue;
                }
                let built = g
                    .definition
                    .as_ref()
                    .ok_or("Goal has no measurement")
                    .and_then(|base| {
                        let m = catalog
                            .iter()
                            .find(|m| m.id == base.measurement_id)
                            .ok_or("Measurement is unavailable or its source access has changed")?;
                        let mut def =
                            window_definition(base, g.period, g.starts_on, today, &m.buckets)
                                .map_err(
                                    |_| "This measurement cannot be tracked over that period",
                                )?;
                        def.timezone = timezone.into();
                        let m = validate_definition(&def, &catalog)
                            .map_err(|_| "This measurement cannot be tracked over that period")?;
                        Ok((def, m))
                    });
                match built {
                    Ok(pair) => {
                        inputs.push(pair);
                        slots.push(index);
                    }
                    Err(message) => views[index] = Some(view(g, 0.0, today, Some(message.into()))),
                }
            }
            for (chunk, chunk_slots) in inputs.chunks(12).zip(slots.chunks(12)) {
                let results = self.results(actor, &meta, chunk, false, Utc::now()).await?;
                for (slot, result) in chunk_slots.iter().zip(results) {
                    views[*slot] = Some(view(
                        &rows[*slot],
                        result.total.unwrap_or(0.0),
                        today,
                        result.error,
                    ));
                }
            }
        }
        Ok(rows
            .iter()
            .zip(views)
            .map(|(g, v)| v.unwrap_or_else(|| view(g, g.saved, today, None)))
            .collect())
    }
    pub async fn goals(&self, actor: &Actor, timezone: &str) -> Result<Vec<GoalView>, PulseError> {
        Self::timezone(timezone)?;
        let rows = self.repo.list_goals(actor.user_id).await?;
        self.goal_views(actor, rows, timezone).await
    }
    /// Checks a draft against the user's real measurements and returns the cleaned version.
    async fn validate_draft(
        &self,
        actor: &Actor,
        mut draft: GoalDraft,
    ) -> Result<GoalDraft, PulseError> {
        let today = today_in(&draft.timezone)?;
        let invalid = |m: &str| PulseError::Invalid(m.into());
        draft.title = draft.title.trim().chars().take(120).collect();
        if draft.title.is_empty() {
            return Err(invalid("Give the goal a title"));
        }
        if !draft.target.is_finite() || draft.target <= 0.0 || draft.target >= 1e12 {
            return Err(invalid("Choose a target above zero"));
        }
        if draft.deadline.is_some_and(|d| d < today) {
            return Err(invalid("The deadline is in the past"));
        }
        match draft.kind {
            GoalKind::Saving => {
                draft.direction = GoalDirection::AtLeast;
                draft.period = None;
                draft.definition = None;
                draft.unit = draft.unit.trim().into();
                if draft.unit.is_empty() || draft.unit.chars().count() > 16 {
                    return Err(invalid("Choose a unit such as INR or ₹"));
                }
            }
            GoalKind::Metric => {
                let base = draft
                    .definition
                    .clone()
                    .ok_or_else(|| invalid("Pick what to measure"))?;
                if draft.period.is_some() {
                    draft.deadline = None;
                }
                let meta = self.repo.metadata(actor.user_id, None).await?;
                let inventory = self.inventory(actor, &meta, false).await?;
                let catalog = measurement_catalog(&inventory.profiles);
                let m = catalog
                    .iter()
                    .find(|m| m.id == base.measurement_id)
                    .ok_or_else(|| invalid("That measurement is unavailable"))?;
                let def = window_definition(&base, draft.period, today, today, &m.buckets)
                    .map_err(PulseError::Invalid)?;
                let m = validate_definition(&def, &catalog).map_err(PulseError::Invalid)?;
                draft.unit = m.unit.clone();
                draft.definition = Some(PulseDefinition {
                    timezone: draft.timezone.clone(),
                    ..def
                });
            }
        }
        Ok(draft)
    }
    pub async fn create_goal(
        &self,
        actor: &Actor,
        draft: GoalDraft,
    ) -> Result<GoalView, PulseError> {
        self.create_goal_for(actor, draft, None).await
    }
    async fn create_goal_for(
        &self,
        actor: &Actor,
        draft: GoalDraft,
        space_node_id: Option<Uuid>,
    ) -> Result<GoalView, PulseError> {
        let draft = self.validate_draft(actor, draft).await?;
        if self.repo.count_goals(actor.user_id).await? >= MAX_GOALS {
            return Err(PulseError::Invalid(
                "You have reached the goal limit".into(),
            ));
        }
        let today = today_in(&draft.timezone)?;
        let id = self
            .repo
            .insert_goal(actor.user_id, &draft, today, space_node_id)
            .await?;
        let row = self
            .repo
            .get_goal(actor.user_id, id)
            .await?
            .ok_or_else(|| PulseError::Invalid("Goal not found".into()))?;
        Ok(self
            .goal_views(actor, vec![row], &draft.timezone)
            .await?
            .remove(0))
    }
    pub async fn delete_goal(&self, actor: &Actor, id: Uuid) -> Result<(), PulseError> {
        if self.repo.delete_goal(actor.user_id, id).await? {
            Ok(())
        } else {
            Err(PulseError::Invalid("Goal not found".into()))
        }
    }
    pub async fn add_goal_entry(
        &self,
        actor: &Actor,
        id: Uuid,
        input: AddGoalEntryInput,
    ) -> Result<GoalView, PulseError> {
        let today = today_in(&input.timezone)?;
        if !input.amount.is_finite() || input.amount == 0.0 || input.amount.abs() >= 1e12 {
            return Err(PulseError::Invalid("Enter a non-zero amount".into()));
        }
        let goal = self
            .repo
            .get_goal(actor.user_id, id)
            .await?
            .filter(|g| g.kind == GoalKind::Saving)
            .ok_or_else(|| PulseError::Invalid("Only saving goals take contributions".into()))?;
        let note: String = input
            .note
            .unwrap_or_default()
            .trim()
            .chars()
            .take(200)
            .collect();
        self.repo
            .add_goal_entry(actor.user_id, goal.id, input.amount, &note, today)
            .await?;
        let row = self
            .repo
            .get_goal(actor.user_id, id)
            .await?
            .ok_or_else(|| PulseError::Invalid("Goal not found".into()))?;
        Ok(self
            .goal_views(actor, vec![row], &input.timezone)
            .await?
            .remove(0))
    }
    /// Creates the goal an agent proposed on a space node, after the user pressed approve,
    /// and stores the goal id back on the node.
    pub async fn approve_node_goal(
        &self,
        actor: &Actor,
        space_id: Uuid,
        node_id: Uuid,
        timezone: &str,
    ) -> Result<(GoalView, SpaceNode), PulseError> {
        let spaces = SpaceRepository::new(self.repo.pool.clone());
        let invalid = |m: &str| PulseError::Invalid(m.into());
        let space = spaces
            .get_space(actor.user_id, space_id)
            .await?
            .ok_or_else(|| invalid("Space not found"))?;
        if space.state == SpaceState::Dropped {
            return Err(invalid("This space was dropped"));
        }
        let lock = self.lock(&format!("goal-node:{node_id}")).await?;
        let _guard = lock.lock().await;
        let node = spaces
            .get_node(space_id, node_id)
            .await?
            .ok_or_else(|| invalid("Node not found"))?;
        if let Some(existing) = self.repo.goal_for_node(actor.user_id, node_id).await? {
            let view = self
                .goal_views(actor, vec![existing], timezone)
                .await?
                .remove(0);
            return Ok((view, node));
        }
        let proposal: GoalSuggestion = node
            .data
            .get("goal_proposal")
            .cloned()
            .and_then(|p| serde_json::from_value(p).ok())
            .ok_or_else(|| invalid("This node has no goal proposal"))?;
        let draft = draft_from_suggestion(proposal, timezone);
        let view = self.create_goal_for(actor, draft, Some(node_id)).await?;
        let mut data = node.data.clone();
        if let Some(fields) = data.as_object_mut() {
            fields.insert("goal_id".into(), serde_json::json!(view.id));
            if let Some(p) = fields
                .get_mut("goal_proposal")
                .and_then(|p| p.as_object_mut())
            {
                p.insert("status".into(), serde_json::json!("approved"));
            }
        }
        let node = spaces
            .update_node(space_id, node_id, None, None, Some(data), None, None, None)
            .await?
            .ok_or_else(|| invalid("Node not found"))?;
        Ok((view, node))
    }
    pub async fn compose_goal(
        &self,
        actor: &Actor,
        input: GoalComposeInput,
    ) -> Result<GoalComposeResponse, PulseError> {
        let today = today_in(&input.timezone)?;
        if input.messages.is_empty() || input.messages.len() > 40 {
            return Err(PulseError::Invalid("Describe your goal".into()));
        }
        let Some(suggester) = &self.suggester else {
            return Err(PulseError::Invalid("Goal assistant is unavailable".into()));
        };
        let meta = self.repo.metadata(actor.user_id, None).await?;
        let inventory = self.inventory(actor, &meta, false).await?;
        let catalog = measurement_catalog(&inventory.profiles);
        let timezone = input.timezone.clone();
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            suggester.compose_goal(
                catalog.iter().take(60).cloned().collect(),
                today.to_string(),
                input,
            ),
        )
        .await
        .map_err(|_| PulseError::Busy)?
        .map_err(|_| PulseError::Invalid("The assistant could not answer. Try again.".into()))?;
        let reply: String = out.reply.chars().take(600).collect();
        let Some(raw) = out.draft else {
            return Ok(GoalComposeResponse {
                reply,
                draft: None,
                preview: None,
            });
        };
        let draft = draft_from_suggestion(raw, &timezone);
        match self.validate_draft(actor, draft).await {
            Ok(draft) => {
                let row = row_from_draft(&draft, today);
                let preview = self
                    .goal_views(actor, vec![row], &timezone)
                    .await?
                    .remove(0);
                Ok(GoalComposeResponse {
                    reply,
                    draft: Some(draft),
                    preview: Some(preview),
                })
            }
            Err(PulseError::Invalid(message)) => Ok(GoalComposeResponse {
                reply: format!("{reply} (I couldn't set that up: {message}.)"),
                draft: None,
                preview: None,
            }),
            Err(other) => Err(other),
        }
    }
}
