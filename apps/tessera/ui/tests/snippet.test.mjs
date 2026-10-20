// Pure snippet formatting: use the project's TS compiler, no browser/server/user data.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import ts from 'typescript';
const source = readFileSync(new URL('../src/lib/snippet.ts', import.meta.url), 'utf8');
const js = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 } }).outputText;
const { plainMarkdown, pieces, highlight } = await import(`data:text/javascript;base64,${Buffer.from(js).toString('base64')}`);

test('note preview drops headings, emphasis, code ticks, lists and link targets', () => {
  assert.equal(plainMarkdown('# Day one\n\nPack the **tent**, _stove_ and `matches`.\n- See [the list](https://example.com/list).'), 'Day one Pack the tent, stove and matches. See the list.');
});
test('wiki labels and image alt text stay readable; angle brackets stay literal', () => {
  assert.equal(plainMarkdown('See [[Packing|the kit]] and ![a map](assets/map.png). <b>Safe</b>.'), 'See the kit and a map. <b>Safe</b>.');
});
test('snake_case and punctuation survive inline emphasis removal', () => {
  assert.equal(plainMarkdown('Keep snake_case_names and **words**. Try *another* (or _third_).'), 'Keep snake_case_names and words. Try another (or third).');
});
test('server FTS marks become highlighted words with no guillemets left', () => {
  assert.deepEqual(pieces('**Map the «reconnect» path:** `socket`'), [{ text: 'Map the ', hit: false }, { text: 'reconnect', hit: true }, { text: ' path: socket', hit: false }]);
});
test('queries are literal, case-insensitive, and never HTML or regular expressions', () => {
  assert.deepEqual(highlight('A c++ and C++ example', 'c++'), [{ text: 'A ', hit: false }, { text: 'c++', hit: true }, { text: ' and ', hit: false }, { text: 'C++', hit: true }, { text: ' example', hit: false }]);
  assert.equal(highlight('<script>alert(1)</script>', '.*').some(p => p.hit), false);
  assert.deepEqual(highlight('plain words', ''), [{ text: 'plain words', hit: false }]);
});
test('raw titles retain markdown-looking literal characters and still highlight', () => {
  assert.deepEqual(pieces('**literal title**', 'title', true), [{ text: '**literal ', hit: false }, { text: 'title', hit: true }, { text: '**', hit: false }]);
});
test('fenced snippets retain code and FTS matches, not fence lines or language ids', () => {
  assert.deepEqual(pieces('Before\n```js\nconst word = «needle»;\n```\nAfter'), [{ text: 'Before const word = ', hit: false }, { text: 'needle', hit: true }, { text: '; After', hit: false }]);
  assert.equal(plainMarkdown('  ~~~python\nprint("needle")\n  ~~~'), 'print("needle")');
});
test('truncated snippets keep a match after an opening fence or before a closing fence', () => {
  for (const text of ['```js\nconst word = «needle»;', 'const word = «needle»;\n```', 'word = «needle»']) {
    const ps = pieces(text);
    assert.equal(ps.filter(p => p.hit).map(p => p.text).join(''), 'needle');
    assert.ok(ps.map(p => p.text).join('').includes('word = needle'));
    assert.ok(!ps.map(p => p.text).join('').includes('```'));
  }
});
test('a query match inside a code-only snippet stays visible without FTS marks', () => {
  assert.ok(pieces('```rust\nlet needle = value;\n```', 'needle').some(p => p.hit && p.text === 'needle'));
});
test('comparisons, generics and JSX retain angle brackets and FTS matches', () => {
  for (const [source, expected, hit] of [
    ['```cpp\nif (a < «limit» && b > 0)\n```', 'if (a < limit && b > 0)', 'limit'],
    ['std::vector<«Widget»>', 'std::vector<Widget>', 'Widget'],
    ['```jsx\n<«Widget» value={a < b} />\n```', '<Widget value={a < b} />', 'Widget'],
    ['<div>«hello»</div>', '<div>hello</div>', 'hello'],
    ['…vector<«Widget»> value;\n```', '…vector<Widget> value;', 'Widget'],
    ['```js\nif (a < «limit» && b >', 'if (a < limit && b >', 'limit'],
  ]) {
    const ps = pieces(source);
    assert.equal(ps.map(p => p.text).join(''), expected);
    assert.equal(ps.filter(p => p.hit).map(p => p.text).join(''), hit);
  }
});
test('HTML-looking content remains text for the escaped Solid renderer, never stripped', () => {
  assert.equal(plainMarkdown('<script>alert("needle")</script>'), '<script>alert("needle")</script>');
  assert.ok(pieces('<Widget />', 'Widget').some(p => p.hit && p.text === 'Widget'));
});
