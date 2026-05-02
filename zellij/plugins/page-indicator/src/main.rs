use zellij_tile::prelude::*;

#[derive(Default)]
struct State {
    plugin_id: u32,
    my_tab: Option<usize>, // 1-indexed
    total: usize,
}

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, _: std::collections::BTreeMap<String, String>) {
        request_permission(&[PermissionType::ReadApplicationState]);
        self.plugin_id = get_plugin_ids().plugin_id;
        subscribe(&[EventType::TabUpdate, EventType::PaneUpdate]);
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::TabUpdate(tabs) => self.total = tabs.len(),
            Event::PaneUpdate(manifest) => {
                self.my_tab = manifest.panes.iter()
                    .find(|(_, panes)| panes.iter().any(|p| p.is_plugin && p.id == self.plugin_id))
                    .map(|(pos, _)| pos + 1);
            }
            _ => return false,
        }
        true
    }

    fn render(&mut self, _rows: usize, cols: usize) {
        if self.total <= 1 { return; }
        let Some(me) = self.my_tab else { return; };
        let label = format!(" Page {}/{} ", me, self.total);
        let pad = cols.saturating_sub(label.chars().count());
        print!("{}\u{1b}[1;7m{}\u{1b}[0m", " ".repeat(pad), label);
    }
}
