use serde_json::Value;

#[derive(Debug, PartialEq, Eq)]
pub enum PasswordDerivationOutcome {
    CandidatePasswords(Vec<String>),
    MissingFact(&'static str),
    NoProviderRule,
}

pub fn derive_provider_password(
    provider_hint: Option<&str>,
    profile_facts: &Value,
) -> PasswordDerivationOutcome {
    let hint = provider_hint.unwrap_or("").to_lowercase();

    let name = profile_facts
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| profile_facts.get("full_name").and_then(Value::as_str));

    let dob = profile_facts
        .get("dob")
        .and_then(Value::as_str)
        .or_else(|| profile_facts.get("date_of_birth").and_then(Value::as_str));

    let bank_phone = profile_facts.get("bank_phone").and_then(Value::as_str);

    if hint.contains("hdfc") {
        let Some(d) = dob else {
            return PasswordDerivationOutcome::MissingFact("dob");
        };
        let Some(n) = name else {
            return PasswordDerivationOutcome::MissingFact("name");
        };
        let ddmm = extract_ddmm(d);
        let Some(ddmm_str) = ddmm else {
            return PasswordDerivationOutcome::MissingFact("dob");
        };
        let name_clean: String = n.chars().filter(|c| c.is_alphabetic()).collect();
        if name_clean.len() < 4 {
            return PasswordDerivationOutcome::MissingFact("name");
        }
        let prefix_lower = name_clean
            .chars()
            .take(4)
            .collect::<String>()
            .to_lowercase();
        let prefix_upper = name_clean
            .chars()
            .take(4)
            .collect::<String>()
            .to_uppercase();
        return PasswordDerivationOutcome::CandidatePasswords(vec![
            format!("{prefix_lower}{ddmm_str}"),
            format!("{prefix_upper}{ddmm_str}"),
        ]);
    }

    if hint.contains("icici") {
        let Some(d) = dob else {
            return PasswordDerivationOutcome::MissingFact("dob");
        };
        let Some(n) = name else {
            return PasswordDerivationOutcome::MissingFact("name");
        };
        let ddmm = extract_ddmm(d);
        let Some(ddmm_str) = ddmm else {
            return PasswordDerivationOutcome::MissingFact("dob");
        };
        let name_clean: String = n.chars().filter(|c| c.is_alphabetic()).collect();
        if name_clean.len() < 4 {
            return PasswordDerivationOutcome::MissingFact("name");
        }
        let prefix_lower = name_clean
            .chars()
            .take(4)
            .collect::<String>()
            .to_lowercase();
        return PasswordDerivationOutcome::CandidatePasswords(vec![format!(
            "{prefix_lower}{ddmm_str}"
        )]);
    }

    if hint.contains("sbi") {
        let Some(d) = dob else {
            return PasswordDerivationOutcome::MissingFact("dob");
        };
        let ddmm = extract_ddmm(d);
        let Some(ddmm_str) = ddmm else {
            return PasswordDerivationOutcome::MissingFact("dob");
        };
        let yymmdd = extract_ddmmyy(d);
        let mut candidates = Vec::new();
        if let Some(dmy) = yymmdd {
            candidates.push(dmy);
        }
        if let Some(bp) = bank_phone {
            let digits: String = bp.chars().filter(|c| c.is_ascii_digit()).collect();
            if digits.len() >= 5 {
                let last5 = &digits[digits.len() - 5..];
                candidates.push(format!("{last5}{ddmm_str}"));
            }
        }
        if !candidates.is_empty() {
            return PasswordDerivationOutcome::CandidatePasswords(candidates);
        }
        return PasswordDerivationOutcome::MissingFact("bank_phone");
    }

    if hint.contains("axis") {
        let Some(bp) = bank_phone else {
            return PasswordDerivationOutcome::MissingFact("bank_phone");
        };
        let Some(n) = name else {
            return PasswordDerivationOutcome::MissingFact("name");
        };
        let name_clean: String = n.chars().filter(|c| c.is_alphabetic()).collect();
        if name_clean.len() < 4 {
            return PasswordDerivationOutcome::MissingFact("name");
        }
        let digits: String = bp.chars().filter(|c| c.is_ascii_digit()).collect();
        if digits.len() < 4 {
            return PasswordDerivationOutcome::MissingFact("bank_phone");
        }
        let prefix = name_clean
            .chars()
            .take(4)
            .collect::<String>()
            .to_uppercase();
        let phone_last4 = &digits[digits.len() - 4..];
        return PasswordDerivationOutcome::CandidatePasswords(vec![format!(
            "{prefix}{phone_last4}"
        )]);
    }

    if hint.contains("kotak") {
        let Some(d) = dob else {
            return PasswordDerivationOutcome::MissingFact("dob");
        };
        let Some(n) = name else {
            return PasswordDerivationOutcome::MissingFact("name");
        };
        let ddmm = extract_ddmm(d);
        let Some(ddmm_str) = ddmm else {
            return PasswordDerivationOutcome::MissingFact("dob");
        };
        let name_clean: String = n.chars().filter(|c| c.is_alphabetic()).collect();
        if name_clean.len() < 4 {
            return PasswordDerivationOutcome::MissingFact("name");
        }
        let prefix_lower = name_clean
            .chars()
            .take(4)
            .collect::<String>()
            .to_lowercase();
        return PasswordDerivationOutcome::CandidatePasswords(vec![format!(
            "{prefix_lower}{ddmm_str}"
        )]);
    }

    PasswordDerivationOutcome::NoProviderRule
}

fn extract_ddmm(dob: &str) -> Option<String> {
    let clean: String = dob
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '-' || *c == '/')
        .collect();
    let parts: Vec<&str> = clean.split(['-', '/']).collect();
    if parts.len() == 3 {
        if parts[0].len() == 4 {
            let mm = parts[1];
            let dd = parts[2];
            return Some(format!("{dd:0>2}{mm:0>2}"));
        } else if parts[2].len() == 4 {
            let dd = parts[0];
            let mm = parts[1];
            return Some(format!("{dd:0>2}{mm:0>2}"));
        }
    }
    None
}

fn extract_ddmmyy(dob: &str) -> Option<String> {
    let clean: String = dob
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '-' || *c == '/')
        .collect();
    let parts: Vec<&str> = clean.split(['-', '/']).collect();
    if parts.len() == 3 {
        if parts[0].len() == 4 {
            let yy = &parts[0][2..];
            let mm = parts[1];
            let dd = parts[2];
            return Some(format!("{dd:0>2}{mm:0>2}{yy}"));
        } else if parts[2].len() == 4 {
            let dd = parts[0];
            let mm = parts[1];
            let yy = &parts[2][2..];
            return Some(format!("{dd:0>2}{mm:0>2}{yy}"));
        }
    }
    None
}
