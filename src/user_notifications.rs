use async_trait::async_trait;
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor, message::header::ContentType,
};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

const MAX_PER_HOUR: i64 = 3;
const MAX_PER_DAY: i64 = 10;
const MAX_SUBJECT_CHARS: usize = 120;
const MAX_BODY_CHARS: usize = 1000;

#[derive(Debug, thiserror::Error)]
pub enum NotifyError {
    #[error("notification storage unavailable")]
    Database(#[from] sqlx::Error),
    #[error("user has no verified email address")]
    NoVerifiedEmail,
    #[error("notification limit reached")]
    RateLimited,
    #[error("email delivery failed")]
    Delivery,
    #[error("email is not configured")]
    NotConfigured,
}

#[derive(Debug, Eq, PartialEq)]
pub enum NotifyOutcome {
    Sent,
    Duplicate,
}

#[async_trait]
pub trait EmailSender: Send + Sync {
    async fn send(&self, to: &str, subject: &str, body: &str) -> Result<(), NotifyError>;
}

pub struct SmtpEmailSender {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: String,
}

impl SmtpEmailSender {
    pub fn new(smtp_url: &str, from: &str) -> Result<Self, NotifyError> {
        from.parse::<lettre::message::Mailbox>()
            .map_err(|_| NotifyError::NotConfigured)?;
        let transport = AsyncSmtpTransport::<Tokio1Executor>::from_url(smtp_url)
            .map_err(|_| NotifyError::NotConfigured)?
            .build();
        Ok(Self {
            transport,
            from: from.to_string(),
        })
    }
}

#[async_trait]
impl EmailSender for SmtpEmailSender {
    async fn send(&self, to: &str, subject: &str, body: &str) -> Result<(), NotifyError> {
        let message = Message::builder()
            .from(self.from.parse().map_err(|_| NotifyError::NotConfigured)?)
            .to(to.parse().map_err(|_| NotifyError::Delivery)?)
            .subject(subject)
            .header(ContentType::TEXT_PLAIN)
            .body(body.to_string())
            .map_err(|_| NotifyError::Delivery)?;
        self.transport
            .send(message)
            .await
            .map_err(|_| NotifyError::Delivery)?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct UserNotifier {
    pool: PgPool,
    sender: Arc<dyn EmailSender>,
}

impl UserNotifier {
    pub fn new(pool: PgPool, sender: Arc<dyn EmailSender>) -> Self {
        Self { pool, sender }
    }

    pub async fn notify(
        &self,
        user_id: Uuid,
        idempotency_key: &str,
        subject: &str,
        body: &str,
    ) -> Result<NotifyOutcome, NotifyError> {
        let subject = sanitize(subject, MAX_SUBJECT_CHARS);
        let body = sanitize(body, MAX_BODY_CHARS);
        if subject.is_empty() || body.is_empty() {
            return Err(NotifyError::Delivery);
        }

        let to: String = sqlx::query_scalar("SELECT verified_email FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?
            .flatten()
            .ok_or(NotifyError::NoVerifiedEmail)?;

        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("user_notifications:{user_id}"))
            .execute(&mut *tx)
            .await?;
        let (last_hour, last_day) = sqlx::query_as::<_, (i64, i64)>(
            "SELECT \
                COUNT(*) FILTER (WHERE created_at > now() - interval '1 hour'), \
                COUNT(*) \
             FROM user_notifications \
             WHERE user_id = $1 AND created_at > now() - interval '1 day'",
        )
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await?;
        if last_hour >= MAX_PER_HOUR || last_day >= MAX_PER_DAY {
            return Err(NotifyError::RateLimited);
        }
        let id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO user_notifications (user_id, channel, idempotency_key, subject) \
             VALUES ($1, 'email', $2, $3) \
             ON CONFLICT (user_id, idempotency_key) DO NOTHING RETURNING id",
        )
        .bind(user_id)
        .bind(idempotency_key)
        .bind(&subject)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        let Some(id) = id else {
            return Ok(NotifyOutcome::Duplicate);
        };

        match self.sender.send(&to, &subject, &body).await {
            Ok(()) => {
                sqlx::query(
                    "UPDATE user_notifications SET status = 'sent', sent_at = now() WHERE id = $1",
                )
                .bind(id)
                .execute(&self.pool)
                .await?;
                Ok(NotifyOutcome::Sent)
            }
            Err(error) => {
                sqlx::query(
                    "UPDATE user_notifications SET status = 'failed', error_code = $2 WHERE id = $1",
                )
                .bind(id)
                .bind(error.to_string())
                .execute(&self.pool)
                .await?;
                Err(error)
            }
        }
    }
}

fn is_link_start(word: &str) -> bool {
    let lower = word
        .trim_start_matches(['(', '<', '[', '"', '\''])
        .to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("www.")
        || lower.contains("://")
        || looks_like_domain(&lower)
}

fn looks_like_domain(word: &str) -> bool {
    let trimmed = word.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    let parts: Vec<&str> = trimmed.split('.').collect();
    parts.len() >= 2
        && parts.iter().all(|part| !part.is_empty())
        && parts
            .last()
            .is_some_and(|tld| tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic()))
        && parts[0].len() >= 2
}

fn numeric_token(word: &str) -> bool {
    word.chars().any(|c| c.is_ascii_digit())
        && word
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | '(' | ')' | '.'))
}

fn flush_numbers<'a>(run: &mut Vec<&'a str>, out: &mut Vec<&'a str>) {
    let digits: usize = run
        .iter()
        .map(|word| word.chars().filter(char::is_ascii_digit).count())
        .sum();
    if digits >= 8 {
        out.push("[number removed]");
    } else {
        out.append(run);
    }
    run.clear();
}

pub fn sanitize(text: &str, max_chars: usize) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut run: Vec<&str> = Vec::new();
    for word in text.split_whitespace() {
        if numeric_token(word) {
            run.push(word);
            continue;
        }
        flush_numbers(&mut run, &mut out);
        if is_link_start(word) {
            out.push("[link removed]");
        } else {
            out.push(word);
        }
    }
    flush_numbers(&mut run, &mut out);
    out.join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(max_chars)
        .collect()
}
