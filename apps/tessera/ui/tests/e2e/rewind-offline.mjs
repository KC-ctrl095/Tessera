// An offline device must replace history after a rewind even when every old
// sequence number has been reused before it reconnects.
import { isDeepStrictEqual } from 'node:util';
import { check, device, mock, signIn, sleep, wsClient } from './cdp.mjs';

export async function offlineRewindCases() {
  for (const [width, theme] of [[344, 'dark'], [344, 'light'], [673, 'dark'], [673, 'light'], [1440, 'dark'], [1440, 'light']]) {
    await mock('/__mock/reset');
    await mock('/__mock/mosaics/reset');
    const control = await wsClient('Rewind control');
    const info = (await control.rpc('mock_session', { title: 'Offline rewind', entries: [
      { kind: 'user', text: 'Keep this question' }, { kind: 'assistant', text: 'Keep this answer' },
      { kind: 'user', text: 'Remove this old question' }, { kind: 'assistant', text: 'Remove this old answer' },
    ] })).result;
    const p = await device();
    await p.viewport(width, 900, width < 700, width < 700 ? 2 : 1);
    await signIn(p, `Offline rewind ${width} ${theme}`);
    await p.theme(theme);
    await p.eval(`__mod('lib/workspace.ts').then(w => w.openSession(${JSON.stringify(info.id)}, { pane: w.workspace.focused() }))`);
    await p.waitFor(`__mod('lib/store.ts').then(m => m.state.sessions[${JSON.stringify(info.id)}]?.loaded)`, 10000);
    const requests = [];
    await p.send('Network.enable');
    p.conn.on(p.sid, 'Network.webSocketFrameSent', (frame) => {
      try {
        const message = JSON.parse(frame.response.payloadData);
        if (['get_session','get_session_page'].includes(message.method) && message.params?.session_id === info.id) requests.push(message.params.since_seq ?? null);
      } catch {}
    });
    await p.eval(`(async () => {
      const w = await __mod('lib/workspace.ts');
      w.setDraft(w.workspace.focused(), 'Keep my local draft');
      await (await __mod('lib/backend.ts')).backend.disconnect();
    })()`);
    await p.waitFor(`__mod('lib/store.ts').then(m => m.state.conn.state === 'disconnected')`, 5000);
    await sleep(250); // Let the mock acknowledge the socket close.
    await control.rpc('rewind', { session_id: info.id, to_seq: 2, files: false, conversation: true, dry_run: false });
    await control.rpc('send', { session_id: info.id, content: [{ type: 'text', text: 'Replacement question' }], delivery: 'auto', attachments: [] });
    let snapshot;
    for (let i = 0; i < 100; i++) {
      snapshot = (await control.rpc('get_session', { session_id: info.id })).result;
      if (snapshot.info.status === 'idle' && snapshot.info.last_seq >= info.last_seq) break;
      await sleep(100);
    }
    check(snapshot.info.last_seq >= info.last_seq && snapshot.history_revision === 1 && snapshot.info.status === 'idle', `${width} ${theme}: server reused old seqs while device was offline`, snapshot.info.last_seq);
    check(await p.eval(`__mod('lib/store.ts').then(m => m.state.sessions[${JSON.stringify(info.id)}]?.historyRevision === 0 && m.state.sessions[${JSON.stringify(info.id)}]?.entries.some(e => JSON.stringify(e).includes('Remove this old')))`), `${width} ${theme}: disconnected device still has its old history`);
    await p.eval(`(async () => { const b = (await __mod('lib/backend.ts')).backend; await b.switchTo((await b.getConfig()).active); })()`);
    await p.waitFor(`__mod('lib/store.ts').then(m => m.state.conn.state === 'connected' && m.state.sessions[${JSON.stringify(info.id)}]?.loaded && m.state.sessions[${JSON.stringify(info.id)}]?.historyRevision === 1)`, 15000);
    const restored = await p.eval(`__mod('lib/store.ts').then(m => JSON.parse(JSON.stringify(m.state.sessions[${JSON.stringify(info.id)}].entries)))`);
    check(isDeepStrictEqual(restored, snapshot.entries), `${width} ${theme}: reconnected history exactly matches server, not deleted entries`, { restored, expected: snapshot.entries });
    check(requests[0] === info.last_seq && requests.includes(null), `${width} ${theme}: incremental revision mismatch requested a full reload`, requests);
    check(await p.eval(`__mod('lib/workspace.ts').then(w => w.getDraft(w.workspace.focused()) === 'Keep my local draft')`), `${width} ${theme}: local draft survived history replacement`);
    await p.close();
    control.close();
  }
}
