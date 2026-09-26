use super::*;

/// Reserve one pixel per window, then apportion the rest with cumulative rounding.
/// u128 keeps even u32::MAX viewports/weights safe; the last boundary is exactly total.
fn distribute(total: u32, weights: &[u32]) -> Vec<u32> {
    if weights.is_empty() {
        return vec![];
    }
    let extra = total as u128 - weights.len() as u128;
    let sum: u128 = weights.iter().map(|&w| w as u128).sum();
    let divisor = if sum == 0 { weights.len() as u128 } else { sum };
    let mut cumulative = 0;
    let mut previous = 0;
    weights
        .iter()
        .map(|&weight| {
            cumulative += if sum == 0 { 1 } else { weight as u128 };
            let boundary = extra * cumulative / divisor;
            let height = (boundary - previous + 1) as u32;
            previous = boundary;
            height
        })
        .collect()
}

impl Engine {
    pub(super) fn sizing_target(&self) -> Result<(WindowId, usize, usize, usize, usize), AppError> {
        let id = self.focused()?;
        let window = &self.snapshot.windows[self.window_index(&id)?];
        if window.floating || window.fullscreen {
            return Err(invalid(
                "Sizing requires a tiled window outside layout fullscreen",
            ));
        }
        let (m, p, column) = self.location(&id)?;
        let (c, row) = column.ok_or_else(|| invalid("Window has no tiled column"))?;
        Ok((id, m, p, c, row))
    }

    pub(super) fn resize_column(&mut self, value: i64, relative: bool) -> Result<(), AppError> {
        let (id, m, p, c, _) = self.sizing_target()?;
        let viewport = self.snapshot.monitors[m].viewport.width;
        let width = &mut self.snapshot.monitors[m].pages[p].columns[c].width;
        let target = if relative {
            *width as i64 + value
        } else {
            value
        };
        *width = target.clamp(1, viewport as i64) as u32;
        self.ensure_visible(&id, false)
    }

    pub(super) fn column_heights(&self, column: &Column, total: u32) -> Vec<u32> {
        let known: Vec<_> = column
            .windows
            .iter()
            .filter_map(|id| self.height_weights.get(id).copied())
            .collect();
        // Newly joined windows get an average share, not a near-zero default weight.
        let default = if known.is_empty() {
            1
        } else {
            (known.iter().map(|&w| w as u64).sum::<u64>() / known.len() as u64).max(1) as u32
        };
        let weights: Vec<_> = column
            .windows
            .iter()
            .map(|id| self.height_weights.get(id).copied().unwrap_or(default))
            .collect();
        distribute(total, &weights)
    }

    pub(super) fn adjust_window_height(&mut self, delta: i32) -> Result<(), AppError> {
        let (_, m, p, c, row) = self.sizing_target()?;
        let monitor = &self.snapshot.monitors[m];
        let column = &monitor.pages[p].columns[c];
        if delta == 0 || column.windows.len() == 1 {
            return Ok(());
        }
        let total = monitor.viewport.height;
        let heights = self.column_heights(column, total);
        let target = (heights[row] as i64 + delta as i64)
            .clamp(1, total as i64 - column.windows.len() as i64 + 1) as u32;
        if target == heights[row] {
            return Ok(());
        }
        let weights: Vec<_> = heights
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != row)
            .map(|(_, &h)| h - 1)
            .collect();
        let mut others = distribute(total - target, &weights).into_iter();
        for (i, id) in column.windows.iter().enumerate() {
            let height = if i == row {
                target
            } else {
                others.next().unwrap()
            };
            self.height_weights.insert(id.clone(), height - 1);
        }
        Ok(())
    }
}
