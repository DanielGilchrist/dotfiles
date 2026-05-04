use zellij_tile::prelude::*;

mod sections;
mod theme;

/// Shared state observed from zellij events. Sections read this to render.
#[derive(Default)]
pub struct Bar {
    pub plugin_id: u32,
    pub my_tab: Option<usize>, // 1-indexed position of the tab this plugin lives in
    pub total_tabs: usize,
    pub agent_count: usize,            // selectable terminal panes across all tabs
    pub agents_per_tab: Vec<usize>,    // index = tab position (0-based)
    pub focused_title: Option<String>, // currently-focused terminal pane title in *this* tab
    pub attention_count: usize,        // panes that are held or exited
    pub mode: InputMode,               // zellij input mode (Normal, Scroll, Locked, …)
    pub accent: Option<PaletteColor>,  // active-pane border colour from the theme
}

register_plugin!(Bar);

const SIDE_MARGIN: usize = 1;
const SEP_VISIBLE: &str = " · ";

fn section_sep() -> String {
    format!(" {}\u{b7}{} ", theme::DIM, theme::NDIM)
}

impl ZellijPlugin for Bar {
    fn load(&mut self, _: std::collections::BTreeMap<String, String>) {
        request_permission(&[PermissionType::ReadApplicationState]);
        self.plugin_id = get_plugin_ids().plugin_id;
        subscribe(&[
            EventType::TabUpdate,
            EventType::PaneUpdate,
            EventType::ModeUpdate,
        ]);
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::TabUpdate(tabs) => self.total_tabs = tabs.len(),
            Event::PaneUpdate(manifest) => {
                self.my_tab = manifest
                    .panes
                    .iter()
                    .find(|(_, panes)| panes.iter().any(|p| p.is_plugin && p.id == self.plugin_id))
                    .map(|(pos, _)| pos + 1);

                let max_pos = manifest.panes.keys().max().copied().unwrap_or(0);
                let mut counts = vec![0; max_pos + 1];
                let mut attention = 0;
                for (pos, panes) in &manifest.panes {
                    for p in panes {
                        if p.is_plugin || p.is_suppressed { continue; }
                        counts[*pos] += 1;
                        if p.is_held || p.exited { attention += 1; }
                    }
                }
                self.agent_count = counts.iter().sum();
                self.agents_per_tab = counts;
                self.attention_count = attention;

                self.focused_title = self.my_tab.and_then(|t| {
                    manifest.panes.get(&(t - 1)).and_then(|panes| {
                        panes.iter()
                            .find(|p| p.is_focused && !p.is_plugin)
                            .map(|p| p.title.clone())
                    })
                });
            }
            Event::ModeUpdate(mode) => {
                self.accent = Some(mode.style.colors.frame_selected.base);
                self.mode = mode.mode;
            }
            _ => return false,
        }
        true
    }

    fn render(&mut self, _rows: usize, cols: usize) {
        let left: Vec<Cell> = sections::LEFT.iter().filter_map(|s| s(self)).collect();
        let right: Vec<Cell> = sections::RIGHT.iter().filter_map(|s| s(self)).collect();
        if left.is_empty() && right.is_empty() {
            return;
        }

        let sep_w = SEP_VISIBLE.chars().count();
        let sep = section_sep();
        let used = total_width(&left, sep_w) + total_width(&right, sep_w);
        let pad = cols
            .saturating_sub(used)
            .saturating_sub(SIDE_MARGIN * 2);

        print!(
            "{}{}{}{}{}\u{1b}[0m",
            " ".repeat(SIDE_MARGIN),
            join_cells(&left, &sep),
            " ".repeat(pad),
            join_cells(&right, &sep),
            " ".repeat(SIDE_MARGIN),
        );
    }
}

/// A renderable bar segment: visible width + ANSI-styled content.
pub struct Cell {
    pub width: usize,
    pub content: String,
}

fn total_width(cells: &[Cell], sep_w: usize) -> usize {
    let n = cells.len();
    cells.iter().map(|c| c.width).sum::<usize>() + sep_w * n.saturating_sub(1)
}

fn join_cells(cells: &[Cell], sep: &str) -> String {
    cells.iter().map(|c| c.content.as_str()).collect::<Vec<_>>().join(sep)
}
