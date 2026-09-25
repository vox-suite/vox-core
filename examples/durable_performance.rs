//! Isolated, readout-only performance probe for the durable-task HTTP path.
//!
//! Run only against a disposable database named `voxcore_perf` and a loopback
//! Core API. This creates synthetic host credentials, users, tasks, and jobs.

use chrono::Utc;
use reqwest::{Client, Response};
use serde_json::{Value, json};
use std::{env, time::Instant};
use uuid::Uuid;
use vox_core::{
    db::{Db, jobs::JobRepository},
    durable_tasks::{DurableTaskService, StartTaskRequest},
    host_trust::{HostAppCredential, HostContextRequest, HostTrustService, RegisterHostAppRequest},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let database_url = env::var("DATABASE_URL")?;
    let api_url = env::var("VOX_CORE_URL").unwrap_or_else(|_| "http://127.0.0.1:3001".into());
    let parsed_db = url::Url::parse(&database_url)?;
    let parsed_api = url::Url::parse(&api_url)?;
    if parsed_db.path() != "/voxcore_perf"
        || !matches!(parsed_db.host_str(), Some("127.0.0.1" | "localhost"))
        || !matches!(parsed_api.host_str(), Some("127.0.0.1" | "localhost"))
    {
        return Err("benchmark requires loopback API and database voxcore_perf".into());
    }
    let samples: usize = env::var("PERF_SAMPLES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000);
    let db = Db::connect(&database_url).await?;
    let read_only = env::var("PERF_READ_ONLY").as_deref() == Ok("1");
    if env::var("PERF_POOL_PROBE").as_deref() == Ok("1") {
        let mut held = Vec::with_capacity(10);
        for _ in 0..10 {
            held.push(db.pool().acquire().await?);
        }
        let started = Instant::now();
        let exhausted = db.pool().acquire().await;
        println!(
            "pool_held=10 acquire_11_result={} elapsed_ms={:.2}",
            if exhausted.is_ok() { "ok" } else { "error" },
            started.elapsed().as_secs_f64() * 1000.0
        );
        drop(held);
        if env::var("PERF_POOL_ONLY").as_deref() == Ok("1") {
            return Ok(());
        }
    }
    let trust = HostTrustService::new(db.clone());
    let host = trust
        .register_host_app(RegisterHostAppRequest {
            deployment_external_key: format!("perf-{}", Uuid::new_v4()),
            host_app_external_key: "benchmark".into(),
            allowed_origins: vec![],
        })
        .await?;
    let context_request = HostContextRequest {
        host_user_id: "synthetic-user".into(),
        organization_external_key: None,
    };
    let resolved = trust
        .resolve_authenticated_context(
            &host
                .credential
                .sign_context_request(&context_request, Utc::now(), Uuid::new_v4())?,
            &context_request,
            None,
            Utc::now(),
        )
        .await?;
    let tasks = DurableTaskService::new(db.clone());
    if !read_only {
        let created = tasks
            .start(
                &resolved,
                StartTaskRequest {
                    title: "Synthetic benchmark task".into(),
                    instruction: "No external action; local latency probe".into(),
                    agent_external_key: None,
                },
            )
            .await?;
        let null_reference: bool =
            sqlx::query_scalar("SELECT payload_reference_id IS NULL FROM jobs WHERE id = $1")
                .bind(created.run_id)
                .fetch_one(db.pool())
                .await?;
        // Make this one synthetic job the next claim candidate even when a
        // previous benchmark run left a large disposable backlog behind.
        sqlx::query("UPDATE jobs SET available_at = '2000-01-01' WHERE id = $1")
            .bind(created.run_id)
            .execute(db.pool())
            .await?;
        let claim = tokio::spawn({
            let jobs = JobRepository::new(db.clone());
            async move {
                jobs.claim("perf-probe", Utc::now(), chrono::Duration::seconds(30), 1)
                    .await
            }
        })
        .await;
        println!(
            "handoff job_payload_reference_null={null_reference} claim_result={}",
            match claim {
                Ok(Ok(_)) => "ok",
                Ok(Err(_)) => "error",
                Err(_) => "panic",
            }
        );
        if env::var("PERF_HANDOFF_ONLY").as_deref() == Ok("1") {
            return Ok(());
        }
    }

    let client = Client::builder().pool_max_idle_per_host(100).build()?;
    let seed = signed_request(
        &client, &api_url, "/v1/durable-tasks", &host.credential, &context_request,
        json!({"host_context": context_request, "task": {
            "title": "Synthetic benchmark task", "instruction": "No external action; local latency probe"
        }}),
    ).await?;
    let seed_status = seed.status();
    let seed_json: Value = seed.json().await?;
    if !seed_status.is_success() {
        return Err(format!("seed task failed: HTTP {seed_status}").into());
    }
    let task_id = seed_json["id"]
        .as_str()
        .ok_or("seed task had no id")?
        .to_owned();
    println!("samples_per_concurrency={samples}");
    let stage_samples = if read_only { 0 } else { samples.min(1000) };
    let mut auth_times = Vec::with_capacity(stage_samples);
    let mut create_times = Vec::with_capacity(stage_samples);
    let mut read_times = Vec::with_capacity(stage_samples);
    for _ in 0..stage_samples {
        let assertion =
            host.credential
                .sign_context_request(&context_request, Utc::now(), Uuid::new_v4())?;
        let started = Instant::now();
        let actor = trust
            .resolve_authenticated_context(&assertion, &context_request, None, Utc::now())
            .await?;
        auth_times.push(started.elapsed().as_secs_f64() * 1000.0);
        let started = Instant::now();
        let task = tasks
            .start(
                &actor,
                StartTaskRequest {
                    title: "Synthetic benchmark task".into(),
                    instruction: "No external action; local latency probe".into(),
                    agent_external_key: None,
                },
            )
            .await?;
        create_times.push(started.elapsed().as_secs_f64() * 1000.0);
        let started = Instant::now();
        let _ = tasks.get(&actor, task.id).await?;
        read_times.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    if !read_only {
        print_stats("stage_auth", 1, &mut auth_times, 0);
        print_stats("stage_create", 1, &mut create_times, 0);
        print_stats("stage_get", 1, &mut read_times, 0);
    }
    let endpoints: &[&str] = if read_only {
        &["get", "status"]
    } else {
        &["get", "status", "submit"]
    };
    for concurrency in [1, 10, 50, 100] {
        for &endpoint in endpoints {
            let queue_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs")
                .fetch_one(db.pool())
                .await?;
            let phase_started = Instant::now();
            let mut times = Vec::with_capacity(samples);
            let mut errors = 0;
            for batch_start in (0..samples).step_by(concurrency) {
                let mut joins = tokio::task::JoinSet::new();
                for _ in batch_start..(batch_start + concurrency).min(samples) {
                    let client = client.clone();
                    let api_url = api_url.clone();
                    let credential = host.credential.clone();
                    let context = context_request.clone();
                    let task_id = task_id.clone();
                    joins.spawn(async move {
                        let (path, body) = match endpoint {
                            "get" => (format!("/v1/durable-tasks/{task_id}"), json!({"host_context": context})),
                            "status" => ("/v1/status-events".to_owned(), json!({"host_context": context, "after": 0, "limit": 1})),
                            _ => ("/v1/durable-tasks".to_owned(), json!({"host_context": context, "task": {
                                "title": "Synthetic benchmark task", "instruction": "No external action; local latency probe"
                            }})),
                        };
                        let started = Instant::now();
                        let response = signed_request(&client, &api_url, &path, &credential, &context, body).await;
                        match response {
                            Ok(response) if response.status().is_success() => {
                                let read = response.bytes().await;
                                (started.elapsed().as_secs_f64() * 1000.0, read.is_ok())
                            }
                            _ => (started.elapsed().as_secs_f64() * 1000.0, false),
                        }
                    });
                }
                while let Some(result) = joins.join_next().await {
                    let (elapsed, ok) = result?;
                    if ok {
                        times.push(elapsed);
                    } else {
                        errors += 1;
                    }
                }
            }
            print_stats(endpoint, concurrency, &mut times, errors);
            println!(
                "phase={endpoint} concurrency={concurrency} queue_rows_before={queue_rows} wall_ms={:.2}",
                phase_started.elapsed().as_secs_f64() * 1000.0
            );
        }
    }
    Ok(())
}

async fn signed_request(
    client: &Client,
    api_url: &str,
    path: &str,
    credential: &HostAppCredential,
    context: &HostContextRequest,
    body: Value,
) -> Result<Response, Box<dyn std::error::Error + Send + Sync>> {
    let assertion = credential.sign_context_request(context, Utc::now(), Uuid::new_v4())?;
    Ok(client
        .post(format!("{api_url}{path}"))
        .header(
            "x-vox-host-credential",
            assertion.credential_id().to_string(),
        )
        .header("x-vox-host-secret", assertion.secret())
        .header("x-vox-host-audience", assertion.audience())
        .header(
            "x-vox-host-timestamp",
            assertion.issued_at().timestamp().to_string(),
        )
        .header("x-vox-host-nonce", assertion.nonce().to_string())
        .header("x-vox-host-signature", assertion.signature())
        .json(&body)
        .send()
        .await?)
}

fn print_stats(endpoint: &str, concurrency: usize, samples: &mut [f64], errors: usize) {
    samples.sort_by(f64::total_cmp);
    let percentile = |p: f64| -> f64 {
        if samples.is_empty() {
            return f64::NAN;
        }
        let index = ((samples.len() - 1) as f64 * p).ceil() as usize;
        samples[index]
    };
    let p999 = if samples.len() >= 10_000 {
        format!("{:.2}", percentile(0.999))
    } else {
        "insufficient_n".into()
    };
    println!(
        "endpoint={endpoint} concurrency={concurrency} ok={} errors={errors} p50_ms={:.2} p75_ms={:.2} p90_ms={:.2} p95_ms={:.2} p99_ms={:.2} p99_9_ms={p999} max_ms={:.2}",
        samples.len(),
        percentile(0.50),
        percentile(0.75),
        percentile(0.90),
        percentile(0.95),
        percentile(0.99),
        percentile(1.0)
    );
}
