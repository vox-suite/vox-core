//! One-off cleanup: merges duplicate LLM-created finance/telecom/utilities schemas into canonical
//! names, moves their spans, and remaps saved Pulse charts to the new measurement ids.
//!
//! Dry run by default. Pass `--apply` to change data. Writes a JSON backup before applying.

use serde_json::{Value, json};
use sqlx::Row;
use std::{collections::HashMap, env};
use uuid::Uuid;
use vox_core::{
    application::pulse::measurements::{definition_hash, measurement_catalog},
    db::Db,
    domain::pulse::{Measurement, PulseDefinition},
    storage::pulse::PulseRepository,
};

const MAPPING: &[(&str, &str, &[&str])] = &[
    (
        "finance",
        "card_bill_payment",
        &[
            "credit_card_payment_received",
            "credit_card_bill_payment_received",
            "credit_card_payment",
        ],
    ),
    (
        "finance",
        "card_payment",
        &["credit_card_transaction", "card_authorization_otp"],
    ),
    ("finance", "bill_due", &["credit_card_statement"]),
    ("finance", "bank_debit", &["bank_transaction"]),
    (
        "finance",
        "subscription_charge",
        &["nach_debit", "autopay_subscription"],
    ),
    (
        "finance",
        "autopay_mandate_setup",
        &["upi_mandate_creation"],
    ),
    (
        "finance",
        "refund",
        &[
            "merchant_refund",
            "merchant_refund_initiated",
            "card_credit_refund",
            "credit_card_refund",
        ],
    ),
    ("finance", "reward_points", &["loyalty_points"]),
    (
        "finance",
        "investment_activity",
        &[
            "mutual_fund_sip",
            "mutual_fund_redemption",
            "demat_ipo_credit",
            "broker_balance_statement",
        ],
    ),
    (
        "telecom",
        "call_usage",
        &[
            "call_charge_log",
            "voice_call_charge",
            "voice_call_usage_log",
        ],
    ),
    (
        "utilities",
        "broadband_bill_payment",
        &["broadband_payment_confirmation"],
    ),
];

struct Plan {
    user: Uuid,
    canonical: &'static str,
    namespace: &'static str,
    old_ids: Vec<Uuid>,
    old_names: Vec<String>,
}

fn same_series(a: &Measurement, b: &Measurement, canonical_schema: Uuid) -> bool {
    a.kind == b.kind
        && a.field == b.field
        && b.profile.schema_id == Some(canonical_schema)
        && a.profile.source == b.profile.source
        && a.profile.action == b.profile.action
        && a.profile.timing == b.profile.timing
        && a.profile.currency == b.profile.currency
        && a.profile.connection_id == b.profile.connection_id
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let apply = env::args().any(|a| a == "--apply");
    let db = Db::connect(&env::var("DATABASE_URL")?).await?;
    let pool = db.pool();
    let pulse = PulseRepository::new(pool.clone());

    let users: Vec<Uuid> =
        sqlx::query_scalar("SELECT DISTINCT user_id FROM data_schemas WHERE user_id IS NOT NULL")
            .fetch_all(pool)
            .await?;

    let mut plans = Vec::new();
    for user in users {
        for (namespace, canonical, olds) in MAPPING {
            let rows = sqlx::query(
                "SELECT id, name FROM data_schemas WHERE user_id = $1 AND namespace = $2 AND name = ANY($3)",
            )
            .bind(user)
            .bind(namespace)
            .bind(olds.iter().map(|s| s.to_string()).collect::<Vec<_>>())
            .fetch_all(pool)
            .await?;
            if rows.is_empty() {
                continue;
            }
            plans.push(Plan {
                user,
                canonical,
                namespace,
                old_ids: rows.iter().map(|r| r.get("id")).collect(),
                old_names: rows.iter().map(|r| r.get("name")).collect(),
            });
        }
    }

    for plan in &plans {
        let spans: i64 = sqlx::query_scalar("SELECT count(*) FROM spans WHERE schema_id = ANY($1)")
            .bind(&plan.old_ids)
            .fetch_one(pool)
            .await?;
        println!(
            "user {}: {}.{{{}}} -> {}  ({} spans)",
            &plan.user.to_string()[..8],
            plan.namespace,
            plan.old_names.join(", "),
            plan.canonical,
            spans
        );
    }
    if plans.is_empty() {
        println!("nothing to merge");
        return Ok(());
    }
    if !apply {
        for user in plans
            .iter()
            .map(|p| p.user)
            .collect::<std::collections::BTreeSet<_>>()
        {
            let catalog = measurement_catalog(&pulse.inventory(user).await?.profiles);
            let rows =
                sqlx::query("SELECT title, definition FROM pulse_saved_charts WHERE user_id = $1")
                    .bind(user)
                    .fetch_all(pool)
                    .await?;
            for row in rows {
                let definition: Value = row.get("definition");
                let Some(m) = catalog
                    .iter()
                    .find(|m| Some(m.id.as_str()) == definition["measurement_id"].as_str())
                else {
                    println!(
                        "chart {:?}: measurement not found today (already broken)",
                        row.get::<String, _>("title")
                    );
                    continue;
                };
                let affected = plans.iter().any(|p| {
                    p.user == user && m.profile.schema_id.is_some_and(|s| p.old_ids.contains(&s))
                });
                println!(
                    "chart {:?}: {} ({}) {}",
                    row.get::<String, _>("title"),
                    m.title,
                    m.profile.category,
                    if affected {
                        "WILL BE REMAPPED"
                    } else {
                        "unaffected"
                    }
                );
            }
        }
        println!("\ndry run only; re-run with --apply to change data");
        return Ok(());
    }

    let mut backup = json!({"schemas": [], "spans": [], "charts": []});
    let mut charts: HashMap<Uuid, Vec<(Uuid, Value, Measurement)>> = HashMap::new();
    for user in plans
        .iter()
        .map(|p| p.user)
        .collect::<std::collections::BTreeSet<_>>()
    {
        let catalog = measurement_catalog(&pulse.inventory(user).await?.profiles);
        let rows = sqlx::query("SELECT id, definition FROM pulse_saved_charts WHERE user_id = $1")
            .bind(user)
            .fetch_all(pool)
            .await?;
        for row in rows {
            let id: Uuid = row.get("id");
            let raw: Value = row.get("definition");
            backup["charts"]
                .as_array_mut()
                .unwrap()
                .push(json!({"id": id, "definition": raw}));
            let definition = raw;
            if let Some(m) = catalog
                .iter()
                .find(|m| Some(m.id.as_str()) == definition["measurement_id"].as_str())
            {
                charts
                    .entry(user)
                    .or_default()
                    .push((id, definition, m.clone()));
            }
        }
    }

    let mut canonical_ids: HashMap<(Uuid, &str), Uuid> = HashMap::new();
    for plan in &plans {
        let mut tx = pool.begin().await?;
        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM data_schemas WHERE user_id = $1 AND namespace = $2 AND name = $3 ORDER BY version DESC LIMIT 1",
        )
        .bind(plan.user)
        .bind(plan.namespace)
        .bind(plan.canonical)
        .fetch_optional(&mut *tx)
        .await?;
        let canonical_id = match existing {
            Some(id) => id,
            None => {
                sqlx::query_scalar(
                    "INSERT INTO data_schemas (user_id, namespace, name, version, description, json_schema, color_token, icon_token)
                     SELECT $1, namespace, $4, 1, description, json_schema, color_token, icon_token FROM (
                         SELECT namespace, description, json_schema, color_token, icon_token, 0 AS rank FROM data_schemas
                         WHERE user_id IS NULL AND namespace = $2 AND name = $4
                         UNION ALL
                         SELECT namespace, description, json_schema, color_token, icon_token, 1 FROM data_schemas
                         WHERE id = $3
                     ) source ORDER BY rank LIMIT 1
                     RETURNING id",
                )
                .bind(plan.user)
                .bind(plan.namespace)
                .bind(plan.old_ids[0])
                .bind(plan.canonical)
                .fetch_one(&mut *tx)
                .await?
            }
        };
        canonical_ids.insert((plan.user, plan.canonical), canonical_id);

        for row in
            sqlx::query("SELECT id, schema_id, category FROM spans WHERE schema_id = ANY($1)")
                .bind(&plan.old_ids)
                .fetch_all(&mut *tx)
                .await?
        {
            backup["spans"].as_array_mut().unwrap().push(json!({
                "id": row.get::<Uuid, _>("id"),
                "schema_id": row.get::<Uuid, _>("schema_id"),
                "old_category": row.get::<String, _>("category"),
            }));
        }
        let rows = sqlx::query("SELECT to_jsonb(d) AS row FROM data_schemas d WHERE id = ANY($1)")
            .bind(&plan.old_ids)
            .fetch_all(&mut *tx)
            .await?;
        for row in rows {
            backup["schemas"]
                .as_array_mut()
                .unwrap()
                .push(row.get("row"));
        }
        sqlx::query("UPDATE spans SET schema_id = $1, category = $2 WHERE schema_id = ANY($3)")
            .bind(canonical_id)
            .bind(plan.canonical)
            .bind(&plan.old_ids)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM data_schemas WHERE id = ANY($1)")
            .bind(&plan.old_ids)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }

    let path = format!(
        "schema-merge-backup-{}.json",
        chrono::Utc::now().timestamp()
    );
    std::fs::write(&path, serde_json::to_string_pretty(&backup)?)?;
    println!("backup written to {path}");

    for (user, list) in charts {
        let after = measurement_catalog(&pulse.inventory(user).await?.profiles);
        for (chart_id, mut definition, old) in list {
            let target_schema = plans
                .iter()
                .find(|p| {
                    p.user == user
                        && old
                            .profile
                            .schema_id
                            .is_some_and(|s| p.old_ids.contains(&s))
                })
                .and_then(|p| canonical_ids.get(&(user, p.canonical)).copied());
            let Some(schema) = target_schema else {
                continue;
            };
            let Some(new) = after.iter().find(|m| same_series(&old, m, schema)) else {
                println!("WARNING chart {chart_id}: no matching measurement after merge");
                continue;
            };
            definition["measurement_id"] = json!(new.id);
            let hash = serde_json::from_value::<PulseDefinition>(definition.clone())
                .ok()
                .map(|typed| definition_hash(&typed));
            sqlx::query(
                "UPDATE pulse_saved_charts SET definition = $1, definition_hash = COALESCE($2, definition_hash) WHERE id = $3",
            )
            .bind(&definition)
            .bind(hash)
            .bind(chart_id)
            .execute(pool)
            .await?;
            println!("chart {chart_id} remapped to {}", new.id);
        }
        sqlx::query("DELETE FROM pulse_cache WHERE user_id = $1")
            .bind(user)
            .execute(pool)
            .await?;
    }
    Ok(())
}
