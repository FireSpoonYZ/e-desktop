import test from 'node:test';
import assert from 'node:assert/strict';
import { HeadlessEmulator } from '../dist/emulator.mjs';

function state(e, buffer = e.terminal.buffer.active) {
  return { lines: Array.from({ length: buffer.length }, (_, i) => buffer.getLine(i).translateToString(true)),
    x: buffer.cursorX, y: buffer.cursorY };
}
test('Orca snapshot roundtrip: scrollback, alternate buffer, cursor, Unicode, kitty and partial CSI', async () => {
  const live = new HeadlessEmulator(24, 5), restored = new HeadlessEmulator(24, 5);
  try {
    await live.write(Array.from({ length: 12 }, (_, n) => 'line' + n + '\r\n').join(''));
    await live.write('汉字👩‍💻\x1b[?1049h\x1b[?2004h\x1b[?1003h\x1b[?1006h\x1b[=9;1uALT汉字👩‍💻\x1b[3;4H\x1b[31');
    const snapshot = live.getSnapshot(42);
    assert.equal(snapshot.seq, 42); assert.equal(snapshot.kittyKeyboardFlags, 9);
    assert.ok(snapshot.ansi.endsWith('\x1b[31'));
    await restored.write(snapshot.ansi);
    await live.write('mRED'); await restored.write('mRED');
    assert.deepEqual(state(restored), state(live));
    assert.deepEqual(state(restored, restored.terminal.buffer.normal), state(live, live.terminal.buffer.normal));
    assert.equal(restored.terminal.buffer.active.type, 'alternate');
    assert.equal(restored.terminal.modes.bracketedPasteMode, true);
    assert.equal(restored.terminal.modes.mouseTrackingMode, 'any');
    assert.equal(restored.terminal.unicode.activeVersion, 'orca-11-zwj');
    assert.equal(restored.getSnapshot(42).kittyKeyboardFlags, 9);
    await live.write('\x1b[?1049l'); await restored.write('\x1b[?1049l');
    assert.deepEqual(state(restored), state(live));
  } finally { live.dispose(); restored.dispose(); }
});
test('only live writes answer terminal queries; snapshot replay has no query authority', async () => {
  const replies = [];
  const e = new HeadlessEmulator(80, 24, reply => replies.push(reply));
  try {
    await e.write('\x1b[6n'); assert.deepEqual(replies, []);
    await e.write('\x1b[6n', { forwardQueryReplies: true });
    assert.deepEqual(replies, ['\x1b[1;1R']);
    await e.write(e.getSnapshot(0).ansi + '\x1b[6n');
    assert.equal(replies.length, 1);
  } finally { e.dispose(); }
});
test('normal snapshot preserves wrap-pending and subsequent wide text', async () => {
  const a = new HeadlessEmulator(10, 4), b = new HeadlessEmulator(10, 4);
  try {
    await a.write('1234567890');
    await b.write(a.getSnapshot(1).ansi);
    await a.write('汉👩‍💻!'); await b.write('汉👩‍💻!');
    assert.deepEqual(state(b), state(a));
  } finally { a.dispose(); b.dispose(); }
});

test('ConPTY DA1 is answered exactly once by the live host, including split startup queries', async () => {
  const replies = [];
  const e = new HeadlessEmulator(80, 24, reply => replies.push(reply));
  try {
    e.installConptyPrimaryDeviceAttributesOverride();
    e.installConptyPrimaryDeviceAttributesOverride();
    await e.write('\x1b[c\x1b[0c');
    assert.deepEqual(replies, []);
    await e.write('\x1b[', { forwardQueryReplies: true });
    await e.write('c', { forwardQueryReplies: true });
    await e.write('\x1b[0c', { forwardQueryReplies: true });
    assert.deepEqual(replies, ['\x1b[?61;4c', '\x1b[?61;4c']);
    await e.write('\x1b[1c', { forwardQueryReplies: true });
    assert.equal(replies.length, 2);
  } finally { e.dispose(); }
});
