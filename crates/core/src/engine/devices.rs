//! Paired devices as the user sees them (Settings → Devices): what each is,
//! which of them run on the same computer, and their names.
//!
//! One computer often has several logins here: its desktop app, its
//! terminal (`tessera`), and its machine (`tessera node`, either the machine
//! side of one of those or a login of its own). Newer programs say which
//! computer they run on (`device_hello`, `node_hello`: a key only Tessera
//! programs on that computer produce, `tessera_secure::computer_key`), and
//! the server tells every client which devices belong together, so each
//! shows the same list. Older programs say nothing; they join a computer
//! only by an exact match of its name, and only where nothing is in doubt
//! (see [`group_computers`]): two computers are never shown as one.
//!
//! Names: a device keeps the name it paired with until the user renames it
//! (`rename_device`, any of the user's devices; logged). Nothing a device
//! reports changes its name. A machine's own name, the one agents and
//! approvals use, is that computer's to change (`tessera node name`, or its
//! desktop app), never from here.

use super::*;
use tessera_proto::DeviceClient;

/// Longest device name, in characters.
const NAME_MAX: usize = 60;

impl Inner {
    /// The paired devices: who is online, what each is, and which share a
    /// computer (`DeviceInfo::computer`).
    pub(super) fn device_list(&self, current: Option<&str>) -> Result<Vec<DeviceInfo>, ApiError> {
        let rows = self.store.device_rows(current).map_err(internal)?;
        let (mut list, keys): (Vec<DeviceInfo>, Vec<Option<String>>) = rows.into_iter().unzip();
        for d in &mut list {
            if d.client == DeviceClient::Unknown {
                d.client = client_by_name(&d.name);
            }
        }
        self.mark_online(&mut list);
        let machines: HashMap<String, String> = self
            .machine_list()
            .into_iter()
            .map(|m| (m.id, m.name))
            .collect();
        group_computers(&mut list, &keys, &machines);
        Ok(list)
    }

    pub(super) fn devices_request(&self, req: Request, device: Option<&str>) -> ApiResult {
        match req {
            Request::ListDevices => to_value(self.device_list(device)?),
            Request::DeviceHello {
                client,
                computer,
                host,
                model,
            } => {
                let id = device.ok_or_else(|| bad("only a paired device can say what it is"))?;
                let computer = computer.filter(|k| is_key(k));
                let host = host.map(|h| clean_name(&h, 64)).filter(|h| !h.is_empty());
                let model = model
                    .map(|m| clean_name(&m, NAME_MAX))
                    .filter(|m| !m.is_empty());
                let before = self.device_list(Some(id))?;
                let found = self
                    .store
                    .report_device(
                        id,
                        Some(client),
                        computer.as_deref(),
                        host.as_deref(),
                        model.as_deref(),
                    )
                    .map_err(internal)?;
                if !found {
                    return Err(ApiError::new(ErrorCode::NotFound, "no such device"));
                }
                // The app's hello can arrive after its built-in machine's.
                // Replace once both the client kind and computer are known.
                if self.machines.has(id) {
                    self.replace_machine_login(id)?;
                }
                let after = self.device_list(Some(id))?;
                // Every client's list follows (only when something changed:
                // devices say hello each time they connect).
                if before != after {
                    self.emit_machines();
                }
                after
                    .into_iter()
                    .find(|d| d.id == id)
                    .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "no such device"))
                    .and_then(to_value)
            }
            Request::RenameDevice { id, name } => {
                let name = valid_name(&name)?;
                let list = self.device_list(device)?;
                let target = list
                    .iter()
                    .find(|d| d.id == id)
                    .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "no such device"))?;
                // The whole computer takes the name; a login that is only a
                // machine keeps the one that machine gave itself.
                let group: Vec<&DeviceInfo> = list
                    .iter()
                    .filter(|d| {
                        d.id == id || (d.computer.is_some() && d.computer == target.computer)
                    })
                    .collect();
                let renamed: Vec<&DeviceInfo> = group
                    .iter()
                    .copied()
                    .filter(|d| d.client != DeviceClient::Machine)
                    .collect();
                if renamed.is_empty() {
                    return Err(bad(format!(
                        "{} is a machine only: its name can be changed only on that computer (in its Tessera app, or `tessera node name`)",
                        target.name
                    )));
                }
                for d in &renamed {
                    self.store.rename_device(&d.id, &name).map_err(internal)?;
                    tracing::info!(
                        "device {} renamed from `{}` to `{name}` (asked by device {})",
                        d.id,
                        d.name,
                        device.unwrap_or("-")
                    );
                }
                self.emit_machines();
                to_value(self.device_list(device)?)
            }
            _ => Err(bad("not a device request")),
        }
    }
}

/// A computer key as `tessera_secure::computer_key` makes them (hex).
fn is_key(k: &str) -> bool {
    (16..=64).contains(&k.len()) && k.bytes().all(|b| b.is_ascii_hexdigit())
}

/// One line, control and hidden characters spelled out, at most `max` characters.
fn clean_name(s: &str, max: usize) -> String {
    crate::util::one_line(&crate::util::visible_controls(s), max)
}

/// A name the user gave: one line of visible text, 1 to 60 characters.
fn valid_name(raw: &str) -> Result<String, ApiError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(bad("Give it a name."));
    }
    if name.chars().count() > NAME_MAX {
        return Err(bad(format!("Names can be at most {NAME_MAX} characters.")));
    }
    if name
        .chars()
        .any(|c| c.is_control() || crate::scan::always_hidden(c))
    {
        return Err(bad("Names can't contain line breaks or hidden characters."));
    }
    Ok(name.to_string())
}

/// What a device that said nothing is, from the names Tessera's own
/// programs pair with.
fn client_by_name(name: &str) -> DeviceClient {
    let n = name.trim();
    let lower = n.to_lowercase();
    if lower.starts_with("terminal on ") || lower == "terminal (this machine)" {
        DeviceClient::Terminal
    } else if lower.ends_with("(desktop app)")
        || matches!(
            lower.as_str(),
            "desktop" | "desktop app" | "linux desktop" | "windows desktop" | "macos desktop"
        )
    {
        DeviceClient::App
    } else if matches!(
        lower.as_str(),
        "android phone" | "android tablet" | "android device" | "ios device"
    ) {
        DeviceClient::Phone
    } else if ["chrome", "firefox", "edge", "safari", "browser"]
        .iter()
        .any(|b| lower == *b || lower.starts_with(&format!("{b} on ")))
    {
        DeviceClient::Browser
    } else {
        DeviceClient::Unknown
    }
}

/// `s` without `affix` at its start (or end), letter case aside.
fn strip_start<'a>(s: &'a str, affix: &str) -> Option<&'a str> {
    s.get(..affix.len())
        .filter(|p| p.eq_ignore_ascii_case(affix))
        .map(|_| &s[affix.len()..])
}
fn strip_end<'a>(s: &'a str, affix: &str) -> Option<&'a str> {
    let at = s.len().checked_sub(affix.len())?;
    s.get(at..)
        .filter(|p| p.eq_ignore_ascii_case(affix))
        .map(|_| &s[..at])
}

/// The computer name a device that reported none gives away: "Terminal on
/// Desktop", "Desktop (desktop app)", or the name of the machine it is.
fn name_hint(d: &DeviceInfo, machines: &HashMap<String, String>) -> Option<String> {
    if matches!(d.client, DeviceClient::Phone | DeviceClient::Browser) {
        return None;
    }
    if let Some(h) = &d.host {
        return Some(h.clone());
    }
    let n = d.name.trim();
    strip_start(n, "terminal on ")
        .or_else(|| strip_end(n, "(desktop app)"))
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .map(str::to_string)
        .or_else(|| machines.get(&d.id).cloned())
}

/// Mark the devices that run on one computer: the same `computer` value
/// (the id of the first of them paired) for all of them.
///
/// Devices that reported the same key are one computer. A device that
/// reported none (an older program) joins a computer that did, by that
/// computer's own name ([`name_hint`]), only when exactly one computer has
/// that name and no device of the same kind is there or joining with it (a
/// second terminal on "raspberrypi" is more likely another Raspberry Pi).
/// Phones and browsers never join by name, and devices that reported
/// nothing never make a computer among themselves: when in doubt, apart.
pub(crate) fn group_computers(
    list: &mut [DeviceInfo],
    keys: &[Option<String>],
    machines: &HashMap<String, String>,
) {
    let n = list.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    fn join(parent: &mut [usize], a: usize, b: usize) {
        let (ra, rb) = (root(parent, a), root(parent, b));
        // The first-paired device stays the root (its id names the group).
        parent[ra.max(rb)] = ra.min(rb);
    }
    // Same key, same computer.
    let mut by_key: HashMap<&str, usize> = HashMap::new();
    for (i, k) in keys.iter().enumerate() {
        if let Some(k) = k.as_deref() {
            match by_key.get(k) {
                Some(&first) => join(&mut parent, first, i),
                None => {
                    by_key.insert(k, i);
                }
            }
        }
    }
    let roots: Vec<usize> = (0..n).map(|i| root(&mut parent, i)).collect();
    // Each such computer: its names, and the kinds of device it has.
    let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
    let mut kinds: HashMap<usize, Vec<DeviceClient>> = HashMap::new();
    for i in (0..n).filter(|&i| keys[i].is_some()) {
        kinds.entry(roots[i]).or_default().push(list[i].client);
        if let Some(h) = list[i]
            .host
            .as_deref()
            .map(str::trim)
            .filter(|h| !h.is_empty())
        {
            let at = by_name.entry(h.to_lowercase()).or_default();
            if !at.contains(&roots[i]) {
                at.push(roots[i]);
            }
        }
    }
    // Older programs claim a computer by its name.
    let mut claims: Vec<(usize, usize)> = Vec::new();
    for i in (0..n).filter(|&i| keys[i].is_none()) {
        let Some(hint) = name_hint(&list[i], machines) else {
            continue;
        };
        let Some(at) = by_name.get(&hint.trim().to_lowercase()) else {
            continue;
        };
        // Two computers with that name: no telling which.
        if at.len() != 1 {
            continue;
        }
        let r = at[0];
        if !kinds.get(&r).is_some_and(|k| k.contains(&list[i].client)) {
            claims.push((r, i));
        }
    }
    for &(r, i) in &claims {
        let twice = claims
            .iter()
            .filter(|&&(r2, j)| r2 == r && list[j].client == list[i].client)
            .count()
            > 1;
        if !twice {
            join(&mut parent, r, i);
        }
    }
    for i in 0..n {
        let r = root(&mut parent, i);
        list[i].computer = Some(list[r].id.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(id: &str, name: &str, client: DeviceClient, host: Option<&str>) -> DeviceInfo {
        DeviceInfo {
            id: id.into(),
            name: name.into(),
            created_at: 0,
            last_seen: None,
            current: false,
            notify: true,
            online: false,
            computer: None,
            client,
            host: host.map(str::to_string),
            model: None,
            named: false,
            browser: client == DeviceClient::Browser,
        }
    }

    fn groups(list: &[DeviceInfo]) -> Vec<(&str, &str)> {
        list.iter()
            .map(|d| (d.id.as_str(), d.computer.as_deref().unwrap()))
            .collect()
    }

    #[test]
    fn one_key_one_computer() {
        // Desktop: its desktop app, and its terminal whose machine side runs there.
        let mut list = vec![
            dev("app", "Desktop app", DeviceClient::App, Some("Desktop")),
            dev("phone", "folding phone", DeviceClient::Phone, None),
            dev(
                "term",
                "Terminal on Desktop",
                DeviceClient::Terminal,
                Some("Desktop"),
            ),
        ];
        let keys = vec![Some("aa".repeat(16)), None, Some("aa".repeat(16))];
        group_computers(&mut list, &keys, &HashMap::new());
        assert_eq!(
            groups(&list),
            [("app", "app"), ("phone", "phone"), ("term", "app")]
        );
    }

    #[test]
    fn older_programs_join_by_the_computer_name_only_when_sure() {
        // The laptop: a new desktop app (key, host), an older machine paired
        // by link under its host name, an older terminal there.
        let mut list = vec![
            dev(
                "app",
                "Dell Laptop",
                DeviceClient::App,
                Some("DESKTOP-EXAMPLE"),
            ),
            dev("mach", "DESKTOP-EXAMPLE", DeviceClient::Machine, None),
            dev(
                "term",
                "Terminal on desktop-example",
                DeviceClient::Terminal,
                None,
            ),
            dev("phone", "DESKTOP-EXAMPLE", DeviceClient::Phone, None),
        ];
        let keys = vec![Some("bb".repeat(16)), None, None, None];
        let machines = HashMap::from([("mach".to_string(), "DESKTOP-EXAMPLE".to_string())]);
        group_computers(&mut list, &keys, &machines);
        assert_eq!(
            groups(&list),
            [
                ("app", "app"),
                ("mach", "app"),
                ("term", "app"),
                ("phone", "phone")
            ]
        );
    }

    #[test]
    fn in_doubt_apart() {
        // Two computers named raspberrypi, both new: by key, never by name.
        let mut list = vec![
            dev(
                "a",
                "Terminal on raspberrypi",
                DeviceClient::Terminal,
                Some("raspberrypi"),
            ),
            dev(
                "b",
                "Terminal on raspberrypi",
                DeviceClient::Terminal,
                Some("raspberrypi"),
            ),
            // An older machine named raspberrypi: which one? Neither.
            dev("c", "raspberrypi", DeviceClient::Machine, None),
        ];
        let keys = vec![Some("cc".repeat(16)), Some("dd".repeat(16)), None];
        let machines = HashMap::from([("c".to_string(), "raspberrypi".to_string())]);
        group_computers(&mut list, &keys, &machines);
        assert_eq!(groups(&list), [("a", "a"), ("b", "b"), ("c", "c")]);

        // One new computer, two older terminals claiming it: neither joins.
        let mut list = vec![
            dev("a", "pi desktop", DeviceClient::App, Some("pi")),
            dev("b", "Terminal on pi", DeviceClient::Terminal, None),
            dev("c", "Terminal on pi", DeviceClient::Terminal, None),
        ];
        let keys = vec![Some("ee".repeat(16)), None, None];
        group_computers(&mut list, &keys, &HashMap::new());
        assert_eq!(groups(&list), [("a", "a"), ("b", "b"), ("c", "c")]);

        // The same kind is there already (by key): the older one stays apart.
        let mut list = vec![
            dev("a", "Terminal on pi", DeviceClient::Terminal, Some("pi")),
            dev("b", "Terminal on pi", DeviceClient::Terminal, None),
        ];
        let keys = vec![Some("ff".repeat(16)), None];
        group_computers(&mut list, &keys, &HashMap::new());
        assert_eq!(groups(&list), [("a", "a"), ("b", "b")]);

        // Older programs alone never make a computer of themselves.
        let mut list = vec![
            dev("a", "Terminal on Desktop", DeviceClient::Terminal, None),
            dev("b", "Desktop (desktop app)", DeviceClient::App, None),
        ];
        group_computers(&mut list, &[None, None], &HashMap::new());
        assert_eq!(groups(&list), [("a", "a"), ("b", "b")]);
    }

    #[test]
    fn names_and_kinds() {
        assert!(valid_name("Dell Laptop").is_ok());
        assert_eq!(valid_name("  Galaxy Z Fold ").unwrap(), "Galaxy Z Fold");
        assert!(valid_name("").is_err() && valid_name("   ").is_err());
        assert!(valid_name(&"x".repeat(61)).is_err());
        assert!(valid_name(&"é".repeat(60)).is_ok());
        assert!(valid_name("two\nlines").is_err());
        assert!(valid_name("hidden\u{202e}text").is_err());
        assert_eq!(
            client_by_name("Terminal on Desktop"),
            DeviceClient::Terminal
        );
        assert_eq!(client_by_name("Desktop (desktop app)"), DeviceClient::App);
        assert_eq!(client_by_name("Linux desktop"), DeviceClient::App);
        assert_eq!(client_by_name("Android tablet"), DeviceClient::Phone);
        assert_eq!(client_by_name("Chrome on Linux"), DeviceClient::Browser);
        assert_eq!(client_by_name("Dell Laptop"), DeviceClient::Unknown);
        assert!(is_key(&"0f".repeat(16)) && !is_key("short") && !is_key(&"zz".repeat(16)));
        let m = HashMap::new();
        let hint = |name: &str| name_hint(&dev("x", name, DeviceClient::Unknown, None), &m);
        assert_eq!(hint("Terminal on Desktop").as_deref(), Some("Desktop"));
        assert_eq!(hint("Desktop (desktop app)").as_deref(), Some("Desktop"));
        assert_eq!(hint("Dell Laptop"), None);
        assert_eq!(hint("Terminal on "), None);
    }
}
