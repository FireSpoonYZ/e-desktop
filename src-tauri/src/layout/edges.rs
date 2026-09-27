//! "Always fill" strip geometry and column-boundary drags. Pure functions shared by the
//! engine and the Windows drag preview, so the preview is exactly what a drop applies.
//! Positions are offsets inside a viewport of width `view`; `x` is the page scroll.

/// Widen the columns proportionally when together they are narrower than the viewport.
pub fn fill(widths: &mut [u32], view: u32) {
    let extent: u64 = widths.iter().map(|&w| u64::from(w)).sum();
    if widths.is_empty() || extent >= u64::from(view) {
        return;
    }
    // Cumulative rounding: the last boundary lands exactly on `view`.
    let (mut cumulative, mut previous) = (0u64, 0u64);
    for w in widths.iter_mut() {
        cumulative += u64::from(*w);
        let boundary = u64::from(view) * cumulative / extent.max(1);
        *w = ((boundary - previous) as u32).max(1);
        previous = boundary;
    }
}

/// No empty space at either end: 0 ..= extent - view.
pub fn clamp_x(widths: &[u32], view: u32, x: i64) -> i64 {
    let extent: i64 = widths.iter().map(|&w| i64::from(w)).sum();
    x.clamp(0, (extent - i64::from(view)).max(0))
}

/// Scroll positions that align a column edge with the left or right screen edge.
fn stops(widths: &[u32], view: u32) -> Vec<i64> {
    let mut edge = 0i64;
    let mut stops = vec![0];
    for &w in widths {
        edge += i64::from(w);
        stops.extend([edge, edge - i64::from(view)]);
    }
    let mut stops: Vec<_> = stops
        .into_iter()
        .map(|s| clamp_x(widths, view, s))
        .collect();
    stops.sort_unstable();
    stops.dedup();
    stops
}

/// Scroll by `delta`, then settle on the nearest aligned position; a nonzero scroll that
/// would settle back where it started moves one stop further in its direction.
pub fn snap_scroll(widths: &[u32], view: u32, x: i64, delta: i64) -> i64 {
    let stops = stops(widths, view);
    let target = x + delta;
    let nearest = *stops
        .iter()
        .min_by_key(|s| (*s - target).abs())
        .unwrap_or(&0);
    if nearest != x || delta == 0 {
        return nearest;
    }
    let next = if delta > 0 {
        stops.iter().find(|s| **s > x)
    } else {
        stops.iter().rev().find(|s| **s < x)
    };
    next.copied().unwrap_or(x)
}

/// Offset of boundary `edge` (0 = left of the first column) inside the viewport.
pub fn edge_position(widths: &[u32], x: i64, edge: usize) -> i64 {
    widths[..edge].iter().map(|&w| i64::from(w)).sum::<i64>() - x
}

/// Where a dragged boundary lands inside a span of `size`: on either end within `snap`, on
/// the middle within a third of it, otherwise where the pointer put it.
pub fn snapped(target: i64, size: i64, snap: i64) -> i64 {
    if target.abs() <= snap {
        0
    } else if (target - size).abs() <= snap {
        size
    } else if (target - size / 2).abs() <= snap / 3 {
        size / 2
    } else {
        target
    }
}

/// Drag boundary `edge` by `delta`. Every other visible boundary keeps its screen position:
/// a fully visible neighbour gives or takes the width; a neighbour cut by the screen edge
/// slides instead (so a column hidden behind that edge peeks in or out). The dragged edge
/// snaps onto the screen edges and the middle (see `snapped`). A boundary between two columns
/// that started clear of a screen edge's snap zone and is dropped onto that edge squeezes:
/// the column on the far side takes the whole screen and everything it passed is pushed off
/// screen, keeping its width. Returns the filled widths and clamped scroll.
pub fn drag_edge(
    widths: &[u32],
    view: u32,
    x: i64,
    edge: usize,
    delta: i64,
    snap: i64,
) -> (Vec<u32>, i64) {
    let mut widths = widths.to_vec();
    let n = widths.len();
    if n == 0 || edge > n {
        return (widths, x);
    }
    let view_i = i64::from(view);
    let min = (view_i / 10).max(1);
    let p = edge_position(&widths, x, edge);
    let target = snapped(p + delta, view_i, snap);
    if edge > 0 && edge < n && ((target == 0 && p > snap) || (target == view_i && p < view_i - snap))
    {
        let keep = if target == 0 { edge } else { edge - 1 };
        widths[keep] = view;
        let x = edge_position(&widths, 0, keep);
        return (widths.clone(), clamp_x(&widths, view, x));
    }
    let left_full = edge > 0 && edge_position(&widths, x, edge - 1) >= 0;
    let right_full = edge < n && edge_position(&widths, x, edge + 1) <= view_i;
    // Which neighbour changes width: Some(true) left grows, Some(false) right shrinks + scroll.
    let (left, right) = match (left_full, right_full) {
        (true, true) => (true, true),
        (true, false) => (true, false),
        (false, true) => (false, true),
        (false, false) if edge > 0 => (true, false),
        (false, false) => (false, true),
    };
    let (mut lo, mut hi) = (i64::MIN, i64::MAX);
    if left {
        let w = i64::from(widths[edge - 1]);
        lo = lo.max(min - w);
        hi = hi.min(view_i - w);
    }
    if right {
        let w = i64::from(widths[edge]);
        lo = lo.max(w - view_i);
        hi = hi.min(w - min);
    }
    if lo > hi {
        let x = clamp_x(&widths, view, x);
        return (widths, x);
    }
    let d = (target - p).clamp(lo, hi);
    let mut x = x;
    if left {
        widths[edge - 1] = (i64::from(widths[edge - 1]) + d) as u32;
    }
    if right {
        widths[edge] = (i64::from(widths[edge]) - d) as u32;
        if !left {
            x -= d; // The left side slides with the dragged edge.
        }
    }
    fill(&mut widths, view);
    let x = clamp_x(&widths, view, x);
    (widths, x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_widens_proportionally_and_exactly() {
        let mut w = vec![500];
        fill(&mut w, 1000);
        assert_eq!(w, [1000]);
        let mut w = vec![300, 100];
        fill(&mut w, 1000);
        assert_eq!(w, [750, 250]);
        let mut w = vec![1, 1, 1];
        fill(&mut w, 1000);
        assert_eq!(w.iter().sum::<u32>(), 1000);
        let mut w = vec![600, 600];
        fill(&mut w, 1000);
        assert_eq!(w, [600, 600]);
        assert_eq!(clamp_x(&w, 1000, -50), 0);
        assert_eq!(clamp_x(&w, 1000, 900), 200);
    }

    #[test]
    fn scroll_snaps_to_aligned_edges() {
        let w = [600, 600, 600];
        // Stops: 0, 200 (col 1 right-aligned... col 0|1 edge at 600-1000<0), 600, 800.
        assert_eq!(snap_scroll(&w, 1000, 0, 333), 200);
        assert_eq!(snap_scroll(&w, 1000, 200, 20), 600);
        assert_eq!(snap_scroll(&w, 1000, 600, -20), 200);
        assert_eq!(snap_scroll(&w, 1000, 800, 500), 800);
        assert_eq!(snap_scroll(&w, 1000, 0, 0), 0);
    }

    #[test]
    fn shared_boundary_transfers_width_between_snapped_neighbours() {
        // Two columns exactly filling the screen.
        let (w, x) = drag_edge(&[500, 500], 1000, 0, 1, 120, 0);
        assert_eq!((w, x), (vec![620, 380], 0));
    }

    #[test]
    fn snapped_screen_edge_unsnaps_and_reveals_the_hidden_column() {
        // Columns 0..3; the screen shows columns 1 and 2 exactly (x = 400).
        let widths = [400, 500, 500];
        let (w, x) = drag_edge(&widths, 1000, 400, 1, 150, 0);
        // Column 1 narrows with its right edge fixed; column 0 peeks 150px in.
        assert_eq!(w, vec![400, 350, 500]);
        assert_eq!(x, 250);
        assert_eq!(edge_position(&w, x, 1), 150);
        assert_eq!(edge_position(&w, x, 3), 1000);
        // Right screen edge: column 1 narrows, column 2 slides in from the right.
        let (w, x) = drag_edge(&[500, 500, 400], 1000, 0, 2, -200, 0);
        assert_eq!((w.clone(), x), (vec![500, 300, 400], 0));
        assert_eq!(edge_position(&w, x, 2), 800);
    }

    #[test]
    fn unsnapped_edge_snaps_back_onto_the_screen_edge() {
        // The boundary started on the left screen edge (column 0 hidden): dragging it in and
        // back out onto that edge leaves the layout as it was.
        let widths = [400, 500, 500];
        let (w, x) = drag_edge(&widths, 1000, 400, 1, 20, 32);
        assert_eq!((w, x), (vec![400, 500, 500], 400));
    }

    #[test]
    fn boundary_dropped_on_a_screen_edge_squeezes_the_neighbour_out() {
        // [A | B] on screen with C hidden on the right. B's left edge onto the left screen
        // edge: B takes the screen, A is pushed out left, C stays out right; widths kept.
        let (w, x) = drag_edge(&[500, 500, 500], 1000, 0, 1, -480, 32);
        assert_eq!((w.clone(), x), (vec![500, 1000, 500], 500));
        assert_eq!(edge_position(&w, x, 1), 0);
        // A's right edge onto the right screen edge: A takes the screen, B is pushed right.
        let (w, x) = drag_edge(&[500, 500, 500], 1000, 0, 1, 490, 32);
        assert_eq!((w, x), (vec![1000, 500, 500], 0));
        // Three visible columns: B|C onto the left edge pushes both A and B out.
        let (w, x) = drag_edge(&[300, 300, 400], 1000, 0, 2, -590, 32);
        assert_eq!((w, x), (vec![300, 300, 1000], 600));
        // A boundary grabbed inside the edge's snap zone (A peeks 50 px) only re-hides A.
        let (w, x) = drag_edge(&[950, 500, 500], 1000, 900, 1, 10, 100);
        assert_eq!((w, x), (vec![950, 550, 500], 950));
    }

    #[test]
    fn boundary_snaps_to_the_middle() {
        let (w, x) = drag_edge(&[700, 300], 1000, 0, 1, -190, 60);
        assert_eq!((w, x), (vec![500, 500], 0));
        // Outside a third of the snap distance the pointer wins.
        let (w, _) = drag_edge(&[700, 300], 1000, 0, 1, -170, 60);
        assert_eq!(w, vec![530, 470]);
    }

    #[test]
    fn widths_stay_above_the_minimum_and_the_screen_stays_filled() {
        let (w, x) = drag_edge(&[500, 500], 1000, 0, 1, 900, 0);
        assert_eq!((w, x), (vec![900, 100], 0));
        // Shrinking the only column refills it.
        let (w, x) = drag_edge(&[1000], 1000, 0, 1, -300, 0);
        assert_eq!((w, x), (vec![1000], 0));
    }
}
