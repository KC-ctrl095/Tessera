// Controlled synthetic recall turn, kept in server state so later snapshots/reloads
// restore the same calls and sources. No client-only entries or seeded-chat hits.
export function recallLazyFixture(phase, { createSession, addEntry, emitSession, broadcast, state, Turn, U, A, R, CALL, zeroUsage }) {
  if (phase === 'seed') {
    const main = createSession({ title: 'Synthetic recall main' });
    const other = createSession({ title: 'Synthetic recall source' });
    const local = addEntry(main, U('Synthetic same-chat source.'));
    addEntry(main, { ...A('Synthetic neighbor stays mounted.'), model: main.info.model, usage: zeroUsage() });
    const remote = addEntry(other, U('Synthetic other-chat source.'));
    const hit = (rec, entry) => ({ session_id: rec.info.id, session_title: rec.info.title, seq: entry.seq, ts: entry.ts, snippet: entry.content[0].text });
    state.recallLazyFixture = { main, other, hits: [hit(main, local), hit(other, remote)], call: `recall-${main.info.id}`, args: { query: 'synthetic sources', scope: 'all' } };
    emitSession(main);
    emitSession(other);
  } else {
    const f = state.recallLazyFixture;
    if (!f) throw new Error('Seed the recall fixture first');
    const { main, call, args } = f;
    const sid = main.info.id;
    if (phase === 'stream') {
      main.info.status = 'running';
      main.turn = new Turn(main);
      main.turn.phase = 'stream';
      main.turn.parts = [{ type: 'tool_call', id: call, name: 'recall', buf: JSON.stringify(args) }];
      emitSession(main);
      broadcast({ type: 'stream_started', session_id: sid, model: main.info.model });
      broadcast({ type: 'stream_delta', session_id: sid, delta: { part: 'tool_call', index: 0, id: call, name: 'recall', arguments: JSON.stringify(args) } });
    } else if (phase === 'call') {
      main.turn.phase = 'tools';
      broadcast({ type: 'stream_ended', session_id: sid });
      addEntry(main, { ...A(CALL(call, 'recall', args)), model: main.info.model, usage: zeroUsage() });
      main.turn.runningTools = [{ call_id: call, name: 'recall', output: 'Synthetic live recall output.' }];
      broadcast({ type: 'tool_started', session_id: sid, call_id: call, name: 'recall' });
      broadcast({ type: 'tool_output', session_id: sid, call_id: call, chunk: 'Synthetic live recall output.' });
    } else if (phase === 'result') {
      addEntry(main, R(call, 'recall', 'Synthetic persisted recall output.', { details: { recall: { hits: f.hits } } }));
      main.turn = null;
      main.info.status = 'idle';
      emitSession(main);
    } else throw new Error(`Unknown recall fixture phase: ${phase}`);
  }
  const f = state.recallLazyFixture;
  return { id: f.main.info.id, other: f.other.info.id, call: f.call, hits: f.hits };
}
