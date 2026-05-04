use crate::{theme, Bar, Cell};

/// Per-tab agent counts as dots. Current tab bracketed in accent; other tabs
/// dim. e.g. tab 1 active with 6 agents, 1 on tab 2: `[●●●●●●] ●`.
pub fn render(bar: &Bar) -> Option<Cell> {
    if bar.agents_per_tab.len() < 2 { return None; }
    let me = bar.my_tab?;

    let mut content = String::new();
    let mut width = 0;

    for (i, count) in bar.agents_per_tab.iter().enumerate() {
        if i > 0 {
            content.push(' ');
            width += 1;
        }
        let dots: String = "●".repeat(*count);
        if i + 1 == me {
            content.push_str(&theme::accented(bar.accent, &format!("[{}]", dots)));
            width += count + 2;
        } else {
            content.push_str(&theme::dim(&dots));
            width += count;
        }
    }

    Some(Cell { width, content })
}
