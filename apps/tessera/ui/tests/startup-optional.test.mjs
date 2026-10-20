// The integrated optional modules get exact call-site proofs, not a blanket
// exemption for their destination. The real startup inbox watcher still counts.
import assert from 'node:assert/strict';
import { readFileSync, existsSync } from 'node:fs';
import { dirname, resolve as absolute } from 'node:path';
import test from 'node:test';
import { startupCallsites } from '../build/startup-audit.mjs';
import { STARTUP_EDGES } from '../build/startup-edges.mjs';
import { recoverableParts } from '../build/recoverable-parts.mjs';

const root = absolute(new URL('..', import.meta.url).pathname);
const cases = [
  ['src/App.tsx', './components/DirBrowser', 'src/components/DirBrowser.tsx'],
  ['src/components/chat/ChatHeader.tsx', './TileOptions', 'src/components/chat/TileOptions.tsx'],
  ['src/components/chat/TileOptions.tsx', './HelperLimits', 'src/components/chat/HelperLimits.tsx'],
  ['src/components/NotificationBell.tsx', './NotificationInbox', 'src/components/NotificationInbox.tsx'],
  ['src/components/notes/NotesList.tsx', './NotesListViews', 'src/components/notes/NotesListViews.tsx'],
  ['src/components/Snippet.tsx', '../lib/snippet', 'src/lib/snippet.ts'],
  ['src/lib/voice-host.ts', './voice/conversation', 'src/lib/voice/conversation.ts'],
];
const resolver = async (specifier, owner) => {
  const path = absolute(dirname(owner), specifier);
  const id = [path, `${path}.ts`, `${path}.tsx`].find(existsSync);
  return id ? { id } : null;
};

for (const [owner, specifier, target] of cases) test(`${target}: real source has one deferred loader, other calls fail closed`, async () => {
  const id = absolute(root, owner), code = readFileSync(id, 'utf8');
  const calls = await startupCallsites(code, id, resolver);
  const selected = calls.filter(site => site.specifier === specifier);
  assert.equal(selected.length, 1);
  assert.deepEqual(selected[0].modes, []);
  assert.ok(selected[0].loaders.length);
  assert.equal(selected[0].target, absolute(root, target));
  await assert.rejects(startupCallsites(`${code}\nimport(${JSON.stringify(specifier)});`, id, resolver), /Unaudited|top-level/);
  await assert.rejects(startupCallsites(code, absolute(root, `${dirname(owner)}/other.tsx`), resolver), /Unaudited/);
});

test('the inbox startup watcher is not excluded by the user-opened inbox UI rule', () => {
  assert.equal(STARTUP_EDGES['src/lib/notificationInbox.ts'], undefined);
  const app = readFileSync(absolute(root, 'src/lib/app.ts'), 'utf8');
  assert.match(app, /import\("\.\/notificationInbox"\).*watchInbox/);
});

test('the named Notes row loader has a fresh dev and emitted-build recovery URL', () => {
  const plugin = recoverableParts();
  let dev;
  plugin.configureServer({ middlewares: { use(route, handler) {
    assert.equal(route, '/parts.json');
    handler({}, { setHeader() {}, end(body) { dev = JSON.parse(body); } });
  } } });
  assert.deepEqual(dev.NotesListViews, { file: 'src/components/notes/NotesListViews.tsx', css: [] });
  const bundle = Object.fromEntries(Object.entries(dev).filter(([, part]) => typeof part === 'object').map(([name, part]) => [`${name}.js`, { type: 'chunk', fileName: `${name}.js`, facadeModuleId: absolute(root, part.file), imports: [], viteMetadata: { importedCss: new Set(name === 'NotesListViews' ? ['notes.css'] : []) } }]));
  let emitted;
  plugin.generateBundle.call({ getFileName() { return 'helper.js'; }, error(message) { throw Error(message); }, emitFile(asset) { emitted = JSON.parse(asset.source); } }, {}, bundle);
  assert.deepEqual(emitted.NotesListViews, { file: 'NotesListViews.js', css: ['notes.css'] });
});
