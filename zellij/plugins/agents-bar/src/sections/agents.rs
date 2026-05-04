use crate::{theme, Bar, Cell};

/// `✦ 7 agents` — accent star, bold count, plain label.
pub fn render(bar: &Bar) -> Option<Cell> {
    if bar.agent_count == 0 { return None; }
    let plural = if bar.agent_count == 1 { "" } else { "s" };
    let count = bar.agent_count.to_string();
    let visible = format!("✦ {} agent{}", count, plural);
    let content = format!(
        "{} {} agent{}",
        theme::accented(bar.accent, "✦"),
        theme::bold(&count),
        plural,
    );
    Some(Cell { width: visible.chars().count(), content })
}
