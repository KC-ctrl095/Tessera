// A stand-in desktop shell shared by the machine settings suites.
export function shellScript(token) {
  return `(() => {
    const TOKEN = ${JSON.stringify(token)};
    const HOME = "/home/me";
    let n = 0, ws = null, rid = 0;
    const cbs = {}, listeners = {}, pending = new Map();
    window.__calls = [];
    window.__node = {
      enabled: false, revoked: false, name: "devbox", version: "0.1.0", scope: { folders: [], commands: false, ask: "always", timeout_secs: 300 },
      os: "linux x86_64",
      paired: false, running: false, connected: false, paused: false, calls: 0, sandbox: "on (namespaces + Landlock)", sandbox_ok: true,
      service: { available: true, installed: false, active: false, enabled: false, linger: false }, log_file: "/home/me/.local/share/tessera/node.log",
      preset: "look_only", home: HOME, desktop_session: "Hyprland on Wayland", bus_filter: true, startup_changes: 0, admin_password: false,
    };
    window.__chosen = [];
    window.__changes = [
      { id: 2, ts: Date.now() - 60000, path: HOME + "/.config/hypr/hyprland.conf", existed: true, exists: true, action: "run", by: "“Tidy my desktop”", lost: false },
      { id: 1, ts: Date.now() - 90000, path: HOME + "/.config/autostart/notes.desktop", existed: false, exists: true, action: "write", by: "Forge in “Sweep”", lost: false },
    ];
    window.__log = [
      { ts: Date.now() - 50000, event: "done", call_id: "c1", session_id: "s1", chat: "Build the site", action: "run", target: "cargo build", detail: "exit 0", duration_ms: 6400 },
      { ts: Date.now() - 60000, event: "started", call_id: "c1", session_id: "s1", chat: "Build the site", action: "run", target: "cargo build", detail: "" },
      { ts: Date.now() - 120000, event: "refused", call_id: "c0", session_id: "s2", chat: "Tidy up", action: "write", target: "/etc/hosts", detail: "outside the allowed folders", bot: "Forge" },
      { ts: Date.now() - 180000, event: "paused", detail: "from Pixel 9" },
    ];
    const LOCAL = { "/home/me": ["code", "notes", ".ssh"], "/home/me/code": ["site", "tools"], "/": ["home", "etc"], "/home": ["me"] };
    const rank = { always: 0, risky: 1, chat: 2 };
    const presetOf = (s) => {
      const homeRw = s.folders.some((f) => f.write && f.path === HOME);
      if (s.desktop) return homeRw && s.commands && s.folders.length === 1 ? "desktop_control" : "custom";
      if (homeRw && s.commands && s.folders.length === 1) return "full_access";
      if (!s.commands && s.folders.every((f) => !f.write)) return "look_only";
      if (s.folders.every((f) => f.write) && (s.commands || s.folders.length)) return "work_in_folders";
      return "custom";
    };
    const setScope = (s) => {
      const p = presetOf(s);
      if (p === "look_only" || p === "work_in_folders") window.__chosen = s.folders.map((f) => f.path);
      window.__node.scope = s;
      window.__node.preset = p;
    };
    const emit = (event, payload) => (listeners[event] || []).forEach((id) => cbs[id] && cbs[id]({ event, id: 1, payload }));
    const status = (state) => emit("tessera://status", { state, serverId: "srv_mock", endpoint: location.origin, error: null, encrypted: true });
    const node = () => ({ node: JSON.parse(JSON.stringify(window.__node)), appServer: "Mock server", serverHere: false });
    function connect() {
      ws = new WebSocket(location.origin.replace(/^http/, "ws") + "/api/ws?token=" + encodeURIComponent(TOKEN));
      ws.onopen = () => status("connected");
      ws.onclose = () => status("disconnected");
      ws.onmessage = (m) => {
        const msg = JSON.parse(m.data);
        if (msg.event) emit("tessera://event", msg.event);
        else if (pending.has(msg.id)) { const p = pending.get(msg.id); pending.delete(msg.id); msg.error ? p.reject(msg.error) : p.resolve(msg.result ?? null); }
      };
    }
    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
    window.__TAURI_INTERNALS__ = {
      metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main", windowLabel: "main" } },
      transformCallback(cb, once) { const id = ++n; cbs[id] = (x) => { if (once) delete cbs[id]; cb(x); }; return id; },
      async invoke(cmd, args = {}) {
        window.__calls.push({ cmd, args: JSON.parse(JSON.stringify(args)) });
        switch (cmd) {
          case "plugin:event|listen": (listeners[args.event] ||= []).push(args.handler); return args.handler;
          case "plugin:event|unlisten": return null;
          case "servers_get": return { servers: [{ id: "srv_mock", name: "Mock server", endpoints: [location.origin], headers: {}, token: "", serverKey: "", fingerprint: "1111 2222", deviceName: "Desktop app" }], active: "srv_mock" };
          case "servers_save": return null;
          case "server_connect": if (!ws) connect(); else status("connected"); return null;
          case "server_disconnect": return null;
          case "app_resumed": return null;
          case "device_report": return { computer: null, host: "devbox", manufacturer: null, model: null };
          case "rpc": return new Promise((resolve, reject) => { const id = ++rid; pending.set(id, { resolve, reject }); const r = args.request; ws.send(JSON.stringify({ id, method: r.method, ...(r.params !== undefined ? { params: r.params } : {}) })); });
          case "this_computer_status": return node();
          case "this_computer_enable": Object.assign(window.__node, { enabled: args.on, running: args.on, connected: args.on, revoked: false, paired: true, server: "Mock server" }); return node();
          case "this_computer_scope": setScope(args.scope); return node();
          // \`tessera node admin password --stdin\` checks it with sudo first ("wrong" stands for one sudo refuses).
          case "this_computer_admin_password":
            if (args.password === "wrong") throw "sudo didn't accept that password";
            window.__node.admin_password = !!args.password;
            return node();
          case "this_computer_preset": {
            const cur = window.__node.scope;
            const def = args.preset === "desktop_control" ? "always" : "risky";
            let s = { ...cur, folders: cur.folders.map((f) => ({ ...f })) };
            if (args.preset) {
              const ask = rank[cur.ask] <= rank[def] ? cur.ask : def;
              const chosen = args.folders ?? window.__chosen;
              if (args.preset === "look_only" || args.preset === "work_in_folders") {
                const w = args.preset === "work_in_folders";
                const webviews = w && !!cur.webviews && window.__node.preset === "work_in_folders";
                const sandbox = window.__node.features == null ? window.__node.sandbox_ok : window.__node.features.includes("sandbox");
                s = { ...cur, ask, folders: chosen.map((p) => ({ path: p, write: w })), commands: w && sandbox && window.__node.sandbox_ok, desktop: false, webviews };
              } else {
                s = { ...cur, ask, folders: [{ path: HOME, write: true }], commands: true, desktop: args.preset === "desktop_control", webviews: true };
              }
            }
            if (args.ask) s.ask = args.ask;
            if (args.webviews != null) s.webviews = args.webviews;
            // Administrator needs commands (node apply_preset).
            if (!s.commands) delete s.admin;
            setScope(s);
            return node();
          }
          // Stop and Guard controls added by the Windows desktop track.
          case "this_computer_guard_install": window.__node.guard = { installed: true, running: true, stopped: false, stops: 0 }; return node();
          case "this_computer_stop": { const s = { ...window.__node.scope }; if (args.stopped) { s.stopped = true; s.stops = (s.stops ?? 0) + 1; } else delete s.stopped; window.__node.scope = s; return node(); }
          case "this_computer_access": { const s = { ...window.__node.scope }; if (args.bots) s.bots = args.bots; else delete s.bots; window.__node.scope = s; return node(); }
          case "this_computer_startup_changes": return window.__changes.slice(0, args.limit);
          case "this_computer_restore": { const c = window.__changes.find((x) => x.id === args.id); window.__changes.unshift({ ...c, id: 9, ts: Date.now(), action: "restore", by: "putting back change " + args.id }); return c; }
          case "this_computer_service": window.__node.service = { available: true, installed: args.on, active: args.on, enabled: args.on, linger: false }; return node();
          case "this_computer_log": return window.__log.slice(0, args.limit);
          case "this_computer_home": return HOME;
          case "this_computer_list_dir": {
            const p = String(args.path).replace(/\\/+$/, "") || "/";
            const kids = LOCAL[p];
            if (!kids) throw "No such folder";
            return kids.map((k) => ({ name: k, path: (p === "/" ? "" : p) + "/" + k, is_dir: true }));
          }
          default: throw "command " + cmd + " not found";
        }
      },
    };
  })()`;
}
