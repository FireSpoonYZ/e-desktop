import { accessSync, constants, statSync } from 'node:fs';
import { delimiter, join, isAbsolute } from 'node:path';
import { homedir } from 'node:os';

export function existingFile(path) {
  if (typeof path !== 'string' || !isAbsolute(path)) return false;
  try { accessSync(path, process.platform === 'win32' ? constants.F_OK : constants.X_OK); return statSync(path).isFile(); }
  catch { return false; }
}
function find(names, candidates = [], accept = () => true) {
  return [...candidates, ...(process.env.PATH ?? '').split(delimiter).flatMap(dir =>
    names.map(name => join(dir.replace(/^"|"$/g, ''), name)))].find(path => existingFile(path) && accept(path));
}
export function profiles() {
  const windows = process.platform === 'win32';
  const pf = process.env.ProgramFiles ?? 'C:\\Program Files';
  const definitions = [
    ...(windows ? [
      ['pwsh', 'PowerShell', ['pwsh.exe'], [join(pf, 'PowerShell', '7', 'pwsh.exe')]],
      ['powershell', 'Windows PowerShell', ['powershell.exe'], [join(process.env.SystemRoot ?? 'C:\\Windows', 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe')]]
    ] : []),
    ['bash', 'Bash', windows ? ['bash.exe'] : ['bash'], windows ? [
      join(pf, 'Git', 'bin', 'bash.exe'), join(process.env.LOCALAPPDATA ?? homedir(), 'Programs', 'Git', 'bin', 'bash.exe')
    ] : ['/bin/bash', '/usr/bin/bash']],
    ['nu', 'Nushell', [windows ? 'nu.exe' : 'nu'], []]
  ];
  return definitions.map(([id, name, names, candidates]) => {
    // Windows System32 bash is the legacy WSL launcher, not Git Bash.
    const executable = find(names, candidates, path =>
      !(windows && id === 'bash' && /[\\/]System32[\\/]/i.test(path)));
    return { id, name, executable: executable ?? '', available: !!executable,
      ...(!executable ? { reason: 'Install this shell on PATH, or provide an absolute executable path.' } : {}) };
  });
}
export function shellArgs(id) {
  return id === 'bash' ? ['--login', '-i'] : id === 'nu' ? ['--interactive'] : ['-NoLogo'];
}
