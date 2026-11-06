//! Findings in the shared `rf.finding/1` format, and their text rendering.

use serde::Serialize;
use serde_json::Value;

pub const SCHEMA: &str = "rf.finding/1";
pub const TOOL: &str = "papers";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warn,
    Error,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warn => "warn",
            Severity::Error => "error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Evidence {
    pub path: String,
    pub locator: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Finding {
    pub schema: &'static str,
    pub tool: &'static str,
    pub rule: &'static str,
    pub severity: Severity,
    pub subject: String,
    pub message: String,
    pub evidence: Vec<Evidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    pub at: String,
}

impl Finding {
    pub fn new(rule: &'static str, severity: Severity, subject: String, message: String) -> Self {
        Finding {
            schema: SCHEMA,
            tool: TOOL,
            rule,
            severity,
            subject,
            message,
            evidence: Vec::new(),
            data: None,
            at: now(),
        }
    }

    pub fn evidence(mut self, path: &str, locator: String) -> Self {
        self.evidence.push(Evidence {
            path: path.to_owned(),
            locator,
        });
        self
    }

    pub fn data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

/// Current UTC time, RFC 3339 to the second.
pub fn now() -> String {
    jiff::Timestamp::now()
        .round(jiff::Unit::Second)
        .map(|t| t.to_string())
        .unwrap_or_else(|_| jiff::Timestamp::now().to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Format {
    Text,
    Jsonl,
}

/// Prints findings to stdout and returns whether none is an error.
pub fn emit(findings: &[Finding], format: Format) -> bool {
    for f in findings {
        match format {
            Format::Jsonl => println!(
                "{}",
                serde_json::to_string(f).expect("findings serialize to JSON")
            ),
            Format::Text => {
                let evidence = f
                    .evidence
                    .iter()
                    .map(|e| format!("{}: {}", e.path, e.locator))
                    .collect::<Vec<_>>()
                    .join("; ");
                let evidence = if evidence.is_empty() {
                    String::new()
                } else {
                    format!(" [{evidence}]")
                };
                println!(
                    "{:<5} {} {}: {}{evidence}",
                    f.severity.label(),
                    f.rule,
                    f.subject,
                    f.message
                );
            }
        }
    }
    findings.iter().all(|f| f.severity != Severity::Error)
}

/// `rule: N (severity)` counts, for a text summary on stderr.
pub fn summary(findings: &[Finding]) -> String {
    let mut counts: std::collections::BTreeMap<(&str, Severity), usize> = Default::default();
    for f in findings {
        *counts.entry((f.rule, f.severity)).or_default() += 1;
    }
    counts
        .iter()
        .map(|((rule, sev), n)| format!("{rule} {n} ({})", sev.label()))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_the_shared_format() {
        let mut f = Finding::new(
            "duplicate-doi",
            Severity::Warn,
            "12".into(),
            "two keys share a DOI".into(),
        )
        .evidence("docs/references.md", "line 40".into())
        .data(serde_json::json!({"keys": ["12", "30"]}));
        f.at = "2026-11-07T03:00:00Z".into();
        assert_eq!(
            serde_json::to_string(&f).unwrap(),
            r#"{"schema":"rf.finding/1","tool":"papers","rule":"duplicate-doi","severity":"warn","subject":"12","message":"two keys share a DOI","evidence":[{"path":"docs/references.md","locator":"line 40"}],"data":{"keys":["12","30"]},"at":"2026-11-07T03:00:00Z"}"#
        );
        assert!(now().ends_with('Z') && now().len() == 20, "{}", now());
    }
}
