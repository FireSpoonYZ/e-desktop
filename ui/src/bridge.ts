import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { Command, Snapshot } from './model';

export const getSnapshot = () => invoke<Snapshot>('get_snapshot');
export const execute = (command: Command) => invoke<Snapshot>('execute', { command });
export const onSnapshot = (handler: (snapshot: Snapshot) => void) =>
  listen<Snapshot>('snapshot', (event) => handler(event.payload));
