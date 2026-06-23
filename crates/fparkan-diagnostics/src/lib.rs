#![forbid(unsafe_code)]
//! Structured diagnostics shared by `FParkan` crates.

use serde::Serialize;

/// Diagnostic severity.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Informational note.
    Info,
    /// Recoverable warning.
    Warning,
    /// Error for the current operation.
    Error,
    /// Fatal error for the current run.
    Fatal,
}

/// Evidence level for a contract or interpretation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EvidenceStatus {
    /// Described by project documentation.
    Documented,
    /// Verified by synthetic fixtures.
    SyntheticVerified,
    /// Verified against the licensed corpus.
    CorpusVerified,
    /// Verified by runtime capture.
    RuntimeCaptured,
    /// Working hypothesis; not a runtime contract.
    Hypothesis,
}

/// Operation phase where a diagnostic was produced.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// Discovery.
    Discover,
    /// Read.
    Read,
    /// Parse.
    Parse,
    /// Validate.
    Validate,
    /// Resolve.
    Resolve,
    /// Prepare.
    Prepare,
    /// Construct.
    Construct,
    /// Register.
    Register,
    /// Simulate.
    Simulate,
    /// Render.
    Render,
}

/// Byte span in an input source.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct SourceSpan {
    /// Start offset.
    pub offset: u64,
    /// Length in bytes.
    pub length: u64,
}

/// Stable diagnostic code.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
pub struct DiagnosticCode(pub &'static str);

/// Context attached to a diagnostic.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct DiagnosticContext {
    /// Phase.
    pub phase: Option<Phase>,
    /// Redacted or logical path.
    pub path: Option<String>,
    /// Archive entry name.
    pub archive_entry: Option<String>,
    /// Object/prototype key.
    pub object_key: Option<String>,
    /// Input span.
    pub span: Option<SourceSpan>,
}

/// Structured diagnostic with cause chain.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Diagnostic {
    /// Stable code.
    pub code: DiagnosticCode,
    /// Severity.
    pub severity: Severity,
    /// Human message.
    pub message: String,
    /// Context.
    pub context: DiagnosticContext,
    /// Causes.
    pub causes: Vec<Diagnostic>,
}

/// Creates a diagnostic with default error severity.
#[must_use]
pub fn diagnostic(code: DiagnosticCode, message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        code,
        severity: Severity::Error,
        message: message.into(),
        context: DiagnosticContext::default(),
        causes: Vec::new(),
    }
}

impl Diagnostic {
    /// Returns a copy with severity changed.
    #[must_use]
    pub fn with_severity(mut self, severity: Severity) -> Self {
        self.severity = severity;
        self
    }

    /// Returns a copy with context changed.
    #[must_use]
    pub fn with_context(mut self, context: DiagnosticContext) -> Self {
        self.context = context;
        self
    }

    /// Adds a cause.
    pub fn push_cause(&mut self, cause: Diagnostic) {
        self.causes.push(cause);
    }
}

/// Renders a compact human-readable diagnostic.
#[must_use]
pub fn render_human(diagnostic: &Diagnostic) -> String {
    let mut out = format!(
        "{:?} {}: {}",
        diagnostic.severity, diagnostic.code.0, diagnostic.message
    );
    if let Some(path) = &diagnostic.context.path {
        out.push_str(" [");
        out.push_str(path);
        out.push(']');
    }
    out
}

/// Renders deterministic JSON using the typed diagnostic schema.
#[must_use]
pub fn render_json(diagnostic: &Diagnostic) -> String {
    match serde_json::to_string(diagnostic) {
        Ok(json) => json,
        Err(err) => format!("{{\"error\":\"diagnostic serialization failed: {err}\"}}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_is_stable() {
        let d = diagnostic(DiagnosticCode("S0-DIAG-001"), "keeps context").with_context(
            DiagnosticContext {
                phase: Some(Phase::Parse),
                ..DiagnosticContext::default()
            },
        );
        assert_eq!(
            render_json(&d),
            "{\"code\":\"S0-DIAG-001\",\"severity\":\"error\",\"message\":\"keeps context\",\"context\":{\"phase\":\"parse\"},\"causes\":[]}"
        );
    }

    #[test]
    fn diagnostic_chain_preserves_context() {
        let mut root = diagnostic(DiagnosticCode("ROOT"), "root").with_context(DiagnosticContext {
            phase: Some(Phase::Resolve),
            path: Some("archives/material.lib".to_string()),
            archive_entry: Some("MATERIAL.MAT0".to_string()),
            object_key: Some("unit/tank".to_string()),
            span: Some(SourceSpan {
                offset: 12,
                length: 4,
            }),
        });
        root.push_cause(diagnostic(DiagnosticCode("CAUSE"), "cause").with_context(
            DiagnosticContext {
                phase: Some(Phase::Parse),
                path: Some("archives/material.lib".to_string()),
                span: Some(SourceSpan {
                    offset: 16,
                    length: 8,
                }),
                ..DiagnosticContext::default()
            },
        ));

        let json = render_json(&root);

        assert!(json.contains("\"code\":\"ROOT\""));
        assert!(json.contains("\"phase\":\"resolve\""));
        assert!(json.contains("\"path\":\"archives/material.lib\""));
        assert!(json.contains("\"archive_entry\":\"MATERIAL.MAT0\""));
        assert!(json.contains("\"object_key\":\"unit/tank\""));
        assert!(json.contains("\"span\":{\"offset\":12,\"length\":4}"));
        assert!(json.contains("\"code\":\"CAUSE\""));
        assert!(json.contains("\"span\":{\"offset\":16,\"length\":8}"));
    }

    #[test]
    fn json_escapes_all_control_characters() {
        let value = diagnostic(DiagnosticCode("S1-H01"), "quote\"\u{0000}tab\tline\r\n");
        let json = render_json(&value);
        assert!(json.contains("\\u0000"));
        assert!(json.contains("\\u0009"));
        assert!(!json.contains('\t'));
        assert!(!json.contains('\r'));
    }
}
