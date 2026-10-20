import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import ts from 'typescript';

const source = readFileSync(new URL('../src/lib/claude-limit.ts', import.meta.url), 'utf8');
const { outputText } = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext } });
const { claudeLimitText } = await import(`data:text/javascript;base64,${Buffer.from(outputText).toString('base64')}`);
const at = '2026-10-12T16:00:00Z';
const phrase = `Your Claude plan's weekly limit is reached. It resets at ${at}. Send a message later to continue, or choose another model.`;
// engine.rs finalization uses human_wait + one_line(error, 200), not a bare notice.
const finalization = (took, error) => `After ${took}: ${error.replace(/\s+/g, ' ').trim().slice(0, 200)}`;

test('bare public reset localizes in Chicago', () => {
  process.env.TZ = 'America/Chicago';
  assert.match(claudeLimitText(phrase), /Oct 12.*11:00.*CDT/);
  assert.ok(!claudeLimitText(phrase).includes(at));
});

test('finalization-generated terminal error body localizes, preserving envelope', () => {
  process.env.TZ = 'America/Chicago';
  for (const took of ['1 min', '35 min', '2 h 5 min']) {
    const body = finalization(took, phrase);
    const localized = claudeLimitText(body);
    assert.ok(localized.startsWith(`After ${took}: Your Claude plan's`));
    assert.match(localized, /Oct 12.*11:00.*CDT/);
    assert.ok(!localized.includes(at));
  }
});

test('DST and date crossing use receiving-device time', () => {
  process.env.TZ = 'America/Chicago';
  assert.match(claudeLimitText(finalization('1 min', phrase.replace(at, '2026-11-01T06:59:00Z'))), /Nov 1.*1:59.*CDT/);
  assert.match(claudeLimitText(finalization('1 min', phrase.replace(at, '2026-11-01T07:01:00Z'))), /Nov 1.*1:01.*CST/);
  process.env.TZ = 'Asia/Tokyo';
  assert.match(claudeLimitText(finalization('1 min', phrase)), /Oct 13.*1:00/);
});

test('terminal envelope alone, embedded plan text and malformed envelopes stay untouched', () => {
  for (const body of [
    `After 1 min: An unrelated note. It resets at ${at}.`,
    `An unrelated quote: ${phrase}`,
    `After 1 min: Tool said ${phrase}`,
    `After tomorrow: ${phrase}`,
    `After 1 min:\n${phrase}`,
    `Before 1 min: ${phrase}`,
  ]) assert.equal(claudeLimitText(body), body);
});

test('reset-less, credits and invalid timestamps are not guessed', () => {
  for (const body of [
    "Your Claude plan's usage limit is reached. Claude Code didn't report a reset time.",
    'After 1 min: Your Claude plan needs usage credits before it can continue.',
    "Your Claude plan's limit is reached. It resets at 2026-13-12T16:00:00Z.",
  ]) assert.equal(claudeLimitText(body), body);
});
