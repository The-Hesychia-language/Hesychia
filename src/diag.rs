//! Compiler diagnostics: errors and warnings tied to a source position.

/// A position in a source file; line and column are 1-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub file: usize,
    pub line: u32,
    pub col: u32,
}

impl Span {
    pub fn new(file: usize, line: u32, col: u32) -> Self {
        Self { file, line, col }
    }
}

/// Every loaded source file, indexed by `Span::file`.
#[derive(Default)]
pub struct SourceMap {
    files: Vec<(String, String)>,
}

impl SourceMap {
    pub fn add(&mut self, path: String, source: String) -> usize {
        self.files.push((path, source));
        self.files.len() - 1
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn path(&self, file: usize) -> &str {
        &self.files[file].0
    }

    pub fn source(&self, file: usize) -> &str {
        &self.files[file].1
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    /// Shown on request: a copy `managed` inserted, with `--warn-copies`.
    Note,
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    pub span: Span,
    pub message: String,
    pub hint: Option<String>,
}

pub type Result<T> = std::result::Result<T, Diagnostic>;

impl Diagnostic {
    pub fn error(span: Span, message: impl Into<String>) -> Self {
        Self::new(Severity::Error, span, message)
    }

    pub fn warning(span: Span, message: impl Into<String>) -> Self {
        Self::new(Severity::Warning, span, message)
    }

    pub fn note(span: Span, message: impl Into<String>) -> Self {
        Self::new(Severity::Note, span, message)
    }

    fn new(severity: Severity, span: Span, message: impl Into<String>) -> Self {
        Self { severity, span, message: message.into(), hint: None }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Formats the diagnostic with the offending source line and a caret.
    pub fn render(&self, sources: &SourceMap) -> String {
        let (path, source) = (sources.path(self.span.file), sources.source(self.span.file));
        let label = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Note => "note",
        };
        let line_text = source.lines().nth(self.span.line as usize - 1).unwrap_or("");
        let number = self.span.line.to_string();
        let gutter = " ".repeat(number.len());
        let caret = " ".repeat(self.span.col.saturating_sub(1) as usize);

        let mut out = format!("{label}: {}\n", self.message);
        out += &format!("{gutter}--> {path}:{}:{}\n", self.span.line, self.span.col);
        out += &format!("{gutter} |\n{number} | {line_text}\n{gutter} | {caret}^\n");
        if let Some(hint) = &self.hint {
            out += &format!("{gutter} = hint: {hint}\n");
        }
        out
    }
}
