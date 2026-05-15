use crate::{theme, Bar, Cell};

/// `✦ N agents` — accent star, bold count, plain label.
pub fn render(bar: &Bar) -> Option<Cell> {
    let count = bar.agent_count();
    if count == 0 { return None; }
    let plural = if count == 1 { "" } else { "s" };
    let count_str = count.to_string();
    let visible = format!("✦ {} agent{}", count_str, plural);
    let content = format!(
        "{} {} agent{}",
        theme::accented(bar.accent, "✦"),
        theme::bold(&count_str),
        plural,
    );
    Some(Cell { width: visible.chars().count(), content })
}
