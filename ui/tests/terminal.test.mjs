import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync } from 'node:fs';
import ts from 'typescript';

function moduleUrl(path) {
  const file = new URL(path, import.meta.url);
  let { outputText } = ts.transpileModule(readFileSync(file, 'utf8'), {
    compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022, jsx: ts.JsxEmit.ReactJSX },
  });
  outputText = outputText.replace(/from ['"]([^'"]+)['"]/g,
    (_, specifier) => `from '${specifier.startsWith('.')
      ? moduleUrl(new URL(specifier + '.ts', file).href) : import.meta.resolve(specifier)}'`);
  return `data:text/javascript;base64,${Buffer.from(outputText).toString('base64')}`;
}
const { createTerminalStream } = await import(moduleUrl('../src/terminal/stream.ts'));
const { extractOnlyTerminalQueryReplies } = await import(moduleUrl('../src/terminal/orca/terminal-query-reply.ts'));
const { parseTerminalKittyKeyboardFlags } = await import(moduleUrl('../src/terminal/orca/terminal-kitty-keyboard-flags.ts'));
const { activateOrcaTerminalUnicodeProvider } = await import(moduleUrl('../src/terminal/orca/terminal-unicode-provider.ts'));

test('terminal stream orders snapshot, live output, resize snapshot and partial escape completion', async () => {
  const operations = [], callbacks = [], errors = [];
  const terminal = {
    reset: () => operations.push('reset'),
    resize: (cols, rows) => operations.push([cols, rows]),
    write: (data, done) => { operations.push(data); callbacks.push(done); },
  };
  let readyCount = 0;
  const stream = createTerminalStream(terminal, () => readyCount++, error => errors.push(error.message));
  const snapshot = { ansi: 'normal + alternate\x1b[', cols: 80, rows: 24, seq: 10, kittyKeyboardFlags: 8 };
  stream.snapshot(snapshot);
  stream.output('31mfirst', 11);
  stream.output('duplicate ignored', 11);
  stream.snapshot({ ...snapshot, ansi: 'resized\x1b[', cols: 100, seq: 11 });
  stream.output('32msecond', 12);
  assert.equal(stream.ready, false);
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(operations, ['reset', [80, 24], '\x1b[=8unormal + alternate\x1b[']);
  callbacks.shift()();
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(operations.at(-1), '31mfirst');
  assert.equal(stream.ready, false);
  callbacks.shift()();
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(operations.slice(-3), ['reset', [100, 24], '\x1b[=8uresized\x1b[']);
  callbacks.shift()();
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(operations.at(-1), '32msecond');
  callbacks.shift()();
  assert.equal(stream.ready, true);
  assert.equal(readyCount, 1);
  assert.deepEqual(errors, []);
  stream.output('gap', 14);
  assert.match(errors[0], /output gap/);
  assert.equal(stream.ready, false);
});

test('terminal stream bounds renderer backlog and rejects invalid snapshots', () => {
  const errors = [];
  const terminal = { reset() {}, resize() {}, write() {} };
  const stream = createTerminalStream(terminal, () => {}, error => errors.push(error.message));
  stream.snapshot({ ansi: '', cols: 80, rows: 24, seq: 0 });
  stream.output('x'.repeat(8 * 1024 * 1024 + 1), 1);
  assert.match(errors[0], /overloaded/);
  const other = createTerminalStream(terminal, () => {}, error => errors.push(error.message));
  other.snapshot({ ansi: '', cols: Infinity, rows: 24, seq: 0 });
  assert.match(errors[1], /Invalid terminal snapshot/);
});

test('Orca query authority drops parser replies but preserves keys, paste and SGR mouse', () => {
  for (const data of ['\x1b[1;2R', '\x1b[?1;2c', '\x1b[?8u', '\x1b[0n\x1b[1;1R', '\x1b]10;rgb:ffff/ffff/ffff\x1b\\']) {
    assert.ok(extractOnlyTerminalQueryReplies(data), JSON.stringify(data));
  }
  for (const data of ['中文', '\x1b[A', '\x1b[13;5u', '\x1b[<0;20;30M', '\x1b[200~paste\x1b[201~']) {
    assert.equal(extractOnlyTerminalQueryReplies(data), null);
  }
  assert.equal(parseTerminalKittyKeyboardFlags(undefined), undefined);
  assert.equal(parseTerminalKittyKeyboardFlags('0'), undefined);
  assert.equal(parseTerminalKittyKeyboardFlags(0), 0);
  assert.equal(parseTerminalKittyKeyboardFlags(8), 8);
});

test('Orca Unicode provider keeps ZWJ emoji at the preceding wide pair', () => {
  const base = { version: '11', wcwidth: cp => cp === 0x200d ? 0 : 2, charProperties: cp => cp << 3 | 4 };
  let provider;
  const terminal = {
    unicode: { activeVersion: '11', versions: ['11'], register(value) { provider = value; } },
    _core: { unicodeService: { _providers: { '11': base } } },
  };
  activateOrcaTerminalUnicodeProvider(terminal);
  assert.equal(terminal.unicode.activeVersion, 'orca-11-zwj');
  const joined = provider.charProperties(0x200d, base.charProperties(0x1f469));
  const next = provider.charProperties(0x1f4bb, joined);
  assert.equal((next >> 1) & 3, 2);
  assert.equal(next & 1, 1);
});

test('real pinned xterm core distinguishes identical modified F3/CPR and blocks replay replies', async () => {
  const { Terminal } = await import('../../node_modules/@xterm/xterm/lib/xterm.mjs');
  const { attachTerminalInputAuthority } = await import(moduleUrl('../src/terminal/input-authority.ts'));
  const terminal = new Terminal({ allowProposedApi: true });
  const core = terminal._core.coreService;
  const original = core.triggerDataEvent;
  const forwarded = [];
  const detach = attachTerminalInputAuthority(terminal, data => {
    forwarded.push(data);
    if (data === 'nested') core.triggerDataEvent('\x1b[1;2R', false);
  });
  core.triggerDataEvent('\x1b[1;2R', false); // parser CPR
  core.triggerDataEvent('\x1b[1;2R', true); // actual modified F3
  terminal.input('中文输入', true);
  terminal.input('\x1b[200~paste\x1b[201~', true);
  core.triggerDataEvent('nested', true);
  assert.deepEqual(forwarded, ['\x1b[1;2R', '中文输入', '\x1b[200~paste\x1b[201~', 'nested']);
  await new Promise(resolve => terminal.write('\x1b[6n\x1b[c\x1b[?u', resolve));
  assert.equal(forwarded.length, 4, 'CPR, DA and kitty queries in snapshot/live output stay local');
  core.triggerDataEvent('\x1b[0n', false);
  assert.equal(forwarded.length, 4, 'reentrant user provenance is restored');
  detach();
  assert.equal(core.triggerDataEvent, original);
  assert.throws(() => attachTerminalInputAuthority({}, () => {}), /Unsupported xterm input core/);
  terminal.dispose();
});


const { observeSessionWindows } = await import(moduleUrl('../src/terminal/session-windows.ts'));
const { CloseSessionButton } = await import(moduleUrl('../src/terminal/CloseSessionButton.tsx'));
const tick = () => new Promise(resolve => setImmediate(resolve));
function sessionSource(initial = []) {
  let sessions = initial;
  let listener;
  return {
    client: {
      async request(method) { assert.equal(method, 'terminal.list'); return sessions; },
      onEvent(handler) { listener = handler; return () => { listener = undefined; }; },
    },
    update(next) { sessions = next; listener?.({ event: 'terminal.listChanged' }); },
  };
}

test('primary session observer opens new running sessions once, not exited or manually detached sessions', async () => {
  const seen = new Set(), opened = [], errors = [];
  const phone = { id: 'phone', status: 'running' };
  const exited = { id: 'exited', status: 'exited' };
  const source = sessionSource([exited]);
  const observe = () => observeSessionWindows(source.client, seen, async id => { opened.push(id); }, error => errors.push(error));
  let stop = observe();
  await tick();
  assert.deepEqual(opened, []);
  source.update([exited, phone]); // No manager or terminal view needs to be mounted.
  source.update([exited, phone]);
  await tick();
  assert.deepEqual(opened, ['phone']);
  // Native view detach deliberately does not alter host sessions.
  source.update([phone, exited]);
  await tick();
  assert.deepEqual(opened, ['phone']);
  stop();
  stop = observe(); // reconnect/effect replay shares the seen IDs
  await tick();
  assert.deepEqual(opened, ['phone']);
  source.update([phone, exited, { id: 'phone-next', status: 'running' }]);
  await tick();
  assert.deepEqual(opened, ['phone', 'phone-next']);
  stop();
  source.update([{ id: 'after-dispose', status: 'running' }]);
  await tick();
  assert.deepEqual(opened, ['phone', 'phone-next']);
  assert.deepEqual(errors, []);
});

test('session observer reserves IDs before async opening and ignores a disposed initial list', async () => {
  let finishOpen;
  const source = sessionSource([{ id: 'first', status: 'running' }]);
  const opened = [];
  const stop = observeSessionWindows(source.client, new Set(), id => {
    opened.push(id); return new Promise(resolve => { finishOpen = resolve; });
  }, error => { throw error; });
  await tick();
  source.update([{ id: 'first', status: 'running' }]);
  source.update([{ id: 'first', status: 'running' }]);
  finishOpen();
  await tick();
  assert.deepEqual(opened, ['first']);
  stop();
  const cancel = observeSessionWindows(source.client, new Set(), async id => { opened.push(id); }, error => { throw error; });
  cancel();
  await tick();
  assert.deepEqual(opened, ['first']);
});

test('both desktop close surfaces share removal of exited sessions without process-close confirmation', async () => {
  const previousWindow = globalThis.window;
  let confirmations = 0, accepted = false, closes = 0;
  globalThis.window = { confirm() { confirmations++; return accepted; } };
  try {
    const running = { id: 'shell', status: 'running' };
    let entry = running;
    const onClose = () => { closes++; entry = null; };
    let button = CloseSessionButton({ session: entry, disabled: false, onClose });
    assert.equal(button.props.children, '结束 shell');
    button.props.onClick();
    assert.equal(closes, 0);
    accepted = true;
    button.props.onClick();
    assert.equal(closes, 1);
    entry = { ...running, status: 'exited' }; // natural exit retains a host slot
    button = CloseSessionButton({ session: entry, disabled: false, onClose });
    assert.equal(button.props.children, '移除会话');
    assert.equal(button.props.disabled, false);
    button.props.onClick();
    assert.equal(confirmations, 2, 'exited removal must not ask to kill a process');
    assert.equal(entry, null, 'removal action is reachable after natural exit');
    entry = { id: 'replacement', status: 'running' }; // newly available slot can be used
    assert.equal(CloseSessionButton({ session: entry, disabled: false, onClose }).props.children, '结束 shell');
    for (const props of [{ session: entry, disabled: true }, { session: null, disabled: false }]) {
      button = CloseSessionButton({ ...props, onClose });
      assert.equal(button.props.disabled, true);
      button.props.onClick();
    }
    assert.equal(closes, 2);
    const source = readFileSync(new URL('../src/terminal/Terminal.tsx', import.meta.url), 'utf8');
    assert.equal(source.match(/<CloseSessionButton /g).length, 2);
    assert.match(source, /client.request\('terminal.close', \{ sessionId: session.id \}\)/);
    assert.match(source, /onClose=\{\(\) => void act\('terminal.close'\)\}/);
  } finally { globalThis.window = previousWindow; }
});

test('application-lifetime observation is gated to the single primary topbar', () => {
  const source = readFileSync(new URL('../src/App.tsx', import.meta.url), 'utf8');
  assert.match(source, /surface !== 'topbar' \|\| monitorIndex !== 0 \|\| !desktopAvailable\(\)/);
  assert.match(source, /observeSessionWindows\(current, seenTerminalSessions.current, openTerminal, report\)/);
  const manager = readFileSync(new URL('../src/terminal/Terminal.tsx', import.meta.url), 'utf8');
  assert.doesNotMatch(manager, /await refresh\(\); await openTerminal\(session.id\)/);
});

test('recovery retains terminal content until the replacement stream receives a snapshot', async () => {
  const operations = [];
  const term = { reset: () => operations.push('reset'), resize() {}, write(data, done) { operations.push(data); done(); } };
  const first = createTerminalStream(term, () => {}, error => { throw error; });
  first.snapshot({ ansi: 'retained output', cols: 80, rows: 24, seq: 9 });
  await tick(); first.dispose();
  const second = createTerminalStream(term, () => {}, error => { throw error; });
  first.output('stale', 10); await tick();
  assert.deepEqual(operations, ['reset', 'retained output']);
  assert.equal(second.ready, false);
  second.snapshot({ ansi: 'current snapshot', cols: 80, rows: 24, seq: 10 }); await tick();
  assert.deepEqual(operations, ['reset', 'retained output', 'reset', 'current snapshot']);
  second.dispose();
  const view = readFileSync(new URL('../src/terminal/Terminal.tsx', import.meta.url), 'utf8');
  assert.match(view, /return \(\) => \{ term.dispose\(\); terminalRef.current = null; \};\s*\}, \[sessionId\]\)/);
});


test('deferred desktop fitting runs when a slow snapshot finishes parsing', async () => {
  const callbacks=[]; let fits=0; let stream;
  const term={reset(){},resize(){},write(_data,done){callbacks.push(done);}};
  const viewport=()=>{if(stream.ready)fits++;};
  stream=createTerminalStream(term,viewport,assert.fail);
  stream.snapshot({ansi:'slow snapshot',cols:80,rows:24,seq:0});
  await new Promise(resolve=>setImmediate(resolve));
  viewport(); assert.equal(fits,0);
  callbacks.shift()(); await new Promise(resolve=>setImmediate(resolve));
  assert.equal(fits,1);
  const source=readFileSync(new URL('../src/terminal/Terminal.tsx',import.meta.url),'utf8');
  assert.match(source,/createTerminalStream\(term, scheduleFit,/);
});
