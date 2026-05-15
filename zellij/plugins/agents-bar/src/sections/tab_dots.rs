use crate::{theme, AgentStatus, Bar, Cell};

/// Per-tab agent dots. Each dot reflects an agent's effective status:
/// `●` ok (dim), `?` awaiting (cyan), `!` held (yellow), `✗` exited (red).
/// Current tab bracketed in the accent colour.
pub fn render(bar: &Bar) -> Option<Cell> {
    if bar.agents_per_tab.len() < 2 { return None; }
    let me = bar.my_tab?;
    let accent = theme::fg(bar.accent);

    let mut content = String::new();
    let mut width = 0;

    for (i, agents) in bar.agents_per_tab.iter().enumerate() {
        if i > 0 {
            content.push_str(&format!(" {}\u{2502}{} ", theme::DIM, theme::NDIM));
            width += 3;
        }
        let visible_w = if agents.is_empty() { 0 } else { agents.len() * 2 - 1 };
        let dots = agents.iter()
            .map(|a| styled_glyph(bar.effective_status(a)))
            .collect::<Vec<_>>()
            .join(" ");
        if i + 1 == me {
            content.push_str(&format!(
                "{}[{}{}{}]{}",
                accent, theme::RESET, dots, accent, theme::RESET,
            ));
            width += visible_w + 2;
        } else {
            content.push_str(&dots);
            width += visible_w;
        }
    }

    Some(Cell { width, content })
}

fn styled_glyph(s: AgentStatus) -> String {
    match s {
        AgentStatus::Ok => theme::dim("●"),
        AgentStatus::Awaiting => format!("\u{1b}[36;1m?\u{1b}[0m"),
        AgentStatus::Held => format!("\u{1b}[33;1m!\u{1b}[0m"),
        AgentStatus::Exited => format!("\u{1b}[31;1m✗\u{1b}[0m"),
    }
}
