import type { Column, WindowId } from '../model';

/** lane: tabbed. The window a tabbed column shows; null lays the column out normally (as the engine does). */
export function shownTab(column: Column): WindowId | null {
  if (column.display !== 'tabbed' || column.windows.length < 2) return null;
  return column.activeTab && column.windows.includes(column.activeTab) ? column.activeTab : column.windows[0];
}
