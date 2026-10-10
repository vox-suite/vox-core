use chrono::{DateTime, Utc};
use lopdf::Document;
use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum PdfError {
    #[error("invalid PDF format")]
    InvalidPdfFormat,
    #[error("password required to open encrypted PDF")]
    PasswordRequired,
    #[error("incorrect password provided")]
    WrongPassword,
    #[error("unsupported PDF encryption or structure: {0}")]
    UnsupportedEncryption(String),
    #[error("document extraction failed: {0}")]
    ExtractionFailed(String),
    #[error("document contains no recognizable financial facts")]
    NoFactsFound,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedFinancialDocument {
    pub kind: String,
    pub title: String,
    pub summary: String,
    pub occurred_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub amount: Option<f64>,
    pub currency: Option<String>,
    pub reference: Option<String>,
    pub biller_or_merchant: Option<String>,
    pub account_hint: Option<String>,
    pub facts: serde_json::Value,
}

pub fn parse_pdf(
    bytes: &[u8],
    password: Option<&str>,
) -> Result<ExtractedFinancialDocument, PdfError> {
    if bytes.len() < 32 || bytes.len() > 50_000_000 {
        return Err(PdfError::InvalidPdfFormat);
    }

    if !bytes.starts_with(b"%PDF-")
        && !bytes[..1024.min(bytes.len())]
            .windows(5)
            .any(|w| w == b"%PDF-")
    {
        return Err(PdfError::InvalidPdfFormat);
    }

    let mut doc = Document::load_mem(bytes).map_err(|_e| PdfError::InvalidPdfFormat)?;

    if doc.is_encrypted() {
        let pwd = password.unwrap_or("");
        if pwd.is_empty() {
            return Err(PdfError::PasswordRequired);
        }

        match doc.decrypt(pwd) {
            Ok(()) => {}
            Err(e) => {
                let err_str = e.to_string().to_lowercase();
                if err_str.contains("password") || err_str.contains("authenticated") {
                    return Err(PdfError::WrongPassword);
                } else if err_str.contains("unsupported") || err_str.contains("algorithm") {
                    return Err(PdfError::UnsupportedEncryption(e.to_string()));
                } else {
                    return Err(PdfError::ExtractionFailed(e.to_string()));
                }
            }
        }
    }

    let pages: Vec<u32> = doc.get_pages().keys().copied().collect();
    if pages.len() > 500 {
        return Err(PdfError::ExtractionFailed(
            "PDF exceeds the 500-page extraction limit".into(),
        ));
    }
    if pages.is_empty() {
        return Err(PdfError::ExtractionFailed("No pages found in PDF".into()));
    }

    let text = doc
        .extract_text_with_limit(&pages, 50_000_000)
        .map_err(|e| PdfError::ExtractionFailed(e.to_string()))?;

    if text.trim().is_empty() {
        return Err(PdfError::ExtractionFailed(
            "PDF contains no extractable text (raster or image only)".into(),
        ));
    }

    extract_document_facts(&text, password)
}

pub fn extract_document_facts(
    raw_text: &str,
    _password: Option<&str>,
) -> Result<ExtractedFinancialDocument, PdfError> {
    let lower = raw_text.to_lowercase();
    if [
        "one time password",
        "one-time password",
        "verification code",
        "payment failed",
        "transaction declined",
        "payment declined",
        "authorisation request",
        "authorization request",
        "payment attempt",
    ]
    .iter()
    .any(|phrase| lower.contains(phrase))
    {
        return Err(PdfError::NoFactsFound);
    }
    let amount_regex = Regex::new(r"(?i)(?:total\s+amount\s+due|total\s+amount|amount\s+paid|amount\s+due|total\s+due|total)\s*(?:is|:|of)?\s*(?:(₹|inr|us\$|usd|€|eur|£|gbp|rs\.?|\$)\s*)?([0-9]+(?:,[0-9]{3})*(?:\.[0-9]{2})?)").unwrap();
    let invoice_regex = Regex::new(r"(?i)(?:invoice|bill|ref|reference|order)\s*(?:no\.?|num\.?|#|id|number)\s*[:#]?\s*([A-Z0-9_-]{4,24})").unwrap();

    let mut amount = None;
    let mut amount_currency = None;
    if let Some(caps) = amount_regex.captures(raw_text) {
        amount_currency = caps.get(1).map(|currency| currency.as_str().to_lowercase());
        if let Some(m) = caps.get(2) {
            let s = m.as_str().replace(',', "");
            if let Ok(val) = s.parse::<f64>()
                && val > 0.0
            {
                amount = Some(val);
            }
        }
    }

    let labelled_date = Regex::new(r"(?i)(?:statement date|bill date|invoice date|transaction date|payment date|date)\s*:?\s*(\d{4}-\d{2}-\d{2}|\d{2}[/-]\d{2}[/-]\d{4}|\d{1,2} [A-Za-z]+ \d{4}|[A-Za-z]+ \d{1,2},? \d{4})").unwrap();
    let occurred_at = labelled_date
        .captures(raw_text)
        .and_then(|c| c.get(1))
        .and_then(|m| parse_date(m.as_str()))
        .ok_or(PdfError::NoFactsFound)?;

    let reference = invoice_regex
        .captures(raw_text)
        .and_then(|caps| caps.get(1).map(|m| m.as_str().to_string()));

    let is_statement = lower.contains("statement") || lower.contains("account summary");
    let is_bill =
        lower.contains("due date") || lower.contains("bill date") || lower.contains("pay before");
    let is_receipt = lower.contains("receipt")
        || lower.contains("paid successfully")
        || lower.contains("payment confirmation");

    if (!is_statement && !is_bill && !is_receipt) || amount.is_none() {
        return Err(PdfError::NoFactsFound);
    }

    let kind = if is_statement {
        "statement".to_string()
    } else if is_bill {
        "bill".to_string()
    } else {
        "transaction".to_string()
    };

    let biller_or_merchant = detect_institution(&lower);
    let title = format!(
        "{} {}",
        biller_or_merchant.as_deref().unwrap_or("Financial"),
        kind
    );
    let summary = format!(
        "{} of {}{}",
        title,
        amount
            .map(|a| format!("{:.2}", a))
            .unwrap_or_else(|| "unspecified amount".into()),
        reference
            .as_ref()
            .map(|r| format!(" (Ref: {})", r))
            .unwrap_or_default()
    );

    let currency = match amount_currency.as_deref() {
        Some("₹" | "inr") => Some("INR".into()),
        Some("usd" | "us$") => Some("USD".into()),
        Some("eur" | "€") => Some("EUR".into()),
        Some("gbp" | "£") => Some("GBP".into()),
        Some("rs" | "rs.")
            if biller_or_merchant.as_deref().is_some_and(|institution| {
                [
                    "HDFC Bank",
                    "ICICI Bank",
                    "State Bank of India",
                    "SBI Card",
                    "Axis Bank",
                    "Kotak Mahindra",
                    "Airtel",
                    "Jio",
                    "BESCOM",
                ]
                .contains(&institution)
            }) =>
        {
            Some("INR".into())
        }
        _ => None,
    };

    let facts = serde_json::json!({
        "direction": if is_statement { "statement" } else if is_bill { "due" } else { "debit" },
        "is_spending": !is_statement && !is_bill && is_receipt,
        "amount": amount,
        "currency": currency,
        "reference": reference,
        "merchant": biller_or_merchant,
        "account_hint": null,
        "document_kind": kind,
        "is_statement": is_statement,
        "occurred_at": occurred_at.to_rfc3339(),
    });

    Ok(ExtractedFinancialDocument {
        kind,
        title,
        summary,
        occurred_at,
        ended_at: None,
        amount,
        currency,
        reference,
        biller_or_merchant,
        account_hint: None,
        facts,
    })
}

fn detect_institution(lower: &str) -> Option<String> {
    for name in [
        "HDFC Bank",
        "ICICI Bank",
        "State Bank of India",
        "SBI Card",
        "Axis Bank",
        "Kotak Mahindra",
        "Airtel",
        "Jio",
        "BESCOM",
        "Amazon",
        "Flipkart",
        "Swiggy",
        "Zomato",
    ] {
        if lower.contains(&name.to_lowercase()) {
            return Some(name.to_string());
        }
    }
    None
}

fn parse_date(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    for format in [
        "%Y-%m-%d",
        "%d/%m/%Y",
        "%d-%m-%Y",
        "%d %b %Y",
        "%d %B %Y",
        "%b %d %Y",
        "%B %d %Y",
        "%b %d, %Y",
        "%B %d, %Y",
    ] {
        if let Ok(date) = chrono::NaiveDate::parse_from_str(s, format) {
            return date
                .and_hms_opt(0, 0, 0)
                .map(|dt| DateTime::from_naive_utc_and_offset(dt, Utc));
        }
    }
    None
}
