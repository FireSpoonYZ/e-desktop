import { invoke, isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { Command, Snapshot } from './model';
import type { PreviewSlot, PreviewStatus } from './overview/previews';

export type Surface = 'overview' | 'commands';
export const desktopAvailable = isTauri;
export const getSnapshot = () => invoke<Snapshot>('get_snapshot');
export const execute = (command: Command) => invoke<Snapshot>('execute', { command });
export const openSurface = (surface: Surface, monitorId?: string) => invoke<void>('open_surface', { surface, monitorId });
export const dismissSurface = (surface: Surface) => invoke<void>('dismiss_surface', { surface });
export const quit = () => invoke<void>('quit');
export const syncPreviews = (session: number, slots: PreviewSlot[]) => invoke<PreviewStatus[]>('sync_previews', { session, slots });
export const onSnapshot = (handler: (snapshot: Snapshot) => void) =>
  listen<Snapshot>('snapshot', (event) => handler(event.payload));
interface SurfaceOpening { monitorId: string | null; previewSession: number | null }
export const onSurfaceOpened = (handler: (opening: SurfaceOpening) => void) =>
  listen<SurfaceOpening>('surface-opened', (event) => handler(event.payload));
