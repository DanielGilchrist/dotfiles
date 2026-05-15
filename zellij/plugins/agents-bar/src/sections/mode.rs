use crate::{theme, Bar, Cell};
use zellij_tile::prelude::InputMode;

/// `[scroll]` etc. — visible only when zellij is in a non-Normal mode, since
/// staying in scroll/lock mode unexpectedly is a common cause of confusion.
pub fn render(bar: &Bar) -> Option<Cell> {
    if matches!(bar.mode, InputMode::Normal) { return None; }
    let name = format!("{:?}", bar.mode).to_lowercase();
    let visible = format!("[{}]", name);
    let content = theme::accented(bar.accent, &visible);
    Some(Cell { width: visible.chars().count(), content })
}
