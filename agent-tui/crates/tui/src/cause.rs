/// Why a request failed, as the user is told: the error and its sources,
/// outermost first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cause(String);

impl Cause {
    pub fn of(e: &(dyn std::error::Error + 'static)) -> Self {
        Self(describe(e))
    }

    pub fn message(text: String) -> Self {
        Self(text)
    }

    pub fn first_line(&self) -> String {
        crate::log::first_printable_line(&self.0).unwrap_or_default()
    }
}

impl std::fmt::Display for Cause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn describe(e: &(dyn std::error::Error + 'static)) -> String {
    let mut out = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        let text = s.to_string();
        if !out.contains(&text) {
            out.push_str(": ");
            out.push_str(&text);
        }
        source = s.source();
    }
    out
}
