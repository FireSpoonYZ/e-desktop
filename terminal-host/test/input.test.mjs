import test from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { PassThrough } from 'node:stream';
import { terminalInput } from '../src/server.mjs';

test('binary terminal input preserves all 256 byte values and enforces byte limits', () => {
  const data = Array.from({ length: 256 }, (_, n) => String.fromCharCode(n)).join('');
  const bytes = terminalInput(data, 'binary');
  assert.ok(Buffer.isBuffer(bytes));
  assert.deepEqual([...bytes], Array.from({ length: 256 }, (_, n) => n));
  assert.equal(terminalInput('汉字'), '汉字');
  assert.equal(terminalInput('\xff'.repeat(65536), 'binary').length, 65536);
  for (const invalid of ['\u0100', '汉', '😀', '\ud800', 'x'.repeat(65537)]) {
    assert.throws(() => terminalInput(invalid, 'binary'), { code: 'INVALID_PARAMS' });
  }
  assert.throws(() => terminalInput('\xff'.repeat(65536)), { code: 'INVALID_PARAMS' });
  assert.throws(() => terminalInput('', 'utf8'), { code: 'INVALID_PARAMS' });
  assert.throws(() => terminalInput('', 'binary', 'unsupported'), { code: 'UNSUPPORTED_ENCODING' });
});

test('installed Windows node-pty write(Buffer) forwards X10 high bytes unchanged to its input socket',
  { skip: process.platform !== 'win32' }, () => {
    const require = createRequire(import.meta.url);
    const { WindowsTerminal } = require('node-pty/lib/windowsTerminal.js');
    // Exercise the installed public write -> _write -> _doWrite implementation,
    // replacing only the OS pipe with a byte stream. No shell consumes mouse bytes.
    const terminal = Object.create(WindowsTerminal.prototype);
    terminal._isReady = true;
    terminal._agent = { inSocket: new PassThrough() };
    const bytes = terminalInput('\x1b[M \x80\xff', 'binary');
    terminal.write(bytes);
    assert.deepEqual(terminal._agent.inSocket.read(), Buffer.from([27, 91, 77, 32, 128, 255]));
    terminal._agent.inSocket.destroy();
  });
