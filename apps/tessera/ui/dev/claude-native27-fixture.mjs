// Match the native27 fake CLI's stored notice/tool shapes, not private CLI files.
export function claudeNative27Fixture(mode, { createSession, addEntry, emitSession }) {
  const model = { provider: 'claude-code', model: 'sonnet' };
  const waiting = mode === 'waiting';
  const rec = createSession({ title: `Claude native27 ${mode}`, model, status: waiting ? 'running' : 'error' });
  const at = mode === 'dst-before' ? '2026-11-01T06:59:00Z'
    : mode === 'dst-after' ? '2026-11-01T07:01:00Z' : '2026-10-12T16:00:00Z';
  const text = mode === 'missing'
    ? "Your Claude plan's usage limit is reached. Claude Code didn't report a reset time. Send a message later to continue, or choose another model."
    : mode === 'credits'
      ? 'Your Claude plan needs usage credits before it can continue. Check your plan in Claude, or choose another model.'
      : `Your Claude plan's ${mode === 'weekly' ? 'weekly' : '5-hour'} limit is reached. It resets at ${at}. ${waiting ? "I'll continue then; Stop cancels the wait." : 'Send a message later to continue, or choose another model.'}`;
  addEntry(rec, { kind: 'notice', level: waiting ? 'warning' : 'error', text });
  for (const [name, args, out] of [
    ['write', { path: 'native27.txt', content: 'alpha\nbeta\n' }, '[native27.txt#ABCD] Created (2 lines)'],
    ['edit', { path: 'native27.txt', lines: [{ at: '1', text: 'gamma' }] }, '[native27.txt#BCDE] edited\n1:gamma'],
    ['glob', { pattern: 'native27*.txt' }, 'native27.txt'],
    ['web_fetch', { url: 'https://example.com' }, '# Example\nReader output'],
  ]) {
    const id = `native27-${name}`;
    addEntry(rec, { kind: 'assistant', usage: {input: 1, output: 1, cache_read: 0, cache_write: 0}, cost: 0, model, stop: 'tool_use', content: [{ type: 'tool_call', id, name, arguments: args }] });
    addEntry(rec, { kind: 'tool_result', call_id: id, name, content: [{ type: 'text', text: out }], is_error: false });
  }
  const childModel = { provider: 'mock', model: 'm' };
  const child = createSession({ kind: 'subagent', parent_id: rec.info.id, title: 'native27 helper', model: childModel, permission: 'read_only' });
  child.info.helper_owner = 'tessera';
  child.info.parent_call = 'native27-agent';
  addEntry(child, { kind: 'assistant', usage: {input: 1, output: 1, cache_read: 0, cache_write: 0}, cost: 0, model: childModel, stop: 'end_turn', content: [{ type: 'text', text: 'hello from the other model' }] });
  addEntry(rec, { kind: 'assistant', usage: {input: 1, output: 1, cache_read: 0, cache_write: 0}, cost: 0, model, stop: 'tool_use', content: [{ type: 'tool_call', id: 'native27-agent', name: 'agent', arguments: { tasks: [{ name: 'native27 helper', prompt: 'Check files', model: 'mock/m', readonly: true }], wait: true } }] });
  addEntry(rec, { kind: 'tool_result', call_id: 'native27-agent', name: 'agent', content: [{ type: 'text', text: 'hello from the other model' }], is_error: false, details: { subagents: [{ session_id: child.info.id, name: 'native27 helper', model: childModel, background: false, readonly: true, isolation: 'none', cwd: child.info.cwd }] } });
  emitSession(child);
  emitSession(rec);
  return { id: rec.info.id, raw: text };
}
