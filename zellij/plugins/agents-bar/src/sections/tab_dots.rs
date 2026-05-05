use crate::{theme, AgentStatus, Bar, Cell};

/// Per-tab agent dots. Each dot reflects an agent's status:
/// `●` ok (dim), `!` held (yellow), `✗` exited (red).
/// Current tab bracketed in the accent colour, others have no brackets.
/// e.g. tab 1 active with 6 ok agents and 1 held on tab 2: `[● ● ● ● ● ●] !`.
pub fn render(bar: &Bar) -> Option<Cell> {
    if bar.agents_per_tab.len() < 2 { return None; }
    let me = bar.my_tab?;
    let accent = theme::fg(bar.accent);

    let mut content = String::new();
    let mut width = 0;

    for (i, statuses) in bar.agents_per_tab.iter().enumerate() {
        if i > 0 {
            content.push(' ');
            width += 1;
        }
        let dots_visible = render_dots_visible(statuses);
        let dots_styled = render_dots_styled(statuses);
        if i + 1 == me {
            // Bracket in accent, glyphs keep their per-status styling.
            content.push_str(&format!(
                "{}[{}{}{}]{}",
                accent, theme::RESET, dots_styled, accent, theme::RESET,
            ));
            width += dots_visible.chars().count() + 2;
        } else {
            content.push_str(&dots_styled);
            width += dots_visible.chars().count();
        }
    }

    Some(Cell { width, content })
}

/// Visible (non-ANSI) representation, used only for width.
fn render_dots_visible(statuses: &[AgentStatus]) -> String {
    statuses.iter().map(glyph)
        .collect::<Vec<_>>()
        .join(" ")
}

/// ANSI-styled dots, joined by single spaces.
fn render_dots_styled(statuses: &[AgentStatus]) -> String {
    statuses.iter()
        .map(|s| styled_glyph(*s))
        .collect::<Vec<_>>()
        .join(" ")
}

fn glyph(s: &AgentStatus) -> &'static str {
    match s {
        AgentStatus::Ok => "●",
        AgentStatus::Held => "!",
        AgentStatus::Exited => "✗",
    }
}

fn styled_glyph(s: AgentStatus) -> String {
    match s {
        AgentStatus::Ok => theme::dim("●"),
        AgentStatus::Held => format!("\u{1b}[33;1m!\u{1b}[0m"),
        AgentStatus::Exited => format!("\u{1b}[31;1m✗\u{1b}[0m"),
    }
}
