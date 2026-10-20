//! Independent saved pages in a tile's tab strip. A sign-in popup remains
//! on its opener's stack, not in this group. Groups hold IDs, not page data.
use super::Browser;
use tessera_proto::Whiteboard;

impl Browser {
    pub(crate) fn group(&self, id: &str) -> Vec<String> {
        if let Some(ids) = self.0.groups.lock().get(id).cloned() {
            return ids;
        }
        let ids = self.host().map(|h| h.tab_group(id)).unwrap_or_default();
        let mut groups = self.0.groups.lock();
        if ids.is_empty() {
            groups.insert(id.into(), Vec::new());
        } else {
            for key in &ids {
                groups.insert(key.clone(), ids.clone());
            }
        }
        ids
    }

    pub(crate) fn new_page(&self, from: &str, raw: &str) -> Result<Whiteboard, String> {
        let board = self.save_page(from, raw)?;
        self.group_page(from, &board.id);
        Ok(board)
    }

    pub(crate) fn save_page(&self, from: &str, raw: &str) -> Result<Whiteboard, String> {
        if self.group(from).len() >= 16 {
            return Err("close a tab first (16 per web view)".into());
        }
        let url = super::normalize_address(raw)?;
        if super::is_own_server(&url, self.host()?.own_port()) {
            return Err("Tessera's own server can't be opened in the web view".into());
        }
        let board = self.host()?.linked_board(from, &url)?;
        self.board_tab(&board.id)?;
        Ok(board)
    }

    pub(crate) fn group_page(&self, from: &str, id: &str) {
        let _ = self.group(from);
        let ids = {
            let mut groups = self.0.groups.lock();
            let mut ids = groups
                .get(from)
                .filter(|ids| !ids.is_empty())
                .cloned()
                .unwrap_or_else(|| vec![from.into()]);
            if !ids.iter().any(|s| s == id) {
                ids.push(id.into());
            }
            for key in &ids {
                groups.insert(key.clone(), ids.clone());
            }
            ids
        };
        if let Ok(host) = self.host() {
            host.save_tab_group(&ids);
        }
        self.publish_group(&ids);
    }

    pub(crate) fn ungroup(&self, id: &str) {
        let ids = {
            let mut groups = self.0.groups.lock();
            let Some(mut ids) = groups.remove(id) else {
                return;
            };
            ids.retain(|s| s != id);
            for key in &ids {
                if ids.len() <= 1 {
                    groups.remove(key);
                } else {
                    groups.insert(key.clone(), ids.clone());
                }
            }
            ids
        };
        if let Ok(host) = self.host() {
            host.save_tab_group(&ids);
        }
        self.publish_group(&ids);
    }

    fn publish_group(&self, ids: &[String]) {
        for id in ids {
            if let Some(t) = self.find(id) {
                t.top().publish();
            }
        }
    }
}
