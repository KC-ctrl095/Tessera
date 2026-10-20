// Opt-in, deterministic render workload. No providers, disk, or external images.
export function transcriptPerfFixture(p, { seedSession, emitSession, state, U, A, R, CALL }) {
  const id = 'ses_transcript_perf';
  const turns = Math.max(750, Math.min(1500, Number(p.turns) || 750));
  const image = 'iVBORw0KGgoAAAANSUhEUgAAAlgAAAFUAQMAAADCgtB5AAAABlBMVEVUe2r////oXxOFAAAAMElEQVR42u3BgQAAAADDoPlTX+EAVQEAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAB8BmTwAAGkYMQDAAAAAElFTkSuQmCC';
  const prose = 'Synthetic review with **emphasis**, `inline code`, and a [reference](https://example.com). '.repeat(12);
  const code = Array.from({ length: 12 }, (_, i) => `const value${i} = await review(${i});`).join('\n');
  const output = Array.from({ length: 180 }, (_, i) => `Synthetic output line ${i} with project details.`).join('\n');
  const items = [];
  for (let i = 0; i < turns; i++) {
    const u = U(`Review synthetic section ${i}. Selection starts here.`);
    if (i % 40 === 0) u.content.push({ type: 'image', mime: 'image/png', data: image });
    const call = `perf-call-${i}`;
    const diff = `--- a/review.ts\n+++ b/review.ts\n@@ -1,2 +1,2 @@\n-old value\n+new value\n context\n`;
    items.push(u, A({ thinking: `Considering synthetic section ${i}. `.repeat(25) }, CALL(call, i % 3 ? 'read' : 'edit', { path: '/synthetic/review.ts', old: 'old value', new: 'new value' })),
      R(call, i % 3 ? 'read' : 'edit', output, i % 3 ? {} : { details: { diff } }),
      A(`## Synthetic section ${i}\n\n${prose}\n\n\`\`\`ts\n${code}\n\`\`\`\n\n| Item | Result |\n|---|---|\n| Review | Complete |\n| Section | ${i} |\n\nSelection ends here. ${i === turns - 1 ? 'Newest synthetic message.' : ''}`));
    if (i % 100 === 0) {
      const cid = `${id}-helper-${i}`;
      const child = seedSession({ id: cid, title: `Synthetic helper ${i}`, kind: 'subagent', parent_id: id, parent_call: call, helper_owner: 'tessera', status: 'idle', model: { provider: 'openrouter', model: 'mistralai/mistral-small-3.2' }, created: 1700000000000, updated: 1700000001000, items: [U('Synthetic review'), A('Review complete')] }, state.providers);
      state.sessions.set(cid, child); emitSession(child);
      items.push({ ...U(`[Subagent report: Synthetic helper ${i} · 1 step]\n\nSynthetic findings.`), author: `subagent:${cid}` });
    }
  }
  const rec = seedSession({ id, title: 'Huge synthetic transcript', model: { provider: 'openrouter', model: 'mistralai/mistral-small-3.2' }, created: 1700000000000, updated: 1700001000000, items }, state.providers);
  state.sessions.set(id, rec); emitSession(rec);
  return { id, entries: rec.entries.length, turns };
}
