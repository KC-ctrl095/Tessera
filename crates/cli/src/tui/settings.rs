//! `/settings` (F2): the few things the terminal needs, in one short list.
//! Each row says what it is now; ⏎ opens its choices (they come back
//! here) or flips it. Everything else lives in the apps' settings.

use tessera_proto::PermissionMode;

use super::app::{App, Modal, PickItem, PickKind, Picker, perm_desc, perm_label};

impl App {
    pub(super) fn open_settings(&mut self) {
        let now = self
            .pane()
            .session
            .as_ref()
            .map(|s| s.permission)
            .or(self.new_permission)
            .unwrap_or(PermissionMode::Default);
        let default_model = self
            .server
            .as_ref()
            .and_then(|s| s.default_model.as_ref())
            .map(|m| {
                self.model_info(m)
                    .map(|i| i.name.clone())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| m.model.clone())
            })
            .unwrap_or_else(|| "none yet".into());
        let on_off = |b: bool| if b { "on" } else { "off" };
        let rows = [
            (
                "Permissions",
                "permission",
                format!("{} · {}", perm_label(now), perm_desc(now)),
            ),
            ("Model for new chats", "default-model", default_model),
            (
                "Show thinking",
                "thinking",
                format!(
                    "{} · the model's reasoning, as it writes",
                    on_off(self.show_thinking)
                ),
            ),
            (
                "Theme",
                "theme",
                format!(
                    "{} · {}",
                    self.theme.pref().label(),
                    self.theme.pref().about()
                ),
            ),
            (
                "Mouse",
                "mouse",
                if self.mouse {
                    "on · the wheel scrolls; hold shift to select text".to_string()
                } else {
                    "off · select text freely; pgup/pgdn scroll".to_string()
                },
            ),
        ];
        let selected = match &self.modal {
            Some(Modal::Picker(p)) if p.kind == PickKind::Settings => p.selected,
            _ => self.settings_row,
        };
        let mut p = Picker::new(PickKind::Settings, "Settings");
        p.loading = false;
        p.items = rows
            .into_iter()
            .map(|(label, value, detail)| PickItem::new(label, value).detail(detail))
            .collect();
        p.selected = selected;
        self.modal = Some(Modal::Picker(p));
    }

    /// ⏎ on a settings row.
    pub(super) fn settings_choose(&mut self, value: &str, selected: usize) {
        self.settings_row = selected;
        match value {
            "permission" => self.open_levels(PickKind::Permission, true),
            "default-model" => self.open_model_picker(true),
            "theme" => self.open_themes(),
            "thinking" => {
                self.set_show_thinking(!self.show_thinking);
                self.open_settings();
            }
            "mouse" => {
                self.mouse = !self.mouse;
                self.mouse_changed = true;
                self.save_prefs();
                self.open_settings();
            }
            _ => {}
        }
    }
}
