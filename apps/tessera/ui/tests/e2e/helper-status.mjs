// The historical status contract and incremental helper-history work. Imported
// by ui-busy so these checks run with the default performance suite as well.
import { check, device, finish, mock, run, signIn } from './cdp.mjs';

export async function helperStatusChecks(p) {
  const result = await p.eval(`(async () => {
    const [{ childStatus }, store, { toolPresentation }, { detailsChildren }] = await Promise.all([
      __mod('components/chat/Subagents.tsx'), __mod('lib/store.ts'), __mod('lib/tools.ts'), __mod('components/chat/SubagentGate.tsx')
    ]);
    const base = Object.values(store.state.sessions).find(s => s.info.kind === 'chat');
    const childId = 'synthetic-status-child', ref = { sessionId: childId, name: 'Status fixture', background: true };
    const child = info => store.setState('sessions', childId, { ...base, entries: [], info: { ...base.info, kind: 'subagent', id: childId, parent_id: 'synthetic-status-parent', ...info } });
    const call = (id, name, arguments_) => ({ type: 'tool_call', id, name, arguments: arguments_ });
    const assistant = (seq, content) => ({ kind: 'assistant', seq, ts: 1, content });
    const report = (seq, body = 'Finished review.') => ({ kind: 'user', seq, ts: 1, author: 'subagent:' + childId, content: [{ type: 'text', text: '[Subagent report: Status fixture · 0 steps · 1 s]\\n\\n' + body }] });
    const reportOf = parent => {
      for (let i = parent.entries.length - 1; i >= 0; i--) {
        const e = parent.entries[i];
        if (e.kind === 'user' && e.author === 'subagent:' + childId) return e.content[0].text.split('\\n\\n').slice(1).join('\\n\\n');
      }
    };
    // The B27 rule before the performance patch, deliberately eager. This is
    // the reference for every owner, not a rewritten version of the new rule.
    const oldStatus = parent => {
      const info = store.state.sessions[childId]?.info;
      if (info) {
        if (info.helper_queued && store.isBusy(info)) return 'waiting';
        if (store.isBusy(info)) return 'running';
        if (info.status === 'error' || info.status === 'interrupted') return 'error';
        if (info.status !== 'idle' || info.steps !== 0) return 'ok';
        const start = info.parent_call ? parent.entries.flatMap(e => e.kind === 'assistant' ? e.content : []).find(p => p.type === 'tool_call' && p.id === info.parent_call) : undefined;
        const native = info.helper_owner === 'tessera' || !info.parent_call || (start?.type === 'tool_call' && toolPresentation(start.name, start.arguments).helper === 'controls');
        const foregroundDone = !!info.parent_call && !!detailsChildren(parent.results[info.parent_call]?.details)?.some(c => c.sessionId === info.id && !c.background);
        if (native && !foregroundDone && !reportOf(parent)) return 'waiting';
        return 'ok';
      }
      const rep = reportOf(parent);
      return rep ? (/^The subagent did not finish/m.test(rep) ? 'error' : 'ok') : 'unknown';
    };
    const rows = [], failures = [];
    const shapes = [['legacy Agent', 'Agent', { tasks: [{ name: 'Legacy' }] }], ['agent', 'agent', { tasks: [{ name: 'Native' }] }], ['CLI Agent', 'Agent', { prompt: 'Review' }], ['read', 'Read', { path: '/synthetic' }], ['missing', null, {}]];
    const phases = [['running', 'running', 0, false], ['queued', 'running', 0, true], ['approval', 'awaiting_approval', 0, false], ['compacting', 'compacting', 0, false], ['idle zero', 'idle', 0, false], ['idle steps', 'idle', 20, false], ['idle queued', 'idle', 0, true], ['error', 'error', 0, false], ['interrupted', 'interrupted', 0, false]];
    try {
      for (const owner of ['tessera', 'claude_code', undefined]) for (const hasCall of [true, false]) for (const [shape, name, args] of shapes) for (const delivered of [false, true]) for (const foreground of [false, true]) for (const [phase, status, steps, queued] of phases) {
        child({ helper_owner: owner, parent_call: hasCall ? 'start' : undefined, status, steps, helper_queued: queued });
        const parent = { ...base, info: { ...base.info, id: 'synthetic-status-parent' }, entriesVersion: 0, entries: [assistant(1, name ? [call('start', name, args)] : [])], results: foreground ? { start: { details: { subagents: [{ name: 'Status fixture', session_id: childId, background: false }] } } } : {} };
        if (delivered) parent.entries.push(report(2));
        const want = oldStatus(parent), got = childStatus(parent, ref);
        const label = [owner ?? 'none', hasCall ? 'call' : 'no call', shape, delivered ? 'report' : 'no report', foreground ? 'foreground' : 'background', phase].join(' / ');
        rows.push([label, got, want]);
        if (got !== want) failures.push([label, got, want]);
      }
      child({ helper_owner: 'claude_code', parent_call: 'start', status: 'idle', steps: 0, helper_queued: false });
      const edge = { ...base, info: { ...base.info, id: 'synthetic-status-parent' }, entriesVersion: 0, entries: [assistant(1, [call('start', 'Agent', { tasks: [{ name: 'Legacy' }] })])], results: {} };
      const legacy = childStatus(edge, ref);
      edge.entries = [assistant(1, [call('start', 'Read', {})]), assistant(2, [call('start', 'Agent', { tasks: [] })])];
      rows.push(['duplicate call id retains the first starting call', childStatus(edge, ref), oldStatus(edge)]);
      store.setState('sessions', childId, undefined);
      for (const [label, body] of [['missing child with report', 'Finished review.'], ['missing child failed', 'The subagent did not finish'], ['missing child no report', null]]) {
        edge.entries = body === null ? [] : [report(1, body)];
        rows.push([label, childStatus(edge, ref), oldStatus(edge)]);
      }

      // Count actual content reads, not elapsed time. Each new parent entry
      // must be indexed once even when forty different helpers ask for it.
      let reads = 0;
      const indexedEntry = (seq, id, name = 'Read') => ({ kind: 'assistant', seq, ts: 1, get content() { reads++; return [call(id, name, name === 'Agent' ? { tasks: [] } : {})]; } });
      const entries = Array.from({ length: 1000 }, (_, i) => indexedEntry(i + 1, 'unrelated-' + i));
      const indexed = { ...base, info: { ...base.info, id: 'synthetic-index-parent' }, entriesVersion: 0, entries, results: {} };
      const assert = (label, got, want) => rows.push([label, got, want]);
      const ask = id => {
        child({ helper_owner: 'claude_code', parent_call: id, status: 'idle', steps: 0, helper_queued: false });
        return childStatus(indexed, ref);
      };
      assert('unknown starting call is done for a CLI-owned helper', ask('late'), 'ok');
      assert('initial parent history is indexed once', reads, 1000);
      for (let i = 0; i < 40; i++) ask('unrelated-' + i);
      assert('forty helpers do not rescan the indexed prefix', reads, 1000);
      entries.push(indexedEntry(1001, 'late', 'Agent'));
      assert('appended legacy control call becomes waiting', ask('late'), 'waiting');
      assert('append scans only the new entry', reads, 1001);
      entries.push(report(1002));
      assert('appended report completes the helper before control-call lookup', ask('late'), 'ok');
      for (let i = 0; i < 40; i++) ask('unrelated-' + i);
      assert('completed reports reuse the same index', reads, 1001);
      entries.length = 1000;
      assert('shrinking history removes the cached call and report', ask('late'), 'ok');
      assert('shrinking history rebuilds once', reads, 2001);
      entries[0] = indexedEntry(1, 'late', 'Agent');
      assert('a changed first entry rebuilds equal-length history', ask('late'), 'waiting');
      assert('changed first entry scans each entry once', reads, 3001);
      entries[500] = indexedEntry(501, 'middle', 'Agent'); indexed.entriesVersion++;
      assert('an in-place history rewrite invalidates by entriesVersion', ask('middle'), 'waiting');
      assert('version invalidation scans once', reads, 4001);
      const other = { ...indexed, info: { ...indexed.info, id: 'synthetic-other-parent' }, entries: [] };
      assert('starting calls never leak into another parent', childStatus(other, ref), 'ok');

      // Use the real Solid store and a real reactive consumer as well. The
      // incremental traversal must not subscribe to each old content node.
      const solidURL = performance.getEntriesByType('resource').find(e => new URL(e.name).pathname.endsWith('/solid-js.js'))?.name;
      if (!solidURL) throw new Error('The app Solid runtime was not observed');
      const { createRoot, createMemo } = await import(solidURL);
      createRoot(dispose => {
        const pid = 'synthetic-reactive-parent';
        store.setState('sessions', pid, { ...base, info: { ...base.info, id: pid }, entriesVersion: 0, entries: [assistant(1, [])], results: {} });
        child({ helper_owner: 'claude_code', parent_call: 'reactive', status: 'idle', steps: 0, helper_queued: false });
        const current = createMemo(() => childStatus(store.state.sessions[pid], ref));
        assert('reactive absent call begins done', current(), 'ok');
        store.setState('sessions', pid, 'entries', 1, assistant(2, [call('reactive', 'Agent', { tasks: [] })]));
        assert('reactive append updates all dependent helper statuses', current(), 'waiting');
        store.setState('sessions', pid, 'entries', 2, report(3));
        assert('reactive report append completes the helper', current(), 'ok');
        store.setState('sessions', pid, 'entries', e => e.slice(0, 2));
        assert('reactive prune discards the completion report', current(), 'waiting');
        dispose(); store.setState('sessions', pid, undefined);
      });
      return { rows, failures, legacy, count: rows.length };
    } finally { store.setState('sessions', childId, undefined); }
  })()`);
  check(result.legacy === 'waiting', 'Claude-owned legacy Agent with tasks stays waiting', result.legacy);
  for (const [label, got, want] of result.rows) check(got === want, `helper status ${label}`, { got, want });
  console.log('STATUS_EQUIVALENCE', JSON.stringify({ cases: result.count, mismatches: result.failures }));
}

if (process.argv[1]?.endsWith('/helper-status.mjs')) run(async () => {
  await mock('/__mock/reset');
  const p = await device();
  await signIn(p, 'Helper status equivalence');
  await helperStatusChecks(p);
  await p.close();
  finish();
});
