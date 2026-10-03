import { invoke, isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { Command, Snapshot } from './model';
import type { PreviewSlot, PreviewStatus } from './overview/previews';
import type { Hotkey } from './hotkeys/describe';

export type Surface = 'overview' | 'commands' | 'hotkeys';
export const desktopAvailable = isTauri;
export const getSnapshot = () => invoke<Snapshot>('get_snapshot');
export const execute = (command: Command) => invoke<Snapshot>('execute', { command });
export const openSurface = (surface: Surface, monitorId?: string) => invoke<void>('open_surface', { surface, monitorId });
export const dismissSurface = (surface: Surface) => invoke<void>('dismiss_surface', { surface });
export const setBarPinned = (monitorId: string, pinned: boolean) => invoke<void>('set_bar_pinned', { monitorId, pinned });
export const quit = () => invoke<void>('quit');
export const syncPreviews = (session: number, slots: PreviewSlot[]) => invoke<PreviewStatus[]>('sync_previews', { session, slots });
export const onSnapshot = (handler: (snapshot: Snapshot) => void) =>
  listen<Snapshot>('snapshot', (event) => handler(event.payload));
/** `hotkeys` (lane: ui-animation) is the effective binding list, sent only to the hotkey overlay. */
interface SurfaceOpening { monitorId: string | null; previewSession: number | null; hotkeys?: Hotkey[] | null }
export const onSurfaceOpened = (handler: (opening: SurfaceOpening) => void) =>
  listen<SurfaceOpening>('surface-opened', (event) => handler(event.payload));
