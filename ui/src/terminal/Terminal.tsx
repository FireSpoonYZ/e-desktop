import { useEffect, useRef, useState } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { Unicode11Addon } from '@xterm/addon-unicode11';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { activateOrcaTerminalUnicodeProvider } from './orca/terminal-unicode-provider';
import { attachTerminalInputAuthority } from './input-authority';
import { createTerminalStream } from './stream';
import { CloseSessionButton } from './CloseSessionButton';
import { TerminalClient, openTerminal, observeTerminalConnection, TerminalRequestError } from './client';
import type { Profile, Session, Snapshot } from './client';
import '@xterm/xterm/css/xterm.css';
import './terminal.css';

const message = (error: unknown) => error instanceof Error ? error.message : String(error);
function useConnection() {
  const [client, setClient] = useState<TerminalClient | null>(null);
  const [error, setError] = useState('');
  const [attempt, setAttempt] = useState(0);
  const [retrying, setRetrying] = useState(true);
  useEffect(() => {
    setClient(null); setError(''); setRetrying(true);
    return observeTerminalConnection(connection => {
      setClient(connection); setError(''); setRetrying(false);
    }, (cause, willRetry) => {
      setClient(null); setError(message(cause)); setRetrying(willRetry);
    });
  }, [attempt]);
  return { client, error, setError, retrying, reconnect: () => setAttempt(value => value + 1) };
}
function ConnectionNotice({ error, reconnect }: { error: string; reconnect: () => void }) {
  return error ? <div className="terminal-notice" role="alert">{error} <button onClick={reconnect}>重新连接</button></div> : null;
}

export function TerminalManager() {
  const { client, error, setError, retrying, reconnect } = useConnection();
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [sessions, setSessions] = useState<Session[]>([]);
  const [devices, setDevices] = useState<Array<{ id: string; name: string }>>([]);
  const [profileId, setProfileId] = useState('');
  const [cwd, setCwd] = useState('');
  const [executable, setExecutable] = useState('');
  const [pairing, setPairing] = useState<{ descriptor: string; expiresAt: number } | null>(null);
  const [busy, setBusy] = useState(false);
  const currentClient = useRef(client);
  currentClient.current = client;
  const profile = profiles.find(profile => profile.id === profileId);
  async function refresh(connection = client) {
    if (!connection) return;
    const [nextProfiles, nextSessions, nextDevices] = await Promise.all([
      connection.request<Profile[]>('profiles.list'), connection.request<Session[]>('terminal.list'),
      connection.request<Array<{ id: string; name: string }>>('devices.list'),
    ]);
    if (connection !== currentClient.current) return;
    setProfiles(nextProfiles); setSessions(nextSessions); setDevices(nextDevices);
    setProfileId(current => current || nextProfiles.find(profile => profile.available)?.id || nextProfiles[0]?.id || '');
  }
  useEffect(() => {
    if (!client) return;
    let active = true;
    const update = () => void refresh(client).catch(error => { if (active) setError(message(error)); });
    update();
    const unlisten = client.onEvent(event => { if (event.event === 'terminal.listChanged') update(); });
    return () => { active = false; unlisten(); };
  }, [client]);
  async function act(action: () => Promise<unknown>) {
    const connection = client;
    setBusy(true); setError('');
    try { await action(); } catch (error) { if (connection === currentClient.current) setError(message(error)); } finally { setBusy(false); }
  }
  return <main className="terminal-manager">
    <header className="terminal-manager-header"><div><h1>终端</h1><p className="muted">独立会话 · 关闭窗口后继续运行</p></div>
      <span className="terminal-status" role="status">{client ? '已连接' : retrying ? '正在重连…' : '离线'}</span>
      <button disabled={!client || busy} onClick={() => void act(() => refresh())}>刷新</button></header>
    <ConnectionNotice error={error} reconnect={reconnect} />
    {!client && !error && <p role="status">正在启动终端服务…</p>}
    <section aria-labelledby="terminal-create"><h2 id="terminal-create">新建会话</h2>
      <form onSubmit={event => { event.preventDefault(); if (client) void act(async () => {
        await client.request<Session>('terminal.create', {
          profileId, ...(cwd.trim() ? { cwd: cwd.trim() } : {}),
          ...(executable.trim() ? { executable: executable.trim() } : {}),
        });
        await refresh(); // The primary topbar opens each new session, including phone creations.
      }); }}>
        <label>Shell<select value={profileId} onChange={event => { setProfileId(event.target.value); setExecutable(''); }}>
          {profiles.map(profile => <option key={profile.id} value={profile.id}>{profile.name}{profile.available ? '' : '（未找到）'}</option>)}
        </select></label>
        <label>工作目录（可选）<input value={cwd} onChange={event => setCwd(event.target.value)} placeholder="默认主目录" /></label>
        {profile && !profile.available && <label>{profile.reason || '未找到可执行文件'} — 绝对路径
          <input value={executable} onChange={event => setExecutable(event.target.value)} placeholder="指定已安装的 shell 可执行文件" /></label>}
        <button className="terminal-primary" disabled={!client || busy || !profile || (!profile.available && !executable.trim())}>新建终端窗口</button>
      </form>
    </section>
    <section className="terminal-sessions" aria-labelledby="terminal-sessions"><h2 id="terminal-sessions">会话 <span className="muted">{sessions.length}</span></h2>
      {!sessions.length && <div className="terminal-empty"><strong>{client ? '还没有终端会话' : '等待终端服务'}</strong><p className="muted">{client ? '选择上方 shell 创建会话，也可以从已配对手机创建。' : '连接后将在这里显示会话；未发送的输入不会自动重试。'}</p></div>}
      <ul>{sessions.map(session => <li key={session.id}><div><strong>{session.title}</strong>
        <small>{session.profileId} · {session.cwd} · {session.status === 'running' ? '运行中' : `已退出 (${session.exitCode ?? '?'})`}
          {session.ownerClientId ? ' · 已有控制端' : ' · 无控制端'}</small></div>
        <button onClick={() => void act(() => openTerminal(session.id))} disabled={busy}>打开窗口</button>
        <details className="terminal-menu"><summary aria-label={`${session.title} 的更多操作`}>更多</summary><div className="terminal-menu-panel"><CloseSessionButton session={session} disabled={!client || busy} onClose={() => {
          if (client) void act(async () => { await client.request('terminal.close', { sessionId: session.id }); await refresh(); });
        }} /></div></details></li>)}</ul>
    </section>
    <details className="terminal-pair"><summary>手机配对与设备 <span className="muted">{devices.length} 台已配对</span></summary><section aria-label="手机配对与设备">
      <p className="muted">同一局域网 / Tailscale。配对信息含一次性密钥，请只交给自己的设备。</p>
      <button disabled={!client || busy} onClick={() => client && void act(async () => {
        setPairing(await client.request('pairing.create'));
      })}>生成 5 分钟配对信息</button>
      {pairing && <div><p>有效期至 {new Date(pairing.expiresAt).toLocaleTimeString()}（仅可用一次）</p>
        <textarea aria-label="一次性配对信息" readOnly value={pairing.descriptor} rows={5} />
        <button onClick={() => void act(() => navigator.clipboard.writeText(pairing.descriptor))}>复制配对信息</button>
        <button onClick={() => setPairing(null)}>隐藏</button></div>}
      <h3>已配对设备</h3>{!devices.length && <p className="muted">尚未配对设备</p>}<ul>{devices.map(device => <li key={device.id}>{device.name}
        <button className="terminal-danger" disabled={!client || busy} onClick={() => {
          if (client && window.confirm(`撤销 ${device.name} 的终端访问权限？`)) void act(async () => {
            await client.request('devices.revoke', { deviceId: device.id }); await refresh();
          });
        }}>撤销</button></li>)}</ul>
      <p className="muted">退出 e-desktop 会结束所有托管 shell。需要 Node.js 22+。</p>
    </section></details>
  </main>;
}

export function TerminalView({ sessionId }: { sessionId: string }) {
  const { client, error, setError, retrying, reconnect } = useConnection();
  const container = useRef<HTMLDivElement>(null);
  const currentClient = useRef(client);
  currentClient.current = client;
  const [loadedClient, setLoadedClient] = useState<TerminalClient | null>(null);
  const [missing, setMissing] = useState(false);
  const ready = !!client && loadedClient === client && !missing;
  const [session, setSession] = useState<Session | null>(null);
  const [owner, setOwner] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [fontSize, setFontSize] = useState(14);
  const terminalRef = useRef<Terminal | null>(null);
  const fitRef = useRef<(() => void) | null>(null);
  const owned = ready && owner === client?.clientId && session?.status === 'running';
  const ownerRef = useRef(false);
  ownerRef.current = owned;

  useEffect(() => {
    if (!container.current) return;
    const theme = getComputedStyle(container.current);
    const term = new Terminal({
      allowProposedApi: true, vtExtensions: { kittyKeyboard: true },
      fontFamily: '"Cascadia Code", Consolas, monospace', fontSize, cursorBlink: true,
      scrollback: 10000, theme: { background: theme.getPropertyValue('--background').trim(), foreground: theme.getPropertyValue('--text').trim() },
    });
    terminalRef.current = term;
    term.loadAddon(new Unicode11Addon());
    activateOrcaTerminalUnicodeProvider(term);
    term.open(container.current);
    setSession(null); setOwner(null);
    return () => { term.dispose(); terminalRef.current = null; };
  }, [sessionId]);

  useEffect(() => {
    if (!client || !container.current || !terminalRef.current) return;
    let active = true;
    let subscribed = false;
    const subscriptionId = crypto.randomUUID();
    setMissing(false);
    const connection = client;
    let resizeTimer: ReturnType<typeof setTimeout> | undefined;
    let lastViewport = '';
    const term = terminalRef.current;
    const fit = new FitAddon();
    term.loadAddon(fit);
    const fail = (cause: unknown) => { if (active) setError(message(cause)); };
    const stream = createTerminalStream(term, scheduleFit, cause => { fail(cause); reconnect(); });
    const send = (data: string) => {
      if (ownerRef.current && stream.ready)
        void client.request('terminal.send', { sessionId, data }).catch(fail);
    };
    let detachInput: () => void;
    try { detachInput = attachTerminalInputAuthority(term, send); }
    catch (cause) { stream.dispose(); fit.dispose(); fail(cause); return; }
    const binary = term.onBinary(data => {
      if (ownerRef.current && stream.ready)
        void connection.request('terminal.send', { sessionId, data, encoding: 'binary' }).catch(fail);
    });
    function viewport() {
      if (!active || !subscribed || !ownerRef.current || !stream.ready) return;
      const size = fit.proposeDimensions();
      if (!size || size.cols < 2 || size.rows < 1) return;
      const key = `${size.cols}x${size.rows}`;
      if (key === lastViewport) return;
      lastViewport = key;
      void connection.request('terminal.updateViewport', { sessionId, cols: size.cols, rows: size.rows }).catch(cause => { lastViewport = ''; fail(cause); });
    }
    function scheduleFit() {
      clearTimeout(resizeTimer);
      resizeTimer = setTimeout(viewport, 100);
    }
    fitRef.current = scheduleFit;
    const observer = new ResizeObserver(scheduleFit);
    observer.observe(container.current);
    const unlisten = client.onEvent(event => {
      if (!active || event.sessionId !== sessionId) return;
      if (event.event === 'terminal.output' && typeof event.data === 'string' && typeof event.seq === 'number') {
        stream.output(event.data, event.seq);
      } else if (event.event === 'terminal.snapshot' && event.snapshot) stream.snapshot(event.snapshot);
      else if (event.event === 'terminal.control') { setOwner(event.ownerClientId ?? null); lastViewport = ''; scheduleFit(); }
      else if (event.event === 'terminal.exit') setSession(current => current && ({ ...current, status: 'exited', exitCode: event.exitCode }));
    });
    void client.request<{ session: Session; snapshot: Snapshot }>('terminal.subscribe', { sessionId, subscriptionId })
      .then(value => {
        if (!active) return;
        subscribed = true; setLoadedClient(client); setSession(value.session); setOwner(value.session.ownerClientId);
        stream.snapshot(value.snapshot);
      }).catch(cause => {
        if (!active) return;
        if (cause instanceof TerminalRequestError && cause.code === 'NOT_FOUND') {
          setMissing(true); setSession(null); setOwner(null);
          setError('此会话已结束或服务已重启。请从会话列表新建终端。');
        } else fail(cause);
      });
    return () => {
      active = false; stream.dispose(); observer.disconnect(); clearTimeout(resizeTimer); unlisten();
      detachInput(); binary.dispose(); fit.dispose(); fitRef.current = null;
      // Retire even a pending subscribe; its ID cannot cancel a newer registration.
      void client.request('terminal.unsubscribe', { sessionId, subscriptionId }).catch(() => {});
    };
  }, [client, sessionId]);
  useEffect(() => {
    if (terminalRef.current) terminalRef.current.options.fontSize = fontSize;
    fitRef.current?.();
  }, [fontSize]);
  async function act(method: string) {
    if (!client || !ready) return;
    setBusy(true);
    try {
      const result = await client.request<{ ownerClientId?: string }>(method, { sessionId });
      if (client !== currentClient.current) return;
      if (method === 'terminal.claim') { setOwner(result.ownerClientId ?? client.clientId); terminalRef.current?.focus(); fitRef.current?.(); }
      if (method === 'terminal.release') setOwner(null);
      if (method === 'terminal.close') { setSession(null); setOwner(null); setMissing(true); }
    } catch (cause) { if (client === currentClient.current) setError(message(cause)); } finally { setBusy(false); }
  }
  return <main className="terminal-view">
    <header className="terminal-toolbar"><strong>{session?.title || '终端'}</strong>
      <span role="status">{!client ? (retrying ? '正在重连…' : '离线') : missing ? '会话已结束' : !ready || !session ? '正在读取会话…' : session.status === 'exited' ? `已退出 (${session.exitCode ?? '?'})` : owned ? '正在控制' : owner ? '只读 · 其他端正在控制' : '只读 · 可接管'}</span>
      <button disabled={!ready || busy || session?.status !== 'running'} onClick={() => void act(owned ? 'terminal.release' : 'terminal.claim')}>{owned ? '释放控制' : '接管输入'}</button>
      <details className="terminal-menu"><summary aria-label="终端更多操作">更多</summary><div className="terminal-menu-panel">
      <button onClick={() => setFontSize(size => Math.max(8, size - 1))} aria-label="缩小字体">A−</button>
      <button onClick={() => setFontSize(size => Math.min(32, size + 1))} aria-label="放大字体">A+</button>
      <button onClick={() => void openTerminal().catch(cause => setError(message(cause)))}>会话 / 配对</button>
      <button onClick={() => void getCurrentWindow().close().catch(cause => setError(message(cause)))}>分离窗口</button>
      <CloseSessionButton session={session} disabled={!ready || busy} onClose={() => void act('terminal.close')} />
      </div></details>
    </header>
    {missing ? <div className="terminal-notice" role={error ? 'alert' : 'status'}><span>{error || '此会话已结束或不存在。原有输出保留在下方。'}</span>
      <button onClick={() => void openTerminal().catch(cause => setError(message(cause)))}>返回会话列表</button>
    </div> : <ConnectionNotice error={error} reconnect={reconnect} />}
    {!client && !error && <p role="status">正在连接…</p>}
    <div ref={container} className="terminal-canvas" aria-label="终端输出与键盘输入" />
  </main>;
}
