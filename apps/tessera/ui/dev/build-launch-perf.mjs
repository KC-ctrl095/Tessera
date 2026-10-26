// Opt-in benchmark build only. No instrumentation is shipped by `npm run build`.
// Usage: PERF_DIST=/absolute/output node dev/build-launch-perf.mjs
import { build } from 'vite';
if (!process.env.PERF_DIST) throw Error('Set PERF_DIST outside the checkout');
function change(source, from, to) {
  if (!source.includes(from)) throw Error(`Launch probe no longer matches: ${from}`);
  return source.replace(from, to);
}
await build({ plugins: [{ name: 'launch-measurements', enforce: 'pre', configResolved(config) {
  // Probes themselves add eager bytes. Keep the actual budgets unchanged and
  // require a separate normal `npm run build` as the release gate.
  const budget = config.plugins.find(p => p.name === 'tessera-first-load-budget');
  if (budget) budget.writeBundle = () => console.log('Measurement build only. Run npm run build for unchanged production budgets.');
}, transform(source, id) {
  if (id.endsWith('/src/lib/transport.ts')) {
    source = change(source, 'this.send(ws, JSON.stringify({ id, ...request }));', 'globalThis.__launch?.sent(id, request.method); this.send(ws, JSON.stringify({ id, ...request }));');
    source = change(source, 'msg = JSON.parse(data) as ServerMessage;', 'const began = performance.now(); msg = JSON.parse(data) as ServerMessage; globalThis.__launch?.received(msg, data.length, performance.now()-began);');
  }
  if (id.endsWith('/src/lib/e2e.ts')) {
    source = change(source, 'let plain: Uint8Array;\n    try {', 'let plain: Uint8Array; const began = performance.now();\n    try {');
    source = change(source, 'this.n++;\n    const flag', 'globalThis.__launch?.decrypt(performance.now()-began); this.n++;\n    const flag');
  }
  if (id.endsWith('/src/lib/backend.ts')) source += '\n globalThis.__launchBackend = backend;';
  if (id.endsWith('/src/lib/workspace.ts')) source += '\n globalThis.__launchWorkspace = {workspace, openSession, focusPane, closePane};';
  if (id.endsWith('/src/lib/store.ts')) {
    source = change(source, 'applySnapshot(snap, since !== undefined);', 'const __snapshotAt = performance.now(); applySnapshot(snap, since !== undefined); globalThis.__launch?.snapshot(performance.now()-__snapshotAt);');
    source = change(source, 'applySnapshot(full, false);', 'const __snapshotAt = performance.now(); applySnapshot(full, false); globalThis.__launch?.snapshot(performance.now()-__snapshotAt);');
    source += '\n globalThis.__launchStore = {state, api, refreshArchived};';
    source = change(source, 'const open = new Set(openSessionIds());', 'const __installAt = performance.now(); const open = new Set(openSessionIds());');
    source = change(source, 'for (const cb of bootListeners) cb();', 'globalThis.__launch?.installed(performance.now()-__installAt); for (const cb of bootListeners) cb();');
  }
  return source;
} }], build: { outDir: process.env.PERF_DIST, emptyOutDir: true } });
