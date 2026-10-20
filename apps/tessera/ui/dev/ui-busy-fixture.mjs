// A long-lived Claude chat. Persist everything in the mock so snapshots cannot
// silently prune the completed helpers (or remove the running tool).
export function uiBusyFixture(p, { seedSession, addEntry, emitSession, state, Turn, U, A, R, CALL, addNotification }) {
  if (p.phase === 'seed') {
    const model = { provider: 'claude-code', model: 'sonnet' };
    const id = 'ses_ui_busy';
    const created = Date.now() - 7 * 86400000, updated = Date.now() - 60000;
    const prose = 'Synthetic project findings with **emphasis**, `code`, and [references](https://example.com). '.repeat(20);
    const output = Array.from({ length: 100 }, (_, i) => `Synthetic file line ${i} with project details.`).join('\n');
    const items = [U('Review this synthetic project over many turns.')];
    for (let i = 0; i < 8000; i++) {
      if (i % 25 === 0) items.push(A(`## Findings ${i}\n\n${prose}`), U(`Continue the synthetic review ${i}`));
      const call = `busy-read-${i}`;
      items.push(A(CALL(call, i % 2 ? 'Read' : 'read', { path: `/synthetic/file-${i}` })), R(call, 'read', i % 10 ? 'Synthetic result' : output));
    }
    const children = [];
    for (let i = 0; i < 143; i++) {
      const cid = `${id}-helper-${i}`, call = `busy-agent-${i}`, running = i >= 140;
      const steps = i < (p.zero_steps ?? 24) ? 0 : 20;
      const native = i % 2 === 0;
      const name = native ? 'Agent' : 'agent';
      const args = native ? { description: `Synthetic helper ${i}`, prompt: 'Review the project', subagent_type: 'Explore', run_in_background: true } : { tasks: [{ name: `Synthetic helper ${i}`, prompt: 'Review the project' }], wait: false };
      items.push(A(CALL(call, name, args)), R(call, name, 'Helper started', { details: { subagents: [{ name: `Synthetic helper ${i}`, session_id: cid, background: true }] } }));
      if (!running) items.push({ ...U(`[Subagent report: Synthetic helper ${i} · ${steps} steps · 1 min]\n\nFinished the synthetic review.`), author: `subagent:${cid}` });
      const childItems = [U('Synthetic project review')];
      for (let step = 0; step < steps; step++) childItems.push(A(CALL(`child-read-${i}-${step}`, 'read', { path: '/synthetic/project' })), R(`child-read-${i}-${step}`, 'read', 'Synthetic findings'));
      childItems.push(A('Review complete'));
      const child = seedSession({ id: cid, title: `Synthetic helper ${i}`, kind: 'subagent', parent_id: id, parent_call: call, helper_owner: i < (p.legacy ?? 0) ? undefined : native ? 'claude_code' : 'tessera', status: running ? 'running' : 'idle', model: native ? model : { provider: 'openrouter', model: 'mistralai/mistral-small-3.2' }, created: updated - 60000 + i, updated, items: childItems }, state.providers);
      state.sessions.set(cid, child);
      emitSession(child);
      children.push(cid);
    }
    const tool = 'busy-running';
    items.push(U('Continue while the other checks finish.'), A(CALL(tool, 'Bash', { command: 'sleep 600' })));
    const rec = seedSession({ id, title: 'Synthetic long-lived Claude chat', model, status: 'running', created, updated, items }, state.providers);
    rec.turn = new Turn(rec);
    rec.turn.phase = 'tools';
    rec.turn.runningTools = [{ call_id: tool, name: 'Bash', output: 'Synthetic command is running.' }];
    state.sessions.set(id, rec);
    state.uiBusy = { rec, tool, children };
    emitSession(rec);
    for (let i = 0; i < 80; i++) addNotification({ kind: 'helper', title: `Synthetic report ${i}`, body: 'Synthetic review complete', session_id: children[i], quiet: true });
  } else {
    const f = state.uiBusy;
    if (!f) throw new Error('Seed the UI busy fixture first');
    if (p.phase === 'tick') {
      addEntry(f.rec, { ...A('Periodic synthetic findings with **new details**.'), model: f.rec.info.model, usage: { input: 10, output: 10, cache_read: 0, cache_write: 0 } });
    } else throw new Error('Unknown UI busy fixture phase');
  }
  const f = state.uiBusy;
  return { id: f.rec.info.id, entries: f.rec.entries.length, children: f.children, tool: f.tool };
}
