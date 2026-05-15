use crate::{theme, Bar, Cell};

/// `▸ <name>` — dim arrow, plain title of the focused agent in this tab.
pub fn render(bar: &Bar) -> Option<Cell> {
    let title = bar.focused_title.as_ref()?;
    if title.is_empty() { return None; }
    let visible = format!("▸ {}", title);
    let content = format!("{} {}", theme::dim("▸"), title);
    Some(Cell { width: visible.chars().count(), content })
}
