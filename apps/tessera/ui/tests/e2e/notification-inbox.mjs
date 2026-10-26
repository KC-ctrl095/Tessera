// The notification center: everything that notified the user in one list (the bell), a tap opens
// its chat; read, unread, clear; scheduled runs and their schedules' ends; who sees what; an older
// server. Desktop, folded phone (344 px) and unfolded Fold, dark and light.
import { APP, SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";
import { notificationRaces } from "./notification-races.mjs";

const SID = "ses_7d3a91e5c4b2";
const wait = async (fn, ms = 8000) => {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    const v = await fn();
    if (v) return v;
    await sleep(80);
  }
  return null;
};
const inbox = (p, js) => p.eval(`__mod('lib/notificationInbox.ts').then(m => { ${js} })`);
const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
/** The first visible, unobscured element: its centre and size. */
const hitBox = (p, selector) =>
  p.eval(`(() => {
  for (const el of document.querySelectorAll(${JSON.stringify(selector)})) {
    const r = el.getBoundingClientRect(), cx = r.x + r.width / 2, cy = r.y + r.height / 2;
    const hit = document.elementFromPoint(cx, cy);
    if (r.width > 0 && r.height > 0 && hit && el.contains(hit)) return { cx, cy, w: r.width, h: r.height };
  }
  return null;
})()`);
/** The bell: in the top bar, or (folded phone) in the sidebar. */
const findBell = async (p, touch = false) => {
  await sleep(200);
  if (!(await hitBox(p, ".notification-bell"))) {
    const menu = await wait(() => hitBox(p, '[aria-label="Open sidebar"]'));
    if (!menu) throw new Error("no way to the sidebar");
    if (touch) await p.tap(menu.cx, menu.cy);
    else await p.mouse(menu.cx, menu.cy);
  }
  const bell = await wait(() => hitBox(p, ".notification-bell"));
  if (!bell) throw new Error("no visible bell");
  return bell;
};
const openInbox = async (p, touch = false) => {
  const bell = await findBell(p, touch);
  if (touch) await p.tap(bell.cx, bell.cy);
  else await p.mouse(bell.cx, bell.cy);
  await p.waitFor(`!!document.querySelector('.notification-inbox')`);
  await wait(() => p.eval(`!document.querySelector('.notification-inbox .inbox-empty[role=status]')`));
};
const close = (p) => p.click(".notification-inbox .modal-close");
const item = (p, id) => p.eval(`(() => { const li = document.querySelector('[data-notification="${id}"]'); return li && { kind: li.dataset.kind, unread: li.classList.contains('unread'), text: li.textContent }; })()`);

run(async () => {
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  const probe = await wsClient("Inbox fixture");
  const p = await device();
  await p.viewport(1440, 900);
  await signIn(p, "Inbox desktop");
  await sleep(500);
  check(!(await p.eval(`!!document.querySelector('.notification-bell')`)), "a newcomer sees no bell: nothing has notified them yet");
  await notificationRaces(p);

  // A modern browser negotiates automatically; an untouched old connection
  // still cannot mistake an automatic item for an explicit agent message.
  const automatic = (await probe.rpc('mock_inbox', { add: { kind: 'error', title: 'Synthetic failed run', body: 'PRIVATE_AUTOMATIC_DETAIL' } })).result;
  check(await wait(() => p.eval(`document.body.textContent.includes('Synthetic failed run')`)), 'a modern browser negotiates and receives typed automatic alerts');
  check(!probe.events.some(e => e.type === 'notification' && e.notification.id === automatic.id), 'an unnegotiated legacy connection receives no automatic payload');
  const oldPending = (await probe.rpc('pending_notifications')).result;
  check(!oldPending.new.some(n => n.id === automatic.id), 'legacy pending route also excludes automatic content');
  await probe.rpc('cancel_notification', { id: automatic.id });
  await wait(() => p.eval(`!document.querySelector('.notification-bell')`));

  // Browsers have no native preview/category settings. Their OS surface
  // uses generic automatic activity, never job names or report details.
  await p.eval(`(() => {
    window.__savedNotification = window.Notification;
    window.__automaticPosts = []; window.__closedNotifications = [];
    window.Notification = class { static permission = 'granted'; constructor(title, options) { this.tag = options.tag; window.__automaticPosts.push({ title, ...options }); } close() { window.__closedNotifications.push(this.tag); } };
    Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => 'hidden' });
  })()`);
  await p.eval(`__mod('lib/alerts.ts').then(m => m.setBrowserNotifications(true))`);
  const generic = (await probe.rpc('mock_inbox', { add: { kind: 'error', title: 'PRIVATE_JOB_NAME', body: 'PRIVATE_REPORT_DETAIL' } })).result;
  const osPost = await wait(() => p.eval(`window.__automaticPosts.find(n => n.tag === '${generic.source_key || `n${generic.id}`}')`));
  check(osPost?.title === 'Tessera' && osPost?.body === 'New activity. Open Tessera for details.', 'browser OS automatic alerts contain no private names/details', osPost);
  const privateMessage = (await probe.rpc('mock_inbox', { add: { private: true, title: 'SECRET_PRIVATE_TITLE', body: 'SECRET_PRIVATE_BODY', from: 'SECRET_PRIVATE_SUBTITLE' } })).result;
  check(privateMessage.private && privateMessage.title === 'Tessera' && !privateMessage.body && !privateMessage.from, 'private wire payload is marked and redacted even for an explicit message', privateMessage);
  const privatePost = await wait(() => p.eval(`window.__automaticPosts.find(n => n.tag === 'n${privateMessage.id}')`));
  check(privatePost?.title === 'Tessera' && privatePost?.body === 'New activity. Open Tessera for details.', 'private browser OS payload is generic without session hydration', privatePost);
  const explicit = (await probe.rpc('mock_inbox', { add: { title: 'SECRET_EXPLICIT_TITLE', body: 'SECRET_EXPLICIT_BODY', from: 'SECRET_EXPLICIT_SENDER' } })).result;
  const explicitPost = await wait(() => p.eval(`window.__automaticPosts.find(n => n.tag === '${explicit.source_key || `n${explicit.id}`}')`));
  check(explicitPost?.title === 'Tessera' && explicitPost?.body === 'New activity. Open Tessera for details.', 'browser OS explicit alerts are generic too', explicitPost);
  const source1 = (await probe.rpc('mock_inbox', { add: { title: 'First job result', source_key: 'job_fixture', urgent: true } })).result;
  const source2 = (await probe.rpc('mock_inbox', { add: { title: 'Updated job result', source_key: 'job_fixture', urgent: true } })).result;
  check(await wait(() => p.eval(`window.__automaticPosts.filter(n => n.tag === 'job_fixture').length === 2 && window.__closedNotifications.includes('job_fixture')`)), 'browser job alerts replace one stable card rather than stacking event ids');
  await probe.rpc('seen_notifications', { ids: [source1.id] });
  await sleep(150);
  check(await p.eval(`window.__closedNotifications.filter(tag => tag === 'job_fixture').length === 1`), 'stale read does not close the newer browser replacement');
  await probe.rpc('seen_notifications', { ids: [source2.id] });
  check(await wait(() => p.eval(`window.__closedNotifications.filter(tag => tag === 'job_fixture').length >= 2`)), 'reading elsewhere closes the actual browser OS notification');
  await probe.rpc('cancel_notification', { id: source1.id });
  await probe.rpc('cancel_notification', { id: source2.id });
  await probe.rpc('cancel_notification', { id: explicit.id });
  await probe.rpc('cancel_notification', { id: privateMessage.id });
  await p.eval(`__mod('lib/alerts.ts').then(m => m.setBrowserNotifications(false))`);
  await p.eval(`(() => { delete document.visibilityState; window.Notification = window.__savedNotification; delete window.__savedNotification; })()`);
  await probe.rpc('cancel_notification', { id: generic.id });
  await wait(() => p.eval(`!document.querySelector('.notification-bell')`));

  // Levels, exact repeats and one-tap source controls use the same inbox.
  const levelFirst = (await probe.rpc('mock_inbox', { add: { kind: 'message', title: 'Monitor report', body: 'Nothing changed', source: 'bot', source_key: 'bot_monitor', from: 'Monitor' } })).result;
  await probe.rpc('mock_inbox', { add: { kind: 'message', title: 'Monitor report', body: 'Nothing changed', source: 'bot', source_key: 'bot_monitor', from: 'Monitor' } });
  await inbox(p, `return m.refreshInbox()`);
  await openInbox(p);
  check(await wait(async () => (await item(p, levelFirst.id))?.text.includes('×2') && (await item(p, levelFirst.id))?.text.includes('Digest · bot')), 'one inbox shows source, level and exact repeat count');
  await p.click(`[data-notification="${levelFirst.id}"] .inbox-source-actions button:first-child`);
  check(await wait(async () => (await probe.rpc('get_notification_policy')).result.sources.some(s => s.key === 'bot_monitor' && s.level === 'silent')), 'one tap mutes this source on the server');
  const muted = (await probe.rpc('mock_inbox', { add: { kind: 'message', title: 'A new report', body: 'Different text', source: 'bot', source_key: 'bot_monitor', from: 'Monitor' } })).result;
  check(muted.level === 'silent' && muted.reason === 'you muted this source', 'source mute applies to new text');
  await probe.rpc('set_notification_source', { key: 'bot_monitor', level: null });
  const page = (await probe.rpc('mock_inbox', { add: { title: 'Owner requested result', urgent: true, source: 'bot', source_key: 'bot_monitor', from: 'Monitor' } })).result;
  await inbox(p, `return m.refreshInbox()`);
  await p.click(`[data-notification="${page.id}"] .inbox-source-actions button:last-child`);
  check(await wait(async () => (await probe.rpc('get_notification_policy')).result.sources.some(s => s.key === 'bot_monitor' && s.level === 'digest')), 'Less like this lowers Page to Digest');
  await probe.rpc('set_notification_source', { key: 'bot_monitor', level: null });
  const digest = (await probe.rpc('mock_digest')).result;
  check(digest?.kind === 'digest' && digest.level === 'digest' && !digest.quiet, 'one non-buzzing digest summary uses the normal delivery path');
  await inbox(p, `return m.refreshInbox()`);
  check((await item(p, digest.id))?.text.includes('Digest · system'), 'digest summary is visible in the same inbox');
  check((await probe.rpc('mock_digest')).result === null, 'an empty digest sends nothing');
  await close(p);
  for (const n of (await probe.rpc('list_notification_inbox')).result) await probe.rpc('cancel_notification', { id: n.id });

  // Routine success is in the task run log, not another bell item.
  const jobs = (await probe.rpc("list_jobs")).result;
  const job = jobs[0];
  await probe.rpc("run_job", { id: job.id });
  const runSid = (await probe.rpc("list_jobs")).result.find((x) => x.id === job.id).last_session;
  await wait(async () => (await probe.rpc('list_jobs')).result.find(x => x.id === job.id).last_status === 'ok', 20000);
  check(!(await probe.rpc('list_notification_inbox')).result.some(n => n.session_id === runSid), 'routine successful runs do not fill the bell');
  // A useful quiet job result, used below to exercise history and opening.
  const finished = (await probe.rpc('mock_inbox', { add: { kind: 'job', title: 'Scheduled result', session_id: runSid, quiet: true } })).result;
  check(await wait(() => inbox(p, `return m.inboxUnread() === 1`)), "the bell counts it");
  check(!(await p.eval(`[...document.querySelectorAll('.toast')].some((t) => t.textContent.includes('Scheduled task finished'))`)), "…without a toast");
  const j = (await probe.rpc("list_jobs")).result.find((x) => x.id === job.id);
  check(j.last_status === "ok" && j.runs === 1 && !!j.last_end, "the job says how its last run went", j);
  const runs = (await probe.rpc("list_job_runs", { id: job.id })).result;
  check(runs.length === 1 && runs[0].outcome === "ok" && runs[0].session_id === runSid, "the run is logged with its chat", runs);
  const label = await p.eval(`document.querySelector('.notification-bell')?.getAttribute('aria-label')`);
  check(label === "Notifications, 1 unread", "the bell says how many are new", label);

  // Ctrl+K and Ctrl+, take the place of the open center (never beneath it).
  await openInbox(p);
  for (const [key, code, name, selector] of [["k", "KeyK", "command palette", ".palette"], [",", "Comma", "Settings", ".settings"]]) {
    await p.key(key, code, 2);
    check(await wait(() => p.eval(`!document.querySelector('.notification-inbox') && !!document.querySelector('${selector}')`)), `the ${name} takes the notification center's place`);
    await p.key("Escape");
    await sleep(250);
    await inbox(p, `m.setInboxOpen(false)`);
    await openInbox(p);
  }
  check((await item(p, finished.id))?.kind === "job", "the run's item shows as a scheduled task");

  // A reminder still to come, then delivered (unread).
  const reminder = (await probe.rpc("mock_notify", { session_id: SID, title: "Review the endpoint", body: "Before the release.", delay: 2 })).result;
  check(await wait(async () => (await item(p, reminder.id))?.text.includes("Reminder, still to come")), "a reminder still to come says so");
  check(await wait(async () => (await item(p, reminder.id))?.unread, 6000), "…and is unread once it arrives");
  // Things the server noticed itself: an approval that waited, helpers, a machine, an error.
  const added = {};
  for (const [kind, title] of [["approval", "Needs your approval"], ["helper", "2 helpers reported back"], ["machine", "Terminals turned on"], ["error", "Stopped by an error"]]) {
    added[kind] = (await probe.rpc("mock_inbox", { add: { kind, title, body: `${kind} body`, quiet: kind !== "machine", session_id: kind === "machine" ? null : SID } })).result;
  }
  check(await wait(async () => (await item(p, added.error.id))?.kind === "error"), "every kind is listed with its own icon");
  check((await p.eval(`document.querySelectorAll('.inbox-list li').length`)) === 6, "all six are listed");

  // Desktop, folded phone, unfolded Fold; dark and light: it fits, nothing hides its controls.
  for (const [name, width, height] of [["desktop", 1440, 900], ["phone", 344, 882], ["fold", 884, 1104]]) {
    await p.viewport(width, height, name !== "desktop", 1);
    for (const theme of ["dark", "light"]) {
      await p.theme(theme);
      await sleep(250);
      check(await p.eval(`(() => { const x = document.querySelector('.notification-inbox').getBoundingClientRect(); return x.left >= -1 && x.right <= innerWidth + 1 && x.top >= -1 && x.bottom <= innerHeight + 1; })()`), `${name} ${theme}: it fits the screen`);
      check(await p.eval(`document.querySelector('.inbox-content').scrollWidth <= document.querySelector('.inbox-content').clientWidth + 1`), `${name} ${theme}: nothing sideways`);
      check(await p.eval(`(() => { const b = document.querySelector('.notification-inbox .modal-close').getBoundingClientRect(); return !!document.elementFromPoint(b.x + b.width / 2, b.y + b.height / 2)?.closest('.notification-inbox'); })()`), `${name} ${theme}: its close button isn't covered`);
      const contrast = await p.eval(`(() => { const s = getComputedStyle(document.querySelector('.inbox-item-head strong')); return s.color !== getComputedStyle(document.querySelector('.notification-inbox')).backgroundColor; })()`);
      check(contrast, `${name} ${theme}: titles stand out`);
      // Leaving the phone layout starts the desktop modal's 340ms spring.
      // Check real geometry once it settles, not at the intermediate scale.
      const targets = `(() => { const controls = [...document.querySelectorAll('.inbox-filters button, .inbox-actions .btn, .inbox-mark, .inbox-source-actions button')]; return controls.length > 0 && controls.every(el => { const r = el.getBoundingClientRect(); return r.width >= 40 && r.height >= 40; }); })()`;
      const targetOK = await wait(() => p.eval(targets));
      const dimensions = targetOK ? undefined : await p.eval(`[...document.querySelectorAll('.inbox-filters button, .inbox-actions .btn, .inbox-mark, .inbox-source-actions button')].map(el => { const r = el.getBoundingClientRect(); return [el.textContent, r.width, r.height]; })`);
      check(targetOK, `${name} ${theme}: all inbox controls have at least 40px targets`, dimensions);
      await p.shot(`${SHOTS}/inbox-${name}-${theme}.png`);
    }
  }
  await p.theme("dark");
  await p.viewport(1440, 900);
  await sleep(250);

  // A tap opens its chat where it already is, and marks it read everywhere.
  const target = await ws(p, `m.newWorkspace(); const tile = m.openSession('${runSid}'); m.switchWorkspace(0); return tile;`);
  await p.click(`[data-notification="${finished.id}"] .inbox-item`);
  check(await wait(() => ws(p, `return m.paneSession(m.workspace.focused())?.info.id === '${runSid}'`)), "a tap opens the run's chat");
  check(await ws(p, `return m.workspace.focused() === '${target}' && m.workspace.active() !== 0`), "…in the tile that already shows it");
  check(await wait(async () => (await probe.rpc("list_notification_inbox")).result.find((n) => n.id === finished.id)?.seen), "…and it's read on the server");
  await ws(p, `m.switchWorkspace(0)`);
  await openInbox(p);
  // Read one, then the rest; clear the read ones.
  await p.click(`[data-notification="${added.machine.id}"] .inbox-mark`);
  check(await wait(async () => !(await item(p, added.machine.id))?.unread), "✓ marks one read");
  await p.click(".inbox-filters button:nth-child(2)");
  check((await p.eval(`[...document.querySelectorAll('.inbox-list li')].every((li) => li.classList.contains('unread'))`)), "Unread shows only new ones");
  await p.click(".inbox-read-all");
  check(await wait(() => p.eval(`document.querySelector('.inbox-empty')?.textContent.includes('Nothing new')`)), "Mark all read empties Unread");
  check(await wait(() => inbox(p, `return m.inboxUnread() === 0`)), "…and the count");
  await p.click(".inbox-filters button:first-child");
  const before = await p.eval(`document.querySelectorAll('.inbox-list li').length`);
  await p.click(`[data-notification="${reminder.id}"] .inbox-mark`);
  check(await wait(async () => !(await item(p, reminder.id)) && (await p.eval(`document.querySelectorAll('.inbox-list li').length`)) === before - 1), "× clears one read item");
  await p.click(".inbox-clear");
  check(await wait(() => p.eval(`!document.querySelector('.inbox-list li')`)), "Clear read clears the rest");
  check((await probe.rpc("list_notification_inbox")).result.length === 0, "…on the server too");
  await close(p);
  check(await wait(() => p.eval(`!document.querySelector('.notification-bell')`)), "an empty center: the bell goes again");

  // The count follows with this device's system notifications off, and quiet ones never toast.
  const me = (await probe.rpc("list_devices")).result.find((d) => d.name === "Inbox desktop");
  await probe.rpc("set_device_notify", { id: me.id, notify: false });
  await probe.rpc("mock_notify", { title: "Review one" });
  await probe.rpc("mock_inbox", { add: { kind: "job", title: "Scheduled task finished", body: "quiet run", quiet: true } });
  check(await wait(() => inbox(p, `return m.inboxUnread() === 2`)), "the count follows with system notifications off here");
  check(!(await p.eval(`[...document.querySelectorAll('.toast')].some((t) => /Review one|quiet run/.test(t.textContent))`)), "…and nothing pops up for it");
  await probe.rpc("set_device_notify", { id: me.id, notify: true });

  // Phone and Fold: the bell is reachable by touch and big enough for a finger.
  for (const [name, width, height] of [["phone", 344, 882], ["fold", 884, 1104]]) {
    await p.viewport(width, height, true, 1);
    await sleep(300);
    await ws(p, `m.openSession('${SID}')`);
    if (name === "phone") {
      const dot = await p.eval(`(() => { const b = [...document.querySelectorAll('.head-menu')].find((x) => x.getClientRects().length); return b ? getComputedStyle(b, '::after').content : null; })()`);
      check(dot && dot !== "none", "phone: the sidebar button shows a dot while something is unread", dot);
    }
    const bell = await findBell(p, true);
    check(bell.w >= 30 && bell.h >= 30, `${name}: the bell is a real touch target`, bell);
    if (name === "phone") {
      check(bell.w >= 40 && bell.h >= 40, "phone: 40 px or more in the sidebar", bell);
      await p.shot(`${SHOTS}/inbox-phone-bell.png`);
    }
    await p.tap(bell.cx, bell.cy);
    await p.waitFor(`!!document.querySelector('.notification-inbox .inbox-item')`);
    await p.shot(`${SHOTS}/inbox-${name}-touch.png`);
    await close(p);
  }
  await p.viewport(1440, 900);
  await sleep(250);

  // A schedule that ends by itself shows why.
  const once = (await probe.rpc("save_job", { job: { name: "One-off check", schedule: "@every 1h", prompt: "Check once", cwd: "", enabled: true, permission: "auto", max_runs: 1 } })).result;
  await probe.rpc("run_job", { id: once.id });
  const ended = await wait(async () => (await probe.rpc("list_jobs")).result.find((x) => x.id === once.id && x.ended));
  check(ended && !ended.enabled && ended.ended === "it ran once, as set", "a one-off ends its own schedule", ended);
  check(await wait(async () => (await probe.rpc("list_notification_inbox")).result.some((n) => n.title === "Scheduled task ended")), "…and says so in the center");
  await p.click(`.sidebar:not(.drawer) .sb-tab[id$="-jobs"]`);
  await sleep(300);
  check(await wait(() => p.eval(`[...document.querySelectorAll('.sidebar .srow')].some((r) => r.textContent.includes('One-off check') && r.textContent.includes('ended'))`)), "the sidebar's row says ended, not paused");
  await p.clickText(".sidebar .srow-main", "One-off check");
  const view = await wait(() => p.eval(`document.querySelector('.job-editor-view .job-state')?.textContent ?? ''`));
  check(/Run 1 of 1/.test(view ?? "") && /Ended: it ran once, as set/.test(view ?? "") && /Last .*(finished|running)/.test(view ?? ""), "its own view says how the last run went, its limit and why it ended", view);
  check(await p.eval(`!!document.querySelector('.job-editor-view .job-limits input[type=number]')`), "…and its editor offers the limits");
  await p.shot(`${SHOTS}/inbox-job-ended.png`);
  await p.key("Escape");
  await sleep(300);
  const refused = await probe.rpc("save_job", { job: { ...ended, enabled: true } });
  check(refused.error && /max_runs/.test(refused.error.message), "turning it on again needs a higher limit", refused.error);
  const kept = (await probe.rpc("save_job", { job: { name: ended.name, schedule: ended.schedule, prompt: ended.prompt, cwd: ended.cwd, enabled: false, permission: "auto", id: ended.id } })).result;
  check(kept.max_runs === 1, "an older app leaving the limit out keeps it", kept);

  // Continue the same run after a user Stop: it is working again, not
  // still "stopped"; no concurrent Run now and no second history row.
  await p.click(`.sidebar:not(.drawer) .sb-tab[id$="-jobs"]`);
  await sleep(300);
  await p.clickText(".sidebar .srow-main", job.name);
  const lastStatus = `document.querySelector('.job-editor-view .job-state .job-last span')?.textContent.trim()`;
  await p.waitFor(`${lastStatus} === 'finished'`);
  const live = (await probe.rpc("run_job", { id: job.id })).result;
  const overlap = await probe.rpc("run_job", { id: job.id });
  check(!!overlap.error && overlap.error.message.includes("still working"), "Run now cannot overlap the working run");
  await probe.rpc("abort", { session_id: live.id });
  await p.waitFor(`${lastStatus} === 'stopped'`);
  await probe.rpc("send", { session_id: live.id, content: [{ type: "text", text: "Continue that check." }], delivery: "follow_up", attachments: [] });
  await p.waitFor(`${lastStatus} === 'running'`);
  const workingRuns = (await probe.rpc("list_job_runs", { id: job.id })).result;
  check(workingRuns[0]?.session_id === live.id && workingRuns[0]?.outcome === "running" && !workingRuns[0]?.ended_at, "continuing clears the run's end, not its original start");
  check((await probe.rpc("list_jobs")).result.find((j) => j.id === job.id)?.runs === 2, "continuing does not count another scheduled run");
  await probe.rpc("abort", { session_id: live.id });
  const stoppedRuns = (await probe.rpc("list_job_runs", { id: job.id })).result;
  check(stoppedRuns.length === 2 && stoppedRuns[0]?.outcome === "stopped", "continued run keeps one history row");
  await p.key("Escape");
  await sleep(250);

  // Who sees what: a private chat's items are its device's alone.
  const privateChat = await p.eval(`__mod('lib/store.ts').then(m => m.api.createSession({ private: true }))`);
  const secret = await p.eval(`__mod('lib/store.ts').then(m => m.call({ method: 'send_notification', params: { session_id: '${privateChat.id}', title: 'Personal reminder', body: 'Private message', delay_secs: 0 } }))`);
  await openInbox(p);
  check(await wait(async () => !!(await item(p, secret.id))), "its own device sees a private chat's");
  await close(p);
  const other = await device();
  await other.viewport(344, 882, true, 1);
  await signIn(other, "Other phone");
  await ws(other, `m.openSession('${SID}')`);
  await openInbox(other, true);
  check(!(await other.eval(`document.querySelector('.notification-inbox').textContent.includes('Private message')`)), "another device doesn't");
  const denied = await other.eval(`__mod('lib/store.ts').then(async m => { try { await m.call({ method: 'seen_notifications', params: { ids: [${secret.id}] } }); return 'allowed'; } catch (e) { return e.code; } })`);
  check(denied === "not_found", "…nor can act on it by id", denied);
  await close(other);

  // A chat gone after the list came: says so, opens nothing.
  const temp = (await probe.rpc("create_session", { title: "Temporary task" })).result;
  const stale = (await probe.rpc("mock_notify", { session_id: temp.id, title: "Open temporary task" })).result;
  await openInbox(p);
  await wait(async () => !!(await item(p, stale.id)));
  await probe.rpc("mock_inbox", { missing: temp.id });
  await p.click(`[data-notification="${stale.id}"] .inbox-item`);
  check(await wait(() => p.eval(`document.querySelector('.inbox-error')?.textContent.includes("isn't here any more")`)), "a chat that's gone: it says so");
  await p.shot(`${SHOTS}/inbox-gone-chat.png`);
  check((await inbox(p, `return m.inboxItems().length`)) > 0, "history before signing out");
  await p.eval(`__mod('lib/app.ts').then(async m => { const s = m.activeServer(); await m.removeServer(s.id); })`);
  check(await inbox(p, `return m.inboxItems().length === 0 && !m.inboxOpen()`), "signing out clears it and closes it");
  check(!(await p.eval(`document.body.textContent.includes('Private message')`)), "…leaving nothing private on screen");


  // A read/clear based on device B's old content cannot eat revision B2.
  const oldRevision = (await probe.rpc('mock_inbox', { add: { session_id: runSid, kind: 'helper', quiet: true, title: 'Old report', replace: 'same' } })).result;
  const newRevision = (await probe.rpc('mock_inbox', { add: { session_id: runSid, kind: 'helper', quiet: true, title: 'New report', replace: 'same' } })).result;
  check(oldRevision.id !== newRevision.id, 'changed activity has a new revision id');
  await probe.rpc('seen_notifications', { ids: [oldRevision.id], session_id: null });
  await probe.rpc('clear_notifications', { ids: [oldRevision.id] });
  const revision = (await probe.rpc('list_notification_inbox')).result.find((n) => n.id === newRevision.id);
  check(!!revision && !revision.seen && revision.title === 'New report', 'a stale read/clear leaves the new revision unread');

  await p.send('Emulation.setTimezoneOverride', { timezoneId: 'America/Chicago' });
  const dateLimits = await p.eval(`__mod('components/Jobs.tsx').then(m => ['2026-03-08', '2026-11-01'].map(d => new Date(m.nextLocalMidnight(d)).toISOString()))`);
  check(dateLimits[0] === '2026-03-09T05:00:00.000Z' && dateLimits[1] === '2026-11-02T06:00:00.000Z', 'date picker ends at next local midnight across 23h/25h days', dateLimits);
  await p.send('Emulation.setTimezoneOverride', { timezoneId: '' });
  // An older server: no center (no bell), everything else as before.
  await probe.rpc("mock_inbox", { legacy: true });
  const legacy = await device();
  await legacy.viewport(1440, 900);
  await signIn(legacy, "Older server");
  await sleep(600);
  check(await inbox(legacy, `return m.inboxStatus() === 'unsupported'`), "an older server: the center knows it isn't there");
  check(!(await legacy.eval(`!!document.querySelector('.notification-bell')`)), "…and shows no bell");
  await probe.rpc("mock_notify", { title: "Old server reminder", body: "Toasts still work." });
  check(await wait(() => legacy.eval(`document.body.textContent.includes('Old server reminder')`)), "…and its messages still pop up");
  await probe.rpc("mock_inbox", { legacy: false });
  check(p.errors.length === 0 && other.errors.length === 0 && legacy.errors.length === 0, "no page errors", [...p.errors, ...other.errors, ...legacy.errors]);
  probe.close();
  await finish(p, other, legacy);
});
