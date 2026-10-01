use super::*;

#[derive(Clone)]
pub(super) struct DisconnectedMonitor {
    pages: Vec<PageId>,
    active_page: PageId,
}

impl Engine {
    fn borrowed_page(&self, page: &str) -> bool {
        self.disconnected_monitors
            .values()
            .any(|monitor| monitor.pages.iter().any(|id| id == page))
    }

    pub(super) fn record_hotplug_move(&mut self, window: &str, page: &str) {
        self.hotplug_pinned.remove(window);
        if self.borrowed_page(page) {
            let (m, _) = self.page_index(page).unwrap();
            self.hotplug_pinned
                .insert(window.into(), self.snapshot.monitors[m].monitor.id.clone());
        }
    }

    pub(super) fn reconcile_monitors(
        &mut self,
        monitors: Vec<Monitor>,
        windows: &BTreeSet<WindowId>,
    ) {
        self.hotplug_pinned.retain(|id, _| windows.contains(id));
        let mut removed = vec![];
        for mut old in std::mem::take(&mut self.snapshot.monitors) {
            for page in &mut old.pages {
                for column in &mut page.columns {
                    column.windows.retain(|id| windows.contains(id));
                }
                page.columns.retain(|column| !column.windows.is_empty());
                page.floating_windows.retain(|id| windows.contains(id));
            }
            if monitors.iter().any(|m| m.id == old.monitor.id) {
                self.snapshot.monitors.push(old);
            } else {
                // Borrowed pages retain their first owner through further disconnections.
                let pages = old
                    .pages
                    .iter()
                    .filter(|p| !self.borrowed_page(&p.id))
                    .map(|p| p.id.clone())
                    .collect();
                self.disconnected_monitors.insert(
                    old.monitor.id.clone(),
                    DisconnectedMonitor {
                        pages,
                        active_page: old.active_page.clone(),
                    },
                );
                removed.push(old);
            }
        }
        for monitor in monitors {
            let viewport = self
                .viewports
                .get(&monitor.id)
                .copied()
                .unwrap_or(monitor.work_area);
            if let Some(existing) = self
                .snapshot
                .monitors
                .iter_mut()
                .find(|m| m.monitor.id == monitor.id)
            {
                existing.monitor = monitor;
                existing.viewport = viewport;
            } else {
                if !self.monitor_order.contains(&monitor.id) {
                    self.monitor_order.push(monitor.id.clone());
                }
                let page = self.page();
                self.snapshot.monitors.push(MonitorState {
                    monitor,
                    viewport,
                    active_page: page.id.clone(),
                    pages: vec![page],
                });
            }
        }
        self.snapshot
            .monitors
            .sort_by_key(|m| self.monitor_order.iter().position(|id| id == &m.monitor.id));
        let receiver = self
            .snapshot
            .monitors
            .iter()
            .position(|m| m.monitor.primary)
            .unwrap_or(0);
        for old in removed {
            for page in old.pages {
                if !empty(&page)
                    || self
                        .minimized_slots
                        .iter()
                        .any(|slot| slot.page_id == page.id)
                {
                    self.append_hotplug_page(receiver, page);
                }
            }
        }
        // Restore in session order, never in native enumeration order.
        for m in 0..self.snapshot.monitors.len() {
            let owner = self.snapshot.monitors[m].monitor.id.clone();
            let Some(saved) = self.disconnected_monitors.remove(&owner) else {
                continue;
            };
            let mut restored = vec![];
            for id in saved.pages {
                let mut page = if let Ok((host, p)) = self.page_index(&id) {
                    let mut page = self.snapshot.monitors[host].pages.remove(p);
                    // Explicit moves into a borrowed page belong to the chosen host, not its owner.
                    let pinned_owners: BTreeSet<_> = ids(&page)
                        .filter_map(|id| self.hotplug_pinned.get(id))
                        .filter(|id| *id != &owner)
                        .cloned()
                        .collect();
                    for chosen in pinned_owners {
                        let mut stayed = page.clone();
                        stayed.id = self.id("page");
                        for column in &mut stayed.columns {
                            column.id = self.id("column");
                            column
                                .windows
                                .retain(|id| self.hotplug_pinned.get(id) == Some(&chosen));
                        }
                        stayed.columns.retain(|c| !c.windows.is_empty());
                        stayed
                            .floating_windows
                            .retain(|id| self.hotplug_pinned.get(id) == Some(&chosen));
                        for id in ids(&stayed) {
                            self.hotplug_pinned.remove(id);
                            for column in &mut page.columns {
                                column.windows.retain(|w| w != id);
                            }
                            page.floating_windows.retain(|w| w != id);
                        }
                        let target = self.monitor_index(&chosen).unwrap_or(host);
                        if let Some(disconnected) = self.disconnected_monitors.get_mut(&chosen) {
                            disconnected.pages.push(stayed.id.clone());
                        }
                        self.append_hotplug_page(target, stayed);
                    }
                    page
                } else {
                    // Cleanup may coalesce empty pages; remember their identities, never stale windows.
                    Page {
                        id,
                        name: String::new(),
                        columns: vec![],
                        floating_windows: vec![],
                        viewport_x: 0,
                    }
                };
                for id in ids(&page) {
                    self.hotplug_pinned.remove(id);
                }
                page.columns.retain(|c| !c.windows.is_empty());
                restored.push(page);
            }
            let monitor = &mut self.snapshot.monitors[m];
            // Discard the reconnect placeholder, keeping the original empty tail identity.
            monitor.pages.retain(|page| {
                !empty(page)
                    || self
                        .minimized_slots
                        .iter()
                        .any(|slot| slot.page_id == page.id)
            });
            // Keep pages received from other disconnected outputs after this monitor's own pages.
            restored.append(&mut monitor.pages);
            monitor.pages = restored;
            monitor.active_page = saved.active_page;
        }
    }

    fn append_hotplug_page(&mut self, m: usize, page: Page) {
        let pages = &mut self.snapshot.monitors[m].pages;
        let index = pages
            .len()
            .saturating_sub(usize::from(pages.last().is_some_and(empty)));
        pages.insert(index, page);
    }

    pub(super) fn repair_hotplug_focus(&mut self) {
        if let Some((m, p, _)) = self
            .snapshot
            .focused_window
            .as_ref()
            .and_then(|id| self.location(id).ok())
        {
            self.snapshot.active_monitor = Some(self.snapshot.monitors[m].monitor.id.clone());
            self.snapshot.monitors[m].active_page = self.snapshot.monitors[m].pages[p].id.clone();
            self.page_focus.insert(
                self.snapshot.monitors[m].active_page.clone(),
                self.snapshot.focused_window.clone().unwrap(),
            );
        } else if self
            .snapshot
            .active_monitor
            .as_ref()
            .is_none_or(|id| self.monitor_index(id).is_err())
        {
            self.snapshot.active_monitor = self
                .snapshot
                .monitors
                .iter()
                .find(|m| m.monitor.primary)
                .or(self.snapshot.monitors.first())
                .map(|m| m.monitor.id.clone());
        }
    }

    pub(super) fn floating_origins(&self) -> BTreeMap<WindowId, (Rect, Rect)> {
        self.snapshot
            .windows
            .iter()
            .filter(|w| w.floating)
            .filter_map(|w| {
                let (m, _, _) = self.location(&w.native.id).ok()?;
                Some((
                    w.native.id.clone(),
                    (self.snapshot.monitors[m].viewport, w.native.rect),
                ))
            })
            .collect()
    }

    pub(super) fn rebase_floating(&mut self, origins: BTreeMap<WindowId, (Rect, Rect)>) {
        for (id, (source, mut rect)) in origins {
            let Ok((m, _, _)) = self.location(&id) else {
                continue;
            };
            let target = self.snapshot.monitors[m].viewport;
            if source == target {
                continue;
            }
            let w = self.window_index(&id).unwrap();
            let resizable = self.snapshot.windows[w].native.resizable;
            let translate = |rect: &mut Rect| {
                if resizable {
                    rect.width = rect.width.min(target.width).max(1);
                    rect.height = rect.height.min(target.height).max(1);
                }
                rect.x = coordinate((rect.x as i64 + target.x as i64 - source.x as i64).clamp(
                    target.x as i64,
                    target.x as i64 + target.width.saturating_sub(rect.width) as i64,
                ));
                rect.y = coordinate((rect.y as i64 + target.y as i64 - source.y as i64).clamp(
                    target.y as i64,
                    target.y as i64 + target.height.saturating_sub(rect.height) as i64,
                ));
            };
            translate(&mut rect);
            self.snapshot.windows[w].native.rect = rect;
            if let Some(rect) = self.fullscreen_restore.get_mut(&id) {
                translate(rect);
            }
            // A hidden/paused placement has not applied geometry yet, just like a pending rule move.
            self.pending_rule_floating.insert(id);
        }
    }
}
