// Synthetic 6705dc6-shaped payloads. No helper_owner or future capabilities.
export function helperDisplayFixture(mode, { createSession, addEntry, emitSession, broadcast, state }) {
  if (mode === 'pulse' || mode === 'finish') {
    const f = state.helperDisplayFixture;
    if (!f) throw new Error('Seed helper display first');
    if (mode === 'pulse') {
      for (const rec of [f.parent, f.children[0]]) broadcast({ type: 'stream_delta', session_id: rec.info.id, delta: { index: 0, part: 'text', text: 'Synthetic streamed progress. ' } });
    } else {
      for (const child of f.children) {
        child.info.status = 'interrupted';
        emitSession(child);
      }
    }
    return { id: f.parent.info.id, children: f.children.map(c => c.info.id) };
  }
  if (mode === 'associate') {
    const f = state.helperDisplayFixture;
    if (!f) throw new Error('Seed helper display first');
    f.children.forEach((rec, i) => addEntry(f.parent, { kind: 'tool_result', call_id: f.calls[i], name: 'agent', content: [{ type: 'text', text: 'Synthetic started helper.' }], is_error: false, details: { subagents: [{ name: rec.info.title.replace(/^↳\s*/, ''), session_id: rec.info.id, background: true }] } }));
    return { id: f.parent.info.id, children: f.children.map(c => c.info.id), calls: f.calls };
  }
  const native = mode.startsWith('native');
  const model = native ? { provider: 'anthropic', model: 'claude-sonnet-5-5' } : { provider: 'claude-code', model: 'sonnet' };
  const parent = createSession({ title: `Legacy ${mode} helpers`, model, status: 'running' });
  const base = Date.now() - 15000;
  const calls = [];
  const call = (id, name, args, ts) => {
    calls.push(id);
    addEntry(parent, { kind: 'assistant', ts, model, stop: 'tool_use', content: [{ type: 'tool_call', id, name, arguments: args }] });
  };
  const child = (name, status, created, parent_call) => {
    const rec = createSession({ title: `↳ ${name}`, kind: 'subagent', parent_id: parent.info.id, model: native ? model : mode === 'bridge' ? { provider: 'openai-codex', model: 'gpt-6.1-sol' } : model, status });
    Object.assign(rec.info, { created_at: created, updated_at: Date.now(), steps: status === 'running' ? 2 : 0, ...(parent_call ? { parent_call } : {}) });
    addEntry(rec, { kind: 'user', content: [{ type: 'text', text: 'Synthetic helper task. Read this fixture only.' }] });
    emitSession(rec);
    return rec;
  };
  let children;
  if (mode === 'native-ambiguous') {
    calls.push('display-same-first', 'display-same-second');
    addEntry(parent, { kind: 'assistant', ts: base, model, stop: 'tool_use', content: calls.map(id => ({ type: 'tool_call', id, name: 'agent', arguments: { tasks: [{ name: 'Repeated helper', prompt: 'Synthetic' }] } })) });
    children = [child('Repeated helper', 'running', base + 100), child('Repeated helper', 'running', base + 200)];
  } else if (mode === 'native-overlap') {
    call('display-earlier', 'agent', { tasks: [{ name: 'Repeated helper', prompt: 'Synthetic' }] }, base);
    call('display-later', 'agent', { tasks: [{ name: 'Repeated helper', prompt: 'Synthetic' }] }, base + 1000);
    children = [child('Repeated helper', 'running', base + 100)];
  } else if (native) {
    call('display-old', 'agent', { tasks: [{ name: 'First native helper', prompt: 'Synthetic' }] }, base);
    call('display-new', 'agent', { tasks: [{ name: 'Second native helper', prompt: 'Synthetic' }] }, base + 5000);
    children = [child('First native helper', 'running', base + 100), child('Second native helper', 'idle', base + 5100)];
  } else if (mode === 'bridge') {
    call('display-bridge', 'agents', { action: 'start', tasks: [{ name: 'GPT running helper' }, { name: 'GPT queued helper' }] }, base);
    children = [child('GPT running helper', 'running', base + 100, calls[0]), child('GPT queued helper', 'idle', base + 200, calls[0])];
  } else {
    call('display-cli', 'Agent', { description: 'CLI native helper', subagent_type: 'Explore', prompt: 'Synthetic' }, base);
    children = [child('CLI native helper', 'running', base + 100, calls[0])];
    call('display-metadata', 'Task', { description: 'Older background helper', subagent_type: 'Explore', prompt: 'Synthetic' }, base + 3000);
    addEntry(parent, { kind: 'tool_result', call_id: 'display-metadata', name: 'Task', content: [{ type: 'text', text: 'Started in background.' }], is_error: false, details: { subagent: { description: 'Older background helper', type: 'Explore', background: true, tool_uses: 0, status: 'running' } } });
    call('display-approval', 'Task', { description: 'Helper awaiting approval', prompt: 'Synthetic', subagent_type: 'general-purpose' }, base + 4000);
    parent.approvals.push({ id: 'display-approval-request', call_id: 'display-approval', tool: 'Task', summary: 'Start helper', arguments: { description: 'Helper awaiting approval' }, reason: 'Synthetic approval' });
  }

  emitSession(parent);
  state.helperDisplayFixture = { parent, children, calls };
  return { id: parent.info.id, children: children.map(c => c.info.id), calls };
}
