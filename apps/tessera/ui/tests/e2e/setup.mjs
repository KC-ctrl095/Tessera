// Setup and pairing: the wizard's steps (the same as `tessera setup`: your AI, where you'll use it, your
// phone, done), the ways to an AI (Claude note, ChatGPT, servers found on this computer and the network, API
// keys), where you'll use it (home network, remote access: tunnel state, no-domain guide), the phone step's
// reach notes and "Copy pairing link", the summary's "More, any time"; how it looks folded, unfolded and on a
// desktop, dark and light; onboarding with a pasted pairing link (and pasted into an address field, and into
// Add a server); the QR scanner's viewfinder (plugin stubbed); on Windows, "Keep Tessera running" in the
// desktop app's first screen, "Start Tessera here" while it runs (the step it is on, how long, Stop waiting,
// Try again, a failure naming the log), and a Windows server (`mock_security { windows }`): new chats ask first,
// and the home-network switch says what Windows' firewall will ask.
import { APP, SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const shown = (p, sel) => p.eval(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); return !!e && !!e.getClientRects().length; })()`);
const text = (p, sel) => p.eval(`document.querySelector(${JSON.stringify(sel)})?.textContent ?? ''`);
const step = async (p, label) => {
  await p.clickText(".setup-step", label);
  await sleep(500);
};
// Nothing in the card sticks out past it (the mosaic above is wider on purpose, and clipped).
const noSideScroll = (p, card = ".setup-card") =>
  p.eval(`(() => { const c = document.querySelector(${JSON.stringify(card)}); if (!c) return true; const r = c.getBoundingClientRect(); return r.right <= innerWidth + 1 && [...c.querySelectorAll('*')].every((e) => { const b = e.getBoundingClientRect(); return !b.width || b.right <= r.right + 1; }); })()`);

run(async () => {
  const c = await wsClient("setup-suite");

  // ── the wizard on a first run ──
  await mock("/__mock/reset?fresh=1");
  await mock("/__mock/claude-code?state=missing");
  for (const [w, h, mobile, dpr, tag] of [
    [1440, 900, false, 1, "desktop"],
    [375, 812, true, 2, "phone"],
  ]) {
    console.log(`— wizard (${tag})`);
    await c.rpc("mock_tunnel", { remove: true });
    const p = await device();
    await p.viewport(w, h, mobile, dpr);
    await signIn(p, `Setup ${tag}`);
    await p.waitFor(`document.querySelector('.setup-wizard')`, 6000);
    await sleep(600);
    const rail = await p.eval(`[...document.querySelectorAll('.setup-step-label')].map(e => e.textContent.trim())`);
    check(JSON.stringify(rail) === JSON.stringify(["Your AI", "Where", tag === "phone" ? "Devices" : "Your phone", "Done"]), "four steps, like `tessera setup`", rail);
    check(/How should Tessera get its AI/.test(await text(p, ".setup-head h1")), "…starting with the AI, asked as the terminal asks it", await text(p, ".setup-head h1"));
    // It looks around by itself: a server on this computer is used at once, one elsewhere on the network waits for a click.
    await p.waitFor(`document.querySelectorAll('.setup-found li').length >= 2`, 6000).catch(() => {});
    const own = await text(p, ".setup-way-own");
    check(/Ollama \(this computer\)/.test(own) && /LM Studio on 192\.168\.1\.42/.test(own), "the models step finds AI on this computer and the network by itself", own);
    check((await shown(p, ".setup-model-now.ok")) && /qwen3:8b|Qwen3 8B/.test(await text(p, ".setup-model-now")), "…uses the one on this computer at once", await text(p, ".setup-model-now"));
    check(await p.eval(`[...document.querySelectorAll('.setup-found li')].some(li => /LM Studio/.test(li.textContent) && [...li.querySelectorAll('button')].some(b => b.textContent.trim() === 'Use'))`), "…and offers the one elsewhere with a Use button");
    check(await p.eval(`[...document.querySelectorAll('.setup-way-chatgpt button')].some(b => /Sign in/.test(b.textContent))`), "ChatGPT: a Sign in button");
    await p.clickText(".setup-way-key .chip", "Anthropic");
    await sleep(300);
    check(await shown(p, ".setup-ways .form-card input[type=password]"), "API key: Anthropic opens the key field (hidden as typed)");
    await p.clickText(".setup-way-key .chip", "OpenRouter");
    await sleep(300);
    check(await p.eval(`[...document.querySelectorAll('.setup-ways .form-card .chip')].map(c => c.textContent).join('|')`).then((t) => /OpenRouter/.test(t) && /OpenAI/.test(t)), "…OpenRouter, OpenAI or another: the form with presets");
    await p.clickText(".setup-way-key .chip", "OpenRouter");
    await sleep(200);
    check(await shown(p, ".claude-note"), "the models step has the Claude note");
    const note = await text(p, ".claude-note");
    check(/Pro or Max/.test(note) && /Claude Code/.test(note) && /API key/.test(note), "…offering the plan through their own Claude Code, or an API key", note);
    check(!(await p.eval(`[...document.querySelectorAll('button')].some(b => /sign in (with|to) claude/i.test(b.textContent))`)), "no Claude sign-in button anywhere");
    check(await p.eval(`[...document.querySelectorAll('.claude-note .link-btn')].map(b => b.textContent).join('|')`).then((t) => /Get an API key/.test(t) && /Install Claude Code/.test(t) && /Check again/.test(t)), "…with links: get a key, install Claude Code, check again");
    check(await noSideScroll(p), "no sideways scroll on the models step");
    await p.shot(`${SHOTS}/setup-models-${tag}.png`);
    // Claude Code appears on the server: signed out, then signed in (the server's hint; "Check again" asks anew).
    await mock("/__mock/claude-code?state=signed_out");
    await p.clickText(".claude-note .link-btn", "Check again");
    await sleep(500);
    check(/Sign in by running claude once/.test(await text(p, ".claude-note")) && (await p.eval(`!!document.querySelector('.claude-note code')`)) && !/Install Claude Code/.test(await text(p, ".claude-note")), "installed but signed out: the server's next step, with the command as code", await text(p, ".claude-note"));
    await mock("/__mock/claude-code?state=ready");
    await p.clickText(".claude-note .link-btn", "Check again");
    await sleep(600);
    check(/Claude is ready/.test(await text(p, ".claude-note")) && (await shown(p, ".claude-note.ok")), "…and ready once signed in", await text(p, ".claude-note"));
    check(await p.eval(`[...document.querySelectorAll('.setup option')].some(o => o.value.startsWith('claude-code/'))`), "…its models join the default-model choice");
    await p.shot(`${SHOTS}/setup-claude-ready-${tag}.png`);
    await mock("/__mock/claude-code?state=missing");

    // ── where you'll use it: the home network, and remote access (the no-domain guide) ──
    await step(p, "Where");
    check(await shown(p, ".lan-toggle"), "where step: the home network switch");
    check(await shown(p, ".remote-panel .search-choice"), "…and remote access: the two ways");
    await p.clickText(".nodomain .disclosure", "No domain");
    await sleep(300);
    const nd = await text(p, ".nodomain-body");
    check(/Tailscale/.test(nd) && /domain/i.test(nd) && /quick tunnels/.test(nd), "“No domain?” explains Tailscale, a cheap domain and why not quick tunnels", nd.slice(0, 200));
    check((await p.eval(`document.querySelectorAll('.nodomain-body .link-btn').length`)) === 3, "…with links: register a domain, get Tailscale, the full guide");
    check(await noSideScroll(p), "no sideways scroll on the remote step");
    await p.shot(`${SHOTS}/setup-nodomain-${tag}.png`);

    // ── the tunnel's live state ──
    await c.rpc("mock_tunnel", { hostname: "tessera.example.com", connected: false, error: "outbound connections to Cloudflare on port 7844 are blocked by a firewall" });
    await step(p, "Your AI");
    await step(p, "Where");
    await p.waitFor(`document.querySelector('.tunnel-line')`, 4000).catch(() => {});
    check(await shown(p, ".tunnel-line.bad"), "a tunnel that can't connect says so");
    check(/7844/.test(await text(p, ".tunnel-line")) && /set up/.test(await text(p, ".remote-status .strong")), "…with the reason, and the title doesn't claim it works", await text(p, ".remote-status"));
    await p.shot(`${SHOTS}/setup-tunnel-error-${tag}.png`);
    await c.rpc("mock_tunnel", { connected: true, error: null });
    await p.clickText(".tunnel-line .btn", "Check again");
    await sleep(700);
    check((await shown(p, ".tunnel-line.ok")) && /Reachable from anywhere/.test(await text(p, ".remote-status .strong")), "“Check again” picks up the connected tunnel");
    // While it connects, the panel follows it by itself.
    await c.rpc("mock_tunnel", { connected: false, error: null });
    await step(p, "Your AI");
    await step(p, "Where");
    await sleep(500);
    check(/connecting/.test(await text(p, ".tunnel-line")), "a connecting tunnel shows as connecting");
    await c.rpc("mock_tunnel", { connected: true });
    await sleep(3200);
    check((await shown(p, ".tunnel-line.ok")) && (await shown(p, ".remote-status.on")), "…and turns connected without a click");

    // ── the phone step: where the phone can reach it from ──
    await c.rpc("mock_tunnel", { connected: false, error: "outbound connections to Cloudflare on port 7844 are blocked by a firewall" });
    await step(p, tag === "phone" ? "Devices" : "Your phone");
    await p.waitFor(`document.querySelector('.reach-note')`, 4000).catch(() => {});
    await sleep(400);
    check((await shown(p, ".reach-note.warn")) && /tessera\.example\.com/.test(await text(p, ".reach-note")), "phone step: away from home waits for the tunnel (named)", await text(p, ".reach-note"));
    await p.waitFor(`document.querySelector('.pair-copy-link')`, 4000).catch(() => {});
    check(await shown(p, ".pair-copy-link"), "the pairing panel offers “Copy pairing link”");
    await p.click(".pair-copy-link");
    await sleep(300);
    check(await p.eval(`[...document.querySelectorAll('.toast')].some(t => /Pairing link copied/.test(t.textContent))`), "…and says it's private when copied");
    await p.shot(`${SHOTS}/setup-phone-tunnel-${tag}.png`);
    await c.rpc("mock_tunnel", { connected: true, error: null });
    await step(p, "Where");
    await step(p, tag === "phone" ? "Devices" : "Your phone");
    await sleep(600);
    check(await shown(p, ".reach-note.ok"), "…and says it works from anywhere once connected");
    await c.rpc("mock_tunnel", { remove: true });
    await c.rpc("set_remote_access", { urls: [] });
    await step(p, "Where");
    await step(p, tag === "phone" ? "Devices" : "Your phone");
    await sleep(600);
    check((await shown(p, ".reach-note.warn")) && /only this computer/.test(await text(p, ".reach-note")), "no LAN, no remote: only this computer (with a way to fix it)");
    await p.clickText(".reach-note .link-btn", "set up remote access");
    await sleep(400);
    check(await shown(p, ".remote-panel"), "…which leads to the remote step");
    await c.rpc("set_remote_access", { urls: ["https://tessera.example.com"] });

    // ── leaving halfway: closing the app (or an update restarting it) picks setup up where it was left ──
    const phoneLabel = tag === "phone" ? "Devices" : "Your phone";
    await step(p, phoneLabel);
    const phoneDone = await p.eval(`document.querySelector('.setup-step.on')?.classList.contains('done')`);
    await p.clickText(".setup-foot .btn", phoneDone ? "Continue" : "Skip");
    await sleep(500);
    check(/You're set/.test(await text(p, ".setup-head h1")), "skipping the phone step goes on to the summary");
    const kept = await p.eval(`Object.keys(localStorage).filter(k => k.startsWith('tessera.setup.')).map(k => localStorage.getItem(k))`);
    check(kept.length === 1 && /"step":"done"/.test(kept[0]) && /"phone"/.test(kept[0]) && !/key|token|password|secret/i.test(kept[0]), "…where it is stays on this device: step names only", kept);
    await p.reload(1500);
    await p.waitFor(`document.querySelector('.setup-wizard')`, 8000).catch(() => {});
    await sleep(600);
    check(/You're set/.test(await text(p, ".setup-head h1")), "after a restart, setup opens where it was left", await text(p, ".setup-head h1"));
    check(await p.eval(`[...document.querySelectorAll('.setup-step')].find(b => b.textContent.includes(${JSON.stringify(phoneLabel)}))?.classList.contains('done')`), "…with the skipped step still counted");
    check(/"v":1/.test(kept[0]), "…kept with its version", kept[0]);
    // Something unexpected kept there (another version, a hand edit) is set aside: setup still opens, at the start.
    const keptKey = await p.eval(`Object.keys(localStorage).find(k => k.startsWith('tessera.setup.'))`);
    // Kept by the version before (no `v`): picked up, not lost.
    await p.eval(`localStorage.setItem(${JSON.stringify(keptKey)}, ${JSON.stringify('{"step":"done","marked":["phone"]}')})`);
    await p.reload(1500);
    await p.waitFor(`document.querySelector('.setup-wizard')`, 8000).catch(() => {});
    await sleep(400);
    check(/You're set/.test(await text(p, ".setup-head h1")), "setup kept by the version before is picked up where it was", await text(p, ".setup-head h1"));
    for (const bad of ["null", '{"marked":"phone"}', '{"v":2,"step":"done"}', "[1,2]"]) {
      await p.eval(`localStorage.setItem(${JSON.stringify(keptKey)}, ${JSON.stringify(bad)})`);
      await p.reload(1500);
      await p.waitFor(`document.querySelector('.setup-wizard')`, 8000).catch(() => {});
      await sleep(400);
      check((await shown(p, ".setup-wizard")) && /How should Tessera get its AI/.test(await text(p, ".setup-head h1")), `kept setup state ${bad}: set aside, setup opens at the start`, await text(p, ".setup-head h1"));
    }
    const reopen = async () => {
      await p.reload(1500);
      await p.waitFor(`document.querySelector('.setup-wizard')`, 8000).catch(() => {});
      await sleep(600);
      return text(p, ".setup-head h1");
    };
    const phoneTitle = tag === "phone" ? /Add another device/ : /Pair your phone/;
    const counted = (label) => p.eval(`[...document.querySelectorAll('.setup-step')].find(b => b.textContent.includes(${JSON.stringify(label)}))?.classList.contains('done')`);
    // Left halfway on a page with an earlier step skipped: a restart (or an update) opens that very page, the skip still counted.
    await c.rpc("set_remote_access", { urls: [] });
    await step(p, "Where");
    check(!(await counted("Where")), "(where you'll use it isn't set up yet)");
    await p.clickText(".setup-foot .btn", "Skip");
    await sleep(500);
    check(phoneTitle.test(await text(p, ".setup-head h1")), "skipping “Where” goes on to the phone step", await text(p, ".setup-head h1"));
    const half = await p.eval(`localStorage.getItem(${JSON.stringify(keptKey)})`);
    check(/"step":"phone"/.test(half) && /"marked":\["reach"\]/.test(half), "…kept: that page and the skip", half);
    check(phoneTitle.test(await reopen()), "after a restart, setup opens on the unfinished page it was left on", await text(p, ".setup-head h1"));
    check((await counted("Where")) && (await p.eval(`document.querySelector('.setup-step.on')?.textContent.includes(${JSON.stringify(phoneLabel)})`)), "…with “Where” still counted as skipped and the phone step the open one");
    // An update from a version that kept nothing (the builds before this one): setup opens at the first step the server doesn't have yet.
    await c.rpc("set_remote_access", { urls: ["https://tessera.example.com"] });
    const st = (await c.rpc("setup_status")).result;
    const first = !st.has_model ? "models" : !(st.lan || st.remote) ? "reach" : st.devices > 1 ? "done" : "phone";
    check(first !== "models", "(the server has its AI already)", st);
    await p.eval(`localStorage.removeItem(${JSON.stringify(keptKey)})`);
    const titles = { models: /How should Tessera get its AI/, reach: /Where you'll use it/, phone: phoneTitle, done: /You're set/ };
    check(titles[first].test(await reopen()), `nothing kept (an update from a version that didn't keep it): setup opens at the first step not set up yet (${first})`, await text(p, ".setup-head h1"));

    // ── done: the summary, and web search and voice one tap away ──
    await step(p, "Done");
    const more = await p.eval(`[...document.querySelectorAll('.setup-more .chip')].map(b => b.textContent.trim())`);
    check(more.some((t) => /Web search/.test(t)) && more.some((t) => /Voice/.test(t)), "done: “More, any time” has web search and voice", more);
    await p.shot(`${SHOTS}/setup-done-${tag}.png`);
    await p.clickText(".setup-more .chip", "Web search");
    await p.waitFor(`document.querySelector('.modal') && !document.querySelector('.setup-wizard')`, 4000).catch(() => {});
    check(!(await shown(p, ".setup-wizard")) && (await shown(p, ".modal")), "…and a tap finishes setup and opens that page of Settings");
    check((await c.rpc("setup_status")).result?.completed === true, "…marking setup as done");
    check(await p.eval(`!Object.keys(localStorage).some(k => k.startsWith('tessera.setup.'))`), "…and forgetting where it was");
    await mock("/__mock/reset?fresh=1");
    console.log(`${tag} errors:`, p.errors);
    check(!p.errors.length, `no page errors (${tag} wizard)`);
    await p.close();
  }

  // ── the desktop app's first screen, before any server: run Tessera here, or on another computer ──
  // A stand-in for the desktop shell: no saved servers, and the `tessera` program installed or not.
  const shell = (available) => `(() => {
    let n = 0;
    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
    window.__TAURI_INTERNALS__ = {
      metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main", windowLabel: "main" } },
      transformCallback() { return ++n; },
      async invoke(cmd) {
        switch (cmd) {
          case "servers_get": return { servers: [], active: null };
          case "local_server_detect": return { available: ${available}, running: false, url: null, saved: false };
          case "plugin:event|listen": return ++n;
          default: return null;
        }
      },
    };
  })()`;
  for (const [available, w, h, theme] of [
    [false, 1280, 860, "dark"],
    [true, 1280, 860, "light"],
  ]) {
    const tag = available ? "installed" : "not-installed";
    console.log(`— desktop app, first screen (${tag})`);
    const p = await device();
    await p.viewport(w, h);
    await p.send("Page.addScriptToEvaluateOnNewDocument", { source: shell(available) });
    await p.goto(APP, 1500);
    await p.theme(theme);
    await p.waitFor(`document.querySelector('.setup-start .setup-choice')`, 8000).catch(() => {});
    await sleep(600);
    check(await shown(p, ".setup-start"), `${tag}: the first screen asks where Tessera should run`);
    const start = await text(p, ".setup-start");
    check(/releases\/latest\/download\/install\.sh \| sh/.test(start), `${tag}: another computer gets the one install command`, start.slice(0, 300));
    if (available) {
      check(await p.eval(`[...document.querySelectorAll('.setup-start button')].some(b => /Start Tessera here/.test(b.textContent))`), "installed: “Start Tessera here”, with no code to type");
    } else {
      check(/One command in a terminal installs it/.test(start) && (await shown(p, ".setup-start .cmd-line")), "not installed: the one command to install it, with a copy button");
      check(!/scripts\/install\.sh/.test(start), "…never a script path from a source folder");
    }
    check(await noSideScroll(p), `${tag}: nothing sticks out sideways`);
    await p.shot(`${SHOTS}/setup-start-${tag}-${theme}.png`);
    if (!available) {
      // On another computer: the pairing link, or its address and code, after the same one command there.
      await p.clickText(".setup-start .setup-choice", "On another computer");
      await p.waitFor(`document.querySelector('.onboarding')`, 6000).catch(() => {});
      await p.clickText(".onboarding button", "Enter the address and code instead");
      await sleep(400);
      check(/releases\/latest\/download\/install\.sh \| sh/.test(await text(p, ".onb-howto")), "another computer: install it there with the one command, then pair", await text(p, ".onb-howto"));
      check(await noSideScroll(p, ".onb-card"), "…without sideways scroll");
      await p.shot(`${SHOTS}/setup-start-elsewhere-${theme}.png`);
    }
    check(!p.errors.length, `no page errors (first screen, ${tag})`, p.errors);
    await p.close();
  }

  // ── Windows: the desktop app's "Keep Tessera running" says what it means there (the PC stays awake while plugged in) ──
  for (const [w, h, theme] of [
    [1280, 860, "light"],
    [360, 760, "dark"],
  ]) {
    console.log(`— desktop app on Windows, first screen (${w}px, ${theme})`);
    const p = await device();
    await p.viewport(w, h);
    await p.send("Page.addScriptToEvaluateOnNewDocument", { source: `Object.defineProperty(Navigator.prototype, "platform", { get: () => "Win32" });\n${shell(true)}` });
    await p.goto(APP, 1500);
    await p.theme(theme);
    await p.waitFor(`document.querySelector('.setup-start .inline-toggle')`, 8000).catch(() => {});
    await sleep(500);
    const label = await text(p, ".setup-start .inline-toggle");
    check(/Keep Tessera running: start it when I sign in to Windows/.test(label), "Windows: “Keep Tessera running: start it when I sign in to Windows”", label);
    check(/keeps running with this window closed/.test(await text(p, ".setup-keep-on")) && /stays awake while it's plugged in/.test(await text(p, ".setup-keep-on")), "…saying it keeps running with the window closed and the PC stays awake while plugged in", await text(p, ".setup-keep-on"));
    await p.click(".setup-start .inline-toggle input");
    await sleep(200);
    check(/runs until you restart or shut down this PC/.test(await text(p, ".setup-keep-on")), "…and, unticked, that it runs only until the PC restarts", await text(p, ".setup-keep-on"));
    await p.click(".setup-start .inline-toggle input");
    await sleep(200);
    check(await noSideScroll(p), `Windows first screen (${w}px): nothing sticks out sideways`);
    await p.shot(`${SHOTS}/setup-start-windows-${w}-${theme}.png`);
    check(!p.errors.length, `no page errors (Windows first screen, ${w}px)`, p.errors);
    await p.close();
  }

  // ── "Start Tessera here" while it runs: the step it is on, how long, Stop waiting; then Try again ──
  // The stand-in keeps the start pending, sends the steps as the desktop shell does, and ends the
  // start the way the shell does when "Stop waiting" is pressed.
  const STOPPED = "Stopped waiting. Tessera may still be starting in the background: press “Try again” in a moment to connect to it.";
  const startingShell = `(() => {
    let n = 0;
    const cbs = {};
    window.__events = {};
    window.__calls = [];
    window.__args = [];
    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
    window.__TAURI_INTERNALS__ = {
      metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main", windowLabel: "main" } },
      transformCallback(cb) { const id = ++n; cbs[id] = cb; return id; },
      async invoke(cmd, args) {
        window.__calls.push(cmd);
        window.__args.push({ cmd, args });
        switch (cmd) {
          case "servers_get": return { servers: [], active: null };
          case "local_server_detect": return { available: true, running: false, url: null, saved: false };
          case "plugin:event|listen":
            // A slow shell: the step listener takes a while to be set up.
            if (window.__listenDelay) await new Promise((r) => setTimeout(r, window.__listenDelay));
            (window.__events[args.event] ||= []).push(args.handler);
            return ++n;
          case "local_server_start": return new Promise((resolve, reject) => { window.__start = { resolve, reject }; });
          case "local_server_cancel": window.__start?.reject(${JSON.stringify(STOPPED)}); return null;
          default: return null;
        }
      },
    };
    window.__emit = (event, payload) => (window.__events[event] || []).forEach((h) => cbs[h]?.({ event, id: 0, payload }));
  })()`;
  for (const [w, h, theme] of [
    [1280, 860, "light"],
    [360, 760, "dark"],
  ]) {
    console.log(`— desktop app on Windows, starting Tessera here (${w}px, ${theme})`);
    const p = await device();
    await p.viewport(w, h);
    await p.send("Page.addScriptToEvaluateOnNewDocument", { source: `Object.defineProperty(Navigator.prototype, "platform", { get: () => "Win32" });\n${startingShell}` });
    await p.goto(APP, 1500);
    await p.theme(theme);
    await p.waitFor(`document.querySelector('.setup-start .inline-toggle')`, 8000).catch(() => {});
    await sleep(400);
    await p.clickText(".setup-start button", "Start Tessera here");
    await p.waitFor(`document.querySelector('.setup-start-status')`, 4000).catch(() => {});
    check(await shown(p, ".setup-start-status"), `starting (${w}px): a status line shows while it starts`);
    check(
      await p.eval(`[...document.querySelectorAll('.setup-start .btn-primary')].some(b => b.disabled && /Starting Tessera…/.test(b.textContent))`),
      "…the button says “Starting Tessera…” and can't be pressed twice",
    );
    check(/Starting…/.test(await text(p, ".setup-start-step")), "…“Starting…” until the first step arrives", await text(p, ".setup-start-step"));
    await p.eval(`window.__emit("tessera://local-server-progress", { stage: "installing", message: "Adding Tessera to the programs Windows starts when you sign in, and starting it…" })`);
    await sleep(300);
    check(/Adding Tessera to the programs Windows starts/.test(await text(p, ".setup-start-step")), "…then the step it is on", await text(p, ".setup-start-step"));
    await sleep(2200);
    const time = await text(p, ".setup-start-time");
    check(/^[2-9] s$/.test(time.trim()), "…and how long it has been going", time);
    await p.eval(`window.__emit("tessera://local-server-progress", { stage: "waiting", message: "Waiting for the server to answer (a first start can take a minute)…" })`);
    await sleep(300);
    check(/Waiting for the server to answer/.test(await text(p, ".setup-start-step")), "…moving on to the next step", await text(p, ".setup-start-step"));
    check(await shown(p, ".setup-start-status button"), "…with “Stop waiting” beside it");
    check(await noSideScroll(p), `starting (${w}px): nothing sticks out sideways`);
    await p.shot(`${SHOTS}/setup-start-running-${w}-${theme}.png`);
    await p.clickText(".setup-start-status button", "Stop waiting");
    await p.waitFor(`document.querySelector('.setup-start .form-error')`, 4000).catch(() => {});
    check(await p.eval(`window.__calls.includes("local_server_cancel")`), "Stop waiting asks the shell to stop waiting");
    check(
      await p.eval(`(() => {
        const last = (cmd) => window.__args.filter((a) => a.cmd === cmd).at(-1)?.args?.attempt;
        return typeof last("local_server_start") === "number" && last("local_server_start") === last("local_server_cancel");
      })()`),
      "…for this try (the start and the stop name the same attempt)",
    );
    check(/Stopped waiting\. Tessera may still be starting/.test(await text(p, ".setup-start .form-error")), "…and says it may still be starting, and what to press next", await text(p, ".setup-start .form-error"));
    check(!(await shown(p, ".setup-start-status")), "…the status line goes away");
    check(
      await p.eval(`[...document.querySelectorAll('.setup-start .btn-primary')].some(b => !b.disabled && /Try again/.test(b.textContent))`),
      "…and the button offers “Try again”",
    );
    await p.shot(`${SHOTS}/setup-start-stopped-${w}-${theme}.png`);
    // A second try that fails says why (the shell names the server's log on Windows).
    await p.clickText(".setup-start button", "Try again");
    await p.waitFor(`document.querySelector('.setup-start-status')`, 4000).catch(() => {});
    await p.eval(`window.__start.reject("The server didn't start. The end of its log says why: C:\\\\Users\\\\you\\\\AppData\\\\Roaming\\\\tessera\\\\logs\\\\tessera.log")`);
    await p.waitFor(`/log says why/.test(document.querySelector('.setup-start .form-error')?.textContent ?? '')`, 4000).catch(() => {});
    check(/The end of its log says why: C:\\Users\\you\\AppData\\Roaming\\tessera\\logs\\tessera\.log/.test(await text(p, ".setup-start .form-error")), "a start that fails names the log to look at", await text(p, ".setup-start .form-error"));
    check(await noSideScroll(p), `failed start (${w}px): the log path doesn't stick out sideways`);
    // "Stop waiting" before the start even reached the shell (its step
    // listener still being set up): nothing is started, nothing hangs.
    await p.eval(`window.__listenDelay = 1500; window.__startsBefore = window.__calls.filter((c) => c === "local_server_start").length`);
    await p.clickText(".setup-start button", "Try again");
    await p.waitFor(`document.querySelector('.setup-start-status button')`, 2000).catch(() => {});
    await p.clickText(".setup-start-status button", "Stop waiting");
    await p.waitFor(`!document.querySelector('.setup-start-status')`, 6000).catch(() => {});
    check(!(await shown(p, ".setup-start-status")), "Stop pressed before the start reached the shell: it doesn't stay on “Stopping…”");
    check(
      await p.eval(`window.__calls.filter((c) => c === "local_server_start").length === window.__startsBefore`),
      "…nothing is started",
    );
    check(/Stopped waiting\. Tessera may still be starting/.test(await text(p, ".setup-start .form-error")), "…it says it stopped waiting", await text(p, ".setup-start .form-error"));
    check(
      await p.eval(`[...document.querySelectorAll('.setup-start .btn-primary')].some(b => !b.disabled && /Try again/.test(b.textContent))`),
      "…and “Try again” works again",
    );
    await p.eval(`window.__listenDelay = 0`);
    check(!p.errors.length, `no page errors (starting Tessera here, ${w}px)`, p.errors);
    await p.close();
  }

  // ── a Windows server: no sandbox, so new chats ask first; the home-network switch says what Windows will ask ──
  for (const [w, h, mobile, dpr, tag, theme] of [
    [1440, 900, false, 1, "desktop", "light"],
    [344, 882, true, 3, "folded", "dark"],
    [884, 1104, true, 2.5, "unfolded", "light"],
  ]) {
    console.log(`— a Windows server (${tag})`);
    await mock("/__mock/reset?fresh=1");
    await c.rpc("mock_security", { windows: true });
    const p = await device();
    await p.viewport(w, h, mobile, dpr);
    await signIn(p, `Windows ${tag}`);
    await p.theme(theme);
    await p.waitFor(`document.querySelector('.setup-wizard')`, 6000);
    await sleep(500);
    await step(p, "Where");
    check(!(await shown(p, ".lan-firewall")), "the firewall note waits until the network switch is on");
    await p.click(".lan-toggle .toggle");
    await p.waitFor(`document.querySelector('.lan-firewall')`, 4000).catch(() => {});
    const fw = await text(p, ".lan-firewall");
    check(/Private networks/.test(fw) && /Allow/.test(fw) && /Private network/.test(fw), "network switch on a Windows server: tick Private networks, Allow, and how to make the network private", fw);
    check(await noSideScroll(p), `…without sideways scroll (${tag})`);
    await p.shot(`${SHOTS}/setup-windows-lan-${tag}-${theme}.png`);
    await step(p, "Done");
    await sleep(400);
    const safety = await text(p, ".safety-list");
    check(/It asks before it changes anything/.test(safety) && /Every command and every change to your files waits for your OK/.test(safety), "done: new chats ask before every change (“Ask me” by default on Windows)", safety);
    check(/No sandbox on this system/.test(safety), "…and says plainly there's no sandbox", safety);
    await p.shot(`${SHOTS}/setup-windows-done-${tag}-${theme}.png`);
    check(!p.errors.length, `no page errors (Windows server, ${tag})`, p.errors);
    await p.close();
  }
  await c.rpc("mock_security", { windows: false });

  // ── how it looks: the folded phone, the unfolded Fold and a desktop, dark and light ──
  await mock("/__mock/reset?fresh=1");
  for (const [w, h, mobile, dpr, tag] of [
    [344, 882, true, 3, "folded"],
    [884, 1104, true, 2.5, "unfolded"],
    [1440, 900, false, 1, "desktop"],
  ]) {
    for (const theme of ["dark", "light"]) {
      const p = await device();
      await p.viewport(w, h, mobile, dpr);
      await signIn(p, `Looks ${tag} ${theme}`);
      await p.theme(theme);
      await p.waitFor(`document.querySelector('.setup-wizard')`, 6000).catch(() => {});
      check(await shown(p, ".setup-wizard"), `${tag} ${theme}: the wizard opens on a first run`);
      await p.waitFor(`document.querySelectorAll('.setup-found li').length >= 1`, 6000).catch(() => {});
      await sleep(400);
      for (const label of ["Your AI", "Where", mobile ? "Devices" : "Your phone", "Done"]) {
        await step(p, label);
        check(await noSideScroll(p), `${tag} ${theme}: nothing sticks out sideways on “${label}”`);
        await p.shot(`${SHOTS}/setup-${tag}-${theme}-${label.replace(/\W+/g, "-").toLowerCase()}.png`);
      }
      check(!p.errors.length, `no page errors (${tag} ${theme})`, p.errors);
      await p.close();
    }
  }

  // ── onboarding with a pairing link (a browser with direct addresses) ──
  console.log("— onboarding: pasted link");
  await mock("/__mock/reset");
  const minted = (await c.rpc("create_pairing")).result;
  check(/^tessera:\/\/pair\?/.test(minted?.link ?? ""), "the mock mints a pairing link", minted);
  const secret = decodeURIComponent(/h=CF-Access-Client-Secret%3A([^&]+)/.exec(minted.link)?.[1] ?? "");
  {
    const p = await device();
    await p.viewport(1440, 900);
    await p.goto(APP + "?servers=1", 1800);
    await p.waitFor(`document.querySelector('.onboarding')`, 8000);
    check(await p.eval(`[...document.querySelectorAll('.onboarding button')].some(b => /Paste a pairing link/.test(b.textContent))`), "the address form offers “Paste a pairing link instead”");
    await p.clickText(".onboarding button", "Paste a pairing link");
    await sleep(300);
    check(await shown(p, ".link-input"), "…which shows the link field");
    await p.click(".link-input");
    await p.type("https://not-a-link.example");
    await sleep(200);
    check(/isn't a Tessera pairing link/.test(await text(p, ".onb-body .form-error")), "something else is told apart");
    check(await p.eval(`document.querySelector('.onb-body button[type=submit]').disabled`), "…and can't be paired with");
    await p.eval(`(() => { const t = document.querySelector('.link-input'); t.value = ''; t.dispatchEvent(new Event('input', { bubbles: true })); })()`);
    await p.click(".link-input");
    await p.type(minted.link);
    await sleep(300);
    check(await shown(p, ".link-summary"), "a pasted link is read back");
    const sum = await text(p, ".link-summary");
    check(/127\.0\.0\.1/.test(sum) && /tessera\.example\.com/.test(sum) && /Cloudflare Access keys included/.test(sum), "…its addresses (home and anywhere) and that the Access keys come along", sum);
    check(!(await p.eval(`document.body.innerText.includes(${JSON.stringify(secret)})`)) && secret.length > 10, "…never the secret itself");
    check(!(await shown(p, ".link-input")), "…and the pasted link itself is put away (it holds the secret)");
    check(await p.eval(`document.activeElement?.type === 'submit'`), "…with Pair & connect ready for Enter");
    await p.clickText(".link-summary .link-btn", "Paste another");
    await sleep(200);
    check((await shown(p, ".link-input")) && !(await shown(p, ".link-summary")), "“Paste another” brings the empty field back");
    await p.click(".link-input");
    await p.type(minted.link);
    await sleep(300);
    await p.shot(`${SHOTS}/onb-link-desktop.png`);
    await p.click(".onb-body button[type=submit]");
    await p.waitFor(`document.querySelector('.onb-done')`, 8000).catch(() => {});
    check(await shown(p, ".onb-done"), "Pair & connect pairs with what the link holds");
    const cfg = await p.eval(`JSON.parse(localStorage.getItem('tessera.clientConfig'))`);
    const srv = cfg?.servers?.[0];
    check(srv && srv.endpoints.length === 2 && srv.headers["CF-Access-Client-Secret"] === secret, "…saving both addresses and the Access headers", srv && { endpoints: srv.endpoints, headers: Object.keys(srv.headers) });
    await p.clickText(".onb-done button", "Open Tessera");
    await p.waitFor(`document.querySelector('.workspace')`, 10000).catch(() => {});
    check(await shown(p, ".workspace"), "…and the app opens connected");
    console.log("onboarding errors:", p.errors);
    check(!p.errors.length, "no page errors (onboarding)");

    // ── Add a server: a link pasted into the address field fills it all in ──
    const second = (await c.rpc("create_pairing")).result;
    await p.eval(`__mod('lib/ui.ts').then(m => m.setAddServerOpen({}))`);
    await p.waitFor(`document.querySelector('.server-form #addr-home')`, 4000);
    await p.click(".server-form #addr-home");
    await p.type(second.link);
    await sleep(300);
    const v = await p.eval(`({ home: document.querySelector('#addr-home').value, remote: document.querySelector('#addr-remote').value, code: document.querySelector('.server-form .code-input').value, adv: document.querySelector('.server-form .disclosure .count')?.textContent })`);
    check(/127\.0\.0\.1/.test(v.home) && /tessera\.example\.com/.test(v.remote) && v.code.replace(/\D/g, "") === second.code.replace(/\D/g, "") && v.adv === "2", "Add a server: a pasted link fills in the addresses, the code and the two headers", v);
    await p.shot(`${SHOTS}/addserver-link-desktop.png`);
    await p.clickText(".server-form button[type=submit]", "Pair");
    await sleep(1500);
    check(!(await shown(p, ".server-form")), "…and pairs");
    await p.close();
  }

  // ── a link pasted into the home address field of first-run onboarding ──
  {
    const third = (await c.rpc("create_pairing")).result;
    const p = await device();
    await p.viewport(375, 812, true, 2);
    await p.goto(APP + "?servers=1", 1800);
    await p.waitFor(`document.querySelector('#addr-home')`, 8000);
    await p.click("#addr-home");
    await p.type(third.link);
    await sleep(300);
    check((await shown(p, ".link-summary")) && !(await shown(p, ".link-input")), "a link pasted as the address switches to the link, read back (phone size)");
    check(await noSideScroll(p, ".onb-card"), "…without sideways scroll");
    await p.shot(`${SHOTS}/onb-link-phone.png`);
    await p.close();
  }

  // ── the scanner's viewfinder (the camera plugin stubbed) ──
  for (const [w, h, dpr, tag] of [
    [375, 812, 2, "phone"],
    [673, 841, 2.625, "fold"],
  ]) {
    console.log(`— scanner (${tag})`);
    const p = await device();
    await p.viewport(w, h, true, dpr);
    await p.goto(APP + "?servers=1", 1500);
    await p.waitFor(`document.querySelector('.onboarding')`, 8000);
    await p.eval(`(() => {
      window.__calls = [];
      window.__TAURI_INTERNALS__ = {
        transformCallback: (cb) => { const id = Math.floor(Math.random() * 1e9); window['_' + id] = cb; return id; },
        invoke: (cmd, args) => { window.__calls.push(cmd); if (cmd === 'plugin:barcode-scanner|scan') return new Promise((r) => (window.__scanResolve = r)); return Promise.resolve(null); },
      };
      window.__scanP = __mod('components/Scanner.tsx').then((m) => m.scanQr());
    })()`);
    // The scanner loads on demand: give it time on a busy machine.
    await p.waitFor(`document.querySelector('.scanner.live .scanner-frame')`, 8000).catch(() => {});
    await sleep(300);
    const g = await p.eval(`(() => { const f = document.querySelector('.scanner-frame').getBoundingClientRect(), hn = document.querySelector('.scanner-hint').getBoundingClientRect(), c = document.querySelector('.scanner-cancel').getBoundingClientRect(); return { cx: f.x + f.width / 2, cy: f.y + f.height / 2, w: f.width, h: f.height, hintBottom: hn.bottom, top: f.top, bottom: f.bottom, cancelTop: c.top, cancelBottom: c.bottom, vw: innerWidth, vh: innerHeight }; })()`);
    check(await p.eval(`document.documentElement.classList.contains('scanning') && !!document.querySelector('.scanner.live')`), "scanning: the viewfinder is up (live)");
    check(Math.abs(g.cx - g.vw / 2) < 1.5 && Math.abs(g.cy - g.vh / 2) < 1.5 && Math.abs(g.w - g.h) < 0.5 && g.w >= 200, "…a square cutout in the middle of the screen", g);
    check(g.hintBottom <= g.top - 12 && g.cancelTop >= g.bottom + 12 && g.cancelBottom <= g.vh, "…the hint above it, Cancel below it", g);
    check(await p.eval(`getComputedStyle(document.getElementById('root')).visibility === 'hidden' && getComputedStyle(document.documentElement).backgroundColor === 'rgba(0, 0, 0, 0)'`), "…the app hidden and the page see-through (the camera is behind it)");
    check(/tessera pair/.test(await text(p, ".scanner-hint")), "…the hint says where the code comes from");
    check((await p.eval(`window.__calls`)).includes("plugin:barcode-scanner|scan"), "…and the camera was asked for (windowed)");
    await p.shot(`${SHOTS}/scanner-${tag}.png`);
    await p.eval(`window.__scanResolve({ content: ' tessera://pair?v=1&c=12345678 ', format: 'QR_CODE' })`);
    await sleep(60);
    check(await shown(p, ".scanner.found"), "a code read: the viewfinder covers the camera at once");
    const got = await p.eval(`window.__scanP`);
    await sleep(300);
    check(got === "tessera://pair?v=1&c=12345678" && !(await p.eval(`document.documentElement.classList.contains('scanning')`)) && !(await shown(p, ".scanner-host")), "…returns the text and goes away", got);
    // Cancel: the camera stops, nothing is returned.
    await p.eval(`window.__calls = []; window.__scanP = __mod('components/Scanner.tsx').then((m) => m.scanQr()); 0`);
    await p.waitFor(`document.querySelector('.scanner-cancel')`, 8000).catch(() => {});
    await sleep(200);
    await p.click(".scanner-cancel");
    const none = await p.eval(`window.__scanP`);
    await sleep(300);
    check(none === null && (await p.eval(`window.__calls.includes('plugin:barcode-scanner|cancel')`)) && !(await shown(p, ".scanner-host")), "Cancel stops the camera and returns nothing");
    console.log(`${tag} errors:`, p.errors);
    check(!p.errors.length, `no page errors (${tag} scanner)`);
    await p.close();
  }
  // ── Windows desktop app: the server on this PC didn't start again when the app opened (after an update) ──
  // Said on the connect screen with where its log is, and "Start it again" tries once more.
  const WHY = "Tessera's server on this computer didn't start again within a minute. What it wrote is in C:\\Users\\me\\AppData\\Roaming\\tessera\\logs\\tessera.log.";
  const recoveryShell = `(() => {
    let n = 0;
    const cbs = {};
    window.__events = {};
    window.__calls = [];
    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
    window.__TAURI_INTERNALS__ = {
      metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main", windowLabel: "main" } },
      transformCallback(cb) { const id = ++n; cbs[id] = cb; return id; },
      async invoke(cmd, args) {
        window.__calls.push(cmd);
        switch (cmd) {
          case "servers_get": return { servers: [{ id: "srv_here", name: "This PC", endpoints: ["http://127.0.0.1:9"], headers: {}, token: "t", serverKey: "k", fingerprint: "1111 2222", deviceName: "App" }], active: "srv_here" };
          case "plugin:event|listen": (window.__events[args.event] ||= []).push(args.handler); return ++n;
          case "local_server_recovery": return ${JSON.stringify(WHY)};
          case "local_server_resume":
            if (!window.__resumes) { window.__resumes = 1; throw "Tessera's server on this computer didn't start again (the port is in use)."; }
            return null;
          default: return null;
        }
      },
    };
    window.__emit = (event, payload) => (window.__events[event] || []).forEach((h) => cbs[h]?.({ event, id: 0, payload }));
  })()`;
  for (const [w, h, theme] of [
    [1280, 860, "light"],
    [344, 780, "dark"],
  ]) {
    console.log(`— desktop app: the local server didn't start again (${w}px, ${theme})`);
    const p = await device();
    await p.viewport(w, h);
    await p.send("Page.addScriptToEvaluateOnNewDocument", { source: recoveryShell });
    await p.goto(APP, 1500);
    await p.theme(theme);
    await p.waitFor(`document.querySelector('.connect-recovery')`, 8000).catch(() => {});
    await sleep(300);
    check(await shown(p, ".connect-screen .connect-recovery"), `${w}px: the connect screen says the server here didn't start again`);
    const said = await text(p, ".connect-recovery .conn-error");
    check(said === WHY, "…why, and where its log is", said);
    check(await noSideScroll(p, ".connect-card"), `${w}px: nothing sticks out sideways`);
    await p.shot(`${SHOTS}/connect-recovery-${w}-${theme}.png`);
    await p.clickText(".connect-recovery button", "Start it again");
    await p.waitFor(`/port is in use/.test(document.querySelector('.connect-recovery .conn-error')?.textContent ?? '')`, 4000).catch(() => {});
    check(/port is in use/.test(await text(p, ".connect-recovery .conn-error")), "“Start it again” that fails says why it still didn't", await text(p, ".connect-recovery .conn-error"));
    await p.clickText(".connect-recovery button", "Start it again");
    await p.waitFor(`!document.querySelector('.connect-recovery')`, 4000).catch(() => {});
    check(!(await shown(p, ".connect-recovery")), "…and once it starts, the message goes");
    check((await p.eval(`window.__calls.filter((c) => c === 'local_server_resume').length`)) === 2, "…each press tried once");
    await p.eval(`window.__emit('tessera://local-server-recovery', ${JSON.stringify("It didn't start again (from the app).")})`);
    await sleep(200);
    check(/from the app/.test(await text(p, ".connect-recovery .conn-error")), "a failure the app reports later shows too");
    await p.eval(`window.__emit('tessera://local-server-recovery', null)`);
    await sleep(200);
    check(!(await shown(p, ".connect-recovery")), "…and goes when the app says it started");
    check(!p.errors.length, `no page errors (local server recovery, ${w}px)`, p.errors);
    await p.close();
  }
  c.close();
  finish();
});
