import { WebSocket } from 'ws';
import { once } from 'node:events';
import { hash } from '../src/security.mjs';
export async function client(url, tls) {
  const ws = new WebSocket(url, tls ? {
    ca: tls.cert, rejectUnauthorized: true,
    checkServerIdentity: (_hostname, cert) => hash(cert.raw) === tls.fingerprint ? undefined : new Error('Pin mismatch')
  } : {});
  const messages = [], pending = new Map();
  let id = 0;
  ws.on('message', data => {
    const message = JSON.parse(data); messages.push(message);
    if (message.id && pending.has(message.id)) {
      const { resolve, reject, timer } = pending.get(message.id);
      pending.delete(message.id); clearTimeout(timer);
      message.error ? reject(Object.assign(new Error(message.error.message), message.error)) : resolve(message.result);
    }
  });
  await once(ws, 'open');
  return { ws, messages,
    request(method, params = {}) {
      const requestId = String(++id);
      return new Promise((resolve, reject) => {
        const timer = setTimeout(() => { pending.delete(requestId); reject(new Error('Request timed out: ' + method)); }, 10000);
        pending.set(requestId, { resolve, reject, timer });
        ws.send(JSON.stringify({ id: requestId, method, params }));
      });
    },
    close() { ws.terminate(); }
  };
}
export async function until(predicate, timeout = 15000) {
  const deadline = Date.now() + timeout;
  while (!predicate()) {
    if (Date.now() > deadline) throw new Error('Timed out waiting for terminal event');
    await new Promise(resolve => setTimeout(resolve, 25));
  }
}
