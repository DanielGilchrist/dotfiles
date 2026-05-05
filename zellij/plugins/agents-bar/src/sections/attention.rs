use crate::{Bar, Cell};

/// `⚠ N` — bold yellow attention badge for held/exited agents.
pub fn render(bar: &Bar) -> Option<Cell> {
    let count = bar.attention_count();
    if count == 0 { return None; }
    let visible = format!("⚠ {}", count);
    let content = format!("\u{1b}[33;1m{}\u{1b}[0m", visible);
    Some(Cell { width: visible.chars().count(), content })
}
