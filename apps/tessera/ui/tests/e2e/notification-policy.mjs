// Real notification settings UI with a synthetic native command bridge.
// No OS daemon, Android device or production server is used.
import { APP, MOCK, SHOTS, check, device, finish, mock, run, sleep, wsClient } from './cdp.mjs';

function shell(token) {
  return `(() => {
    const callbacks = {}, listeners = {}, pending = new Map(); let id = 0, rid = 0, ws;
    window.__notificationSettings = { enabled: true, approvals: true, finished: true, subagents: true, jobs: true, errors: true, previews: false, keepAlive: 'while_busy', quiet: false, quietFrom: '22:00', quietTo: '07:00', permission: 'granted', batteryOptimized: null, keepAliveAvailable: false };
    const emit = (event, payload) => (listeners[event] || []).forEach(id => callbacks[id]?.({ event, id: 1, payload }));
    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
    window.__TAURI_INTERNALS__ = { metadata: { currentWindow: { label: 'main' }, currentWebview: { label: 'main', windowLabel: 'main' } }, transformCallback(cb) { callbacks[++id] = cb; return id; }, async invoke(cmd, args = {}) {
      switch (cmd) {
        case 'plugin:event|listen': (listeners[args.event] ||= []).push(args.handler); return args.handler;
        case 'plugin:event|unlisten': return null;
        case 'servers_get': return { servers: [{ id: 'srv_mock', name: 'Mock', endpoints: [location.origin], headers: {}, token: '', serverKey: '', deviceName: 'Notification settings' }], active: 'srv_mock' };
        case 'servers_save': return null;
        case 'server_connect': if (!ws) { ws = new WebSocket(location.origin.replace(/^http/, 'ws') + '/api/ws?token=' + encodeURIComponent(${JSON.stringify(token)})); ws.onopen = () => emit('tessera://status', { state: 'connected', serverId: 'srv_mock', endpoint: location.origin, error: null, encrypted: true }); ws.onmessage = e => { const m = JSON.parse(e.data); if (m.event) emit('tessera://event', m.event); else if (pending.has(m.id)) { const p = pending.get(m.id); pending.delete(m.id); m.error ? p.reject(m.error) : p.resolve(m.result ?? null); } }; } return null;
        case 'rpc': return new Promise((resolve, reject) => { const id = ++rid; pending.set(id, { resolve, reject }); ws.send(JSON.stringify({ id, method: args.request.method, params: args.request.params })); });
        case 'notifications_get': return { ...window.__notificationSettings };
        case 'notifications_set': Object.assign(window.__notificationSettings, args.settings); return { ...window.__notificationSettings };
        case 'app_resumed': case 'server_disconnect': case 'notifications_seen': case 'notifications_take_opened': return null;
        default: throw 'command ' + cmd + ' not found';
      }
    } };
  })()`;
}

run(async () => {
  await mock('/__mock/reset'); await mock('/__mock/mosaics/reset');
  const peer = await wsClient('Notification settings fixture');
  const token = (await (await fetch(MOCK + '/api/pair', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ code: '123456', device_name: 'Notification settings' }) })).json()).token;
  const p = await device();
  await p.send('Page.addScriptToEvaluateOnNewDocument', { source: shell(token) });
  await p.goto(APP); await p.waitFor(`document.querySelector('.workspace')`, 15000);
  await p.eval(`__mod('lib/ui.ts').then(m => m.setSettingsTab('notifications'))`);
  await p.waitFor(`document.querySelector('select[aria-label="Notifications"]')`);
  check(await p.eval(`document.querySelector('select[aria-label="Notifications"]').value === 'on'`), 'existing enabled/finished settings migrate to On');
  check(await p.eval(`JSON.stringify([...document.querySelector('select[aria-label="Notifications"]').options].map(o => o.text)) === JSON.stringify(['On', "Only when I'm needed", 'Off'])`), 'three plain choices and no per-tool/helper category knobs');
  for (const mode of ['needed', 'off', 'on']) {
    await p.eval(`(() => { const e = document.querySelector('select[aria-label="Notifications"]'); e.value = '${mode}'; e.dispatchEvent(new Event('change', { bubbles: true })); })()`);
    await sleep(150);
    check(await p.eval(`window.__notificationSettings.enabled === ${mode !== 'off'} && window.__notificationSettings.finished === ${mode === 'on'} && !window.__notificationSettings.subagents && window.__notificationSettings.approvals && window.__notificationSettings.errors`), `${mode} saves existing compatible settings and disables old helper preference`);
  }
  for (const [label, width, height] of [['desktop', 1440, 900], ['phone', 390, 844], ['cover', 344, 882], ['fold', 880, 1000]]) {
    await p.viewport(width, height, width < 1000, width < 1000 ? 2 : 1);
    for (const theme of ['light', 'dark']) {
      await p.theme(theme); await sleep(400);
      check(await p.eval(`(() => { const e = document.querySelector('select[aria-label="Notifications"]'); e.scrollIntoView({ block: 'center' }); const r = e.getBoundingClientRect(); return r.left >= 0 && r.right <= innerWidth + 1 && r.height >= 40; })()`), `${label} ${theme} choice fits and has a forty-pixel target`);
      check(await p.eval(`!document.querySelector('.settings').textContent.includes("Helpers' reports")`), `${label} ${theme} has no helper/progress notification choice`);
      await p.shot(`${SHOTS}/notification-policy-${label}-${theme}.png`);
    }
  }
  await peer.close(); await p.close(); await finish();
});
