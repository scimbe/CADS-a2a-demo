// Runs the logic from index.html itself: node --test tests/
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

const html = readFileSync(new URL('../index.html', import.meta.url), 'utf8');
const m = /\/\/ PAGE-LOGIC-START[^\n]*\n([\s\S]*?)\/\/ PAGE-LOGIC-END/.exec(html);
assert.ok(m, 'page logic block missing from index.html');
const ctx = {};
vm.runInNewContext(`${m[1]}; this.parseReply = parseReply; this.topologyNote = topologyNote;`, ctx);

test('a reply with ok:false is not a success', () => {
  const r = ctx.parseReply('{"ok":false,"error":"handler exited 1"}');
  assert.equal(r.ok, false);
  assert.equal(r.text, 'handler exited 1');
});

test('the reply text is shown, not the raw JSON envelope', () => {
  const r = ctx.parseReply('{"ok":true,"output":"agent-alice: you said \\"hi\\""}');
  assert.equal(r.ok, true);
  assert.equal(r.text, 'agent-alice: you said "hi"');
});

test('a plain text reply stays as it is', () => {
  assert.deepEqual({ ...ctx.parseReply('pong') }, { ok: true, text: 'pong' });
});

test('peers that were never set up are named as such, not just "offline"', () => {
  assert.match(ctx.topologyNote({ bob1_provisioned: false, bob2_provisioned: false }), /not set up/);
  assert.doesNotMatch(ctx.topologyNote({ bob1_provisioned: true, bob2_provisioned: false }), /not set up/);
});

test('a failed POST and a lost stream cannot leave the button disabled', () => {
  assert.match(html, /catch \{ failRound\(/);
  assert.match(html, /_chatStreamLost/);
  assert.match(html, /NO_ANSWER_MS/);
});
