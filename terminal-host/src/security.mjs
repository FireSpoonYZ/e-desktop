import { randomBytes, createHash, timingSafeEqual, X509Certificate } from 'node:crypto';
import { mkdirSync, chmodSync, existsSync, readFileSync, writeFileSync, renameSync } from 'node:fs';
import { join } from 'node:path';
import { userInfo } from 'node:os';
import { execFileSync } from 'node:child_process';
import selfsigned from 'selfsigned';

export const secret = () => randomBytes(32).toString('base64url');
export const hash = value => createHash('sha256').update(value).digest('hex');
export function equal(a, b) {
  const left = Buffer.from(a), right = Buffer.from(b);
  return left.length === right.length && timingSafeEqual(left, right);
}
export function privateDirectory(path) {
  mkdirSync(path, { recursive: true, mode: 0o700 });
  if (process.platform === 'win32') {
    execFileSync('icacls.exe', [path, '/inheritance:r', '/grant:r',
      userInfo().username + ':(OI)(CI)F', '*S-1-5-18:(OI)(CI)F'], { stdio: 'pipe' });
  } else chmodSync(path, 0o700);
}
export function saveJson(path, value) {
  const temp = path + '.tmp';
  writeFileSync(temp, JSON.stringify(value), { mode: 0o600 });
  renameSync(temp, path);
}
export async function credentials(dataDir) {
  privateDirectory(dataDir);
  const tlsPath = join(dataDir, 'tls.json');
  let tls;
  if (existsSync(tlsPath)) tls = JSON.parse(readFileSync(tlsPath, 'utf8'));
  else {
    const generated = await selfsigned.generate([{ name: 'commonName', value: 'e-terminal' }],
      { notAfterDate: new Date(Date.now() + 3650 * 86400000), keySize: 2048, algorithm: 'sha256' });
    tls = { key: generated.private, cert: generated.cert };
    saveJson(tlsPath, tls);
  }
  const fingerprint = hash(new X509Certificate(tls.cert).raw);
  const devicePath = join(dataDir, 'devices.json');
  const devices = existsSync(devicePath) ? JSON.parse(readFileSync(devicePath, 'utf8')) : [];
  if (!Array.isArray(devices) || devices.some(d => typeof d.id !== 'string' ||
    typeof d.name !== 'string' || !/^[a-f0-9]{64}$/.test(d.tokenHash))) throw new Error('Invalid devices.json');
  return { tls, fingerprint, devices, saveDevices: () => saveJson(devicePath, devices) };
}
