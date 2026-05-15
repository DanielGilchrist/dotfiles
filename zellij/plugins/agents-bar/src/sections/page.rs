use crate::{theme, Bar, Cell};

/// `│ Page N/M` — dim bar, bold "Page", accent number.
pub fn render(bar: &Bar) -> Option<Cell> {
    if bar.total_tabs <= 1 { return None; }
    let me = bar.my_tab?;
    let n = format!("{}/{}", me, bar.total_tabs);
    let visible = format!("│ Page {}", n);
    let content = format!(
        "{} {} {}",
        theme::dim("│"),
        theme::bold("Page"),
        theme::accented(bar.accent, &n),
    );
    Some(Cell { width: visible.chars().count(), content })
}
