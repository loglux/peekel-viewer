//! Where a picture goes in the window. Pure arithmetic, so it is tested without a terminal.

use peekel_term::WindowSize;

/// What Windows Terminal reports for a cell, whatever the font (measured in BlueEdit): used when the terminal says nothing.
pub const DEFAULT_CELL: Cell = Cell { width: 10, height: 20 };

/// A character cell's size in the pixels of Sixel graphics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub width: u32,
    pub height: u32,
}

/// The cell the terminal really has, when it is another shape than the one it tells (`CSI 16 t`): Windows Terminal tells 10 x 20
/// whatever the font is, and stretches a Sixel picture over as many real cells as the picture has told ones, so with a font whose
/// cell is not 1 : 2 (Consolas at 150 %: 13 x 28) a picture comes out too tall. The window is measured for the real one
/// (`peekel_term::realcell`); the layout is then worked out in real pixels and the picture is made for the told cell by
/// [`for_told_cells`]. `told` itself when the two are the same shape or the real one is not known; `PEEKEL_REAL_CELL=13x28` gives
/// it by hand and `PEEKEL_OFF=aspect` switches this off.
pub fn real_cell(told: Cell) -> Cell {
    if std::env::var("PEEKEL_OFF").is_ok_and(|v| v.split(',').any(|p| p.trim().eq_ignore_ascii_case("aspect"))) {
        return told;
    }
    let given = std::env::var("PEEKEL_REAL_CELL").ok().and_then(|v| {
        let (width, height) = v.split_once('x')?;
        Some((
            width.trim().parse::<usize>().ok()?,
            height.trim().parse::<usize>().ok()?,
        ))
    });
    let told_size = (told.width as usize, told.height as usize);
    given
        .or_else(peekel_term::realcell::measure)
        .filter(|&real| real.0 > 0 && real.1 > 0 && peekel_term::realcell::differs(told_size, real))
        .map_or(told, |(width, height)| Cell {
            width: width as u32,
            height: height as u32,
        })
}

/// What `--timing` says about the cell: the one the terminal tells, the one the picture was laid out in, and what was found out of
/// the window (so that a picture that comes out stretched can be told from one that was not corrected).
pub fn cell_report(told: Cell, used: Cell) -> String {
    let found = match peekel_term::realcell::facts() {
        Some(facts) => format!("window {facts:?}, solved {:?}", peekel_term::realcell::solve(&facts)),
        None => "no window of Windows Terminal found for this console".to_string(),
    };
    format!(
        "cell: the terminal tells {}x{}, laid out in {}x{} ({found})",
        told.width, told.height, used.width, used.height
    )
}

/// The weights with which each of `out` places takes the `n` places before it: the mean over the stretch of the input that the place
/// covers, the input being cut into `out` equal stretches (a place at the edge takes part of its neighbour by its share).
fn shares(n: usize, out: usize) -> Vec<Vec<(usize, f32)>> {
    let step = n as f64 / out as f64;
    (0..out)
        .map(|o| {
            let (from, to) = (o as f64 * step, (o + 1) as f64 * step);
            let first = from.floor() as usize;
            let last = (to.ceil() as usize).min(n);
            (first..last)
                .map(|i| (i, ((to.min(i as f64 + 1.0) - from.max(i as f64)) / step) as f32))
                .collect()
        })
        .collect()
}

/// The picture (`argb`, `width` pixels a row, opaque) made again for the cells the terminal tells, when the real cell is another
/// shape: each side is changed by the ratio of the told cell to the real one, the terminal stretches the picture back over the real
/// cells, and what is seen has the picture's proportions. None when the two cells are the same, or there is nothing to change.
pub fn for_told_cells(argb: &[u32], width: usize, real: Cell, told: Cell) -> Option<(Vec<u32>, usize)> {
    if real == told || width == 0 || real.width == 0 || real.height == 0 {
        return None;
    }
    let height = argb.len() / width;
    let side = |n: usize, told: u32, real: u32| {
        ((n as u64 * u64::from(told) + u64::from(real) / 2) / u64::from(real)).max(1) as usize
    };
    let (new_width, new_height) = (
        side(width, told.width, real.width),
        side(height, told.height, real.height),
    );
    if height == 0 || (new_width, new_height) == (width, height) {
        return None;
    }
    // across: each row of the picture to `new_width` places, three channels
    let across = shares(width, new_width);
    let mut rows = vec![0f32; new_width * height * 3];
    for (y, row) in argb.chunks_exact(width).enumerate() {
        for (x, places) in across.iter().enumerate() {
            let mut sum = [0f32; 3];
            for &(i, share) in places {
                let pixel = row[i];
                sum[0] += share * ((pixel >> 16) & 0xFF) as f32;
                sum[1] += share * ((pixel >> 8) & 0xFF) as f32;
                sum[2] += share * (pixel & 0xFF) as f32;
            }
            rows[(y * new_width + x) * 3..][..3].copy_from_slice(&sum);
        }
    }
    // down: each column to `new_height` places
    let mut out = vec![0u32; new_width * new_height];
    for (y, places) in shares(height, new_height).iter().enumerate() {
        for x in 0..new_width {
            let mut sum = [0f32; 3];
            for &(j, share) in places {
                for (c, total) in sum.iter_mut().enumerate() {
                    *total += share * rows[(j * new_width + x) * 3 + c];
                }
            }
            let channel = |c: usize| sum[c].round().clamp(0.0, 255.0) as u32;
            out[y * new_width + x] = 0xFF00_0000 | channel(0) << 16 | channel(1) << 8 | channel(2);
        }
    }
    Some((out, new_width))
}

/// The cell's size from the window's size in pixels and in cells, for a terminal that fills the first in (Unix) and
/// has not answered `CSI 16 t`.
pub fn cell_from_window(window: WindowSize) -> Option<Cell> {
    let (width, height) = (u32::from(window.width_px), u32::from(window.height_px));
    let (cols, rows) = (u32::from(window.cols), u32::from(window.rows));
    (cols > 0 && rows > 0 && width >= cols && height >= rows).then(|| Cell {
        width: width / cols,
        height: height / rows,
    })
}

/// The room for a picture in pixels: the whole window but its last row. A picture that reaches the last row would
/// leave the cursor below the screen after it, and the screen would scroll.
pub fn room(window: WindowSize, cell: Cell) -> (usize, usize) {
    let width = u32::from(window.cols) * cell.width;
    let height = u32::from(window.rows.saturating_sub(1)) * cell.height;
    (width as usize, height as usize)
}

/// The cell (row, column, both counted from 1) where a picture of this size starts to be centred in [`room`].
pub fn origin(window: WindowSize, cell: Cell, picture: (usize, usize)) -> (usize, usize) {
    let (room_width, room_height) = room(window, cell);
    let centred =
        |room: usize, picture: usize, cell: u32| 1 + room.saturating_sub(picture) / (cell as usize).max(1) / 2;
    (
        centred(room_height, picture.1, cell.height),
        centred(room_width, picture.0, cell.width),
    )
}

/// What clears the room round a picture of `picture` pixels at `origin` (row, column from 1), and not under it: the rows above and
/// below it whole, the rows it stands on to its left and right. Written before a picture of another size or place, in place of
/// clearing the whole screen, so that the last picture stays where the new one will cover it: a terminal that shows a frame
/// before its end (Windows Terminal holds synchronized output for less than 100 ms, and a big picture takes longer) shows the old
/// picture there, not an empty screen. `docs/internal/quickview-stripe.md`.
pub fn clear_round(window: WindowSize, cell: Cell, origin: (usize, usize), picture: (usize, usize)) -> String {
    // only the cells the picture covers whole are left: the picture does not paint the rest of a cell it covers in part, where the
    // last picture would show
    let rows = picture.1 / (cell.height as usize).max(1);
    let cols = picture.0 / (cell.width as usize).max(1);
    let room_rows = usize::from(window.rows.saturating_sub(1));
    let width = usize::from(window.cols);
    let mut out = String::from("\x1b[0m");
    for row in 1..=room_rows {
        if (origin.0..origin.0 + rows).contains(&row) {
            if origin.1 > 1 {
                out += &format!("\x1b[{row};1H\x1b[{}X", origin.1 - 1);
            }
            if origin.1 + cols <= width {
                out += &format!("\x1b[{row};{}H\x1b[K", origin.1 + cols);
            }
        } else {
            out += &format!("\x1b[{row};1H\x1b[2K");
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot make its fixture should stop at once
mod tests {
    use super::*;

    #[test]
    fn clearing_round_a_picture_leaves_its_cells_alone() {
        let cell = Cell { width: 10, height: 20 };
        // 30 x 2 cells (300 x 40 px) at row 3, column 5, in a window of 100 x 30 (the room is 29 rows)
        let out = clear_round(WINDOW, cell, (3, 5), (300, 40));
        assert!(out.contains("\x1b[1;1H\x1b[2K") && out.contains("\x1b[29;1H\x1b[2K"));
        assert!(!out.contains("\x1b[30;"), "the status line is not touched");
        // the rows of the picture: four cells to its left, from column 35 to the end of the row to its right
        for row in [3, 4] {
            assert!(out.contains(&format!("\x1b[{row};1H\x1b[4X")));
            assert!(out.contains(&format!("\x1b[{row};35H\x1b[K")));
            assert!(!out.contains(&format!("\x1b[{row};1H\x1b[2K")));
        }
        // a picture as wide as the window has nothing on its sides
        let wide = clear_round(WINDOW, cell, (3, 1), (1000, 40));
        assert!(!wide.contains("\x1b[3;1H") && !wide.contains("\x1b[3;101H"));
        // a cell covered in part is cleared: 305 x 45 px covers 30 x 2 cells whole, the 31st column and the 3rd row in part
        let part = clear_round(WINDOW, cell, (3, 5), (305, 45));
        assert!(part.contains("\x1b[3;35H\x1b[K") && part.contains("\x1b[5;1H\x1b[2K"));
    }

    const WINDOW: WindowSize = WindowSize {
        cols: 100,
        rows: 30,
        width_px: 0,
        height_px: 0,
    };

    #[test]
    fn the_cell_is_worked_out_from_the_windows_pixels_when_there_are_any() {
        let sized = WindowSize {
            width_px: 1000,
            height_px: 600,
            ..WINDOW
        };
        assert_eq!(cell_from_window(sized), Some(Cell { width: 10, height: 20 }));
        assert_eq!(cell_from_window(WINDOW), None, "no pixels given");
        assert_eq!(cell_from_window(WindowSize { cols: 0, ..sized }), None);
    }

    #[test]
    fn the_room_is_the_window_but_its_last_row() {
        assert_eq!(room(WINDOW, DEFAULT_CELL), (1000, 580));
    }

    #[test]
    fn a_window_of_one_row_has_no_room() {
        let tiny = WindowSize { rows: 1, ..WINDOW };
        assert_eq!(room(tiny, DEFAULT_CELL), (1000, 0));
    }

    #[test]
    fn a_picture_that_fills_the_room_starts_at_the_corner() {
        assert_eq!(origin(WINDOW, DEFAULT_CELL, (1000, 580)), (1, 1));
    }

    #[test]
    fn a_smaller_picture_is_centred_by_whole_cells() {
        // 500 x 200 in 1000 x 580: 250 px (25 cells) and 190 px (9 whole cells) of margin on each side
        assert_eq!(
            origin(WINDOW, DEFAULT_CELL, (500, 200)),
            (1 + 380 / 20 / 2, 1 + 500 / 10 / 2)
        );
        assert_eq!(origin(WINDOW, DEFAULT_CELL, (500, 200)), (10, 26));
    }

    #[test]
    fn a_picture_larger_than_the_room_does_not_underflow() {
        assert_eq!(origin(WINDOW, DEFAULT_CELL, (5000, 5000)), (1, 1));
    }

    #[test]
    fn a_cell_of_zero_does_not_divide_by_zero() {
        assert_eq!(origin(WINDOW, Cell { width: 0, height: 0 }, (10, 10)), (1, 1));
    }

    const TOLD: Cell = Cell { width: 10, height: 20 };
    const REAL: Cell = Cell { width: 13, height: 28 };

    #[test]
    fn a_picture_is_made_for_the_told_cell_by_the_ratio_of_the_two_cells() {
        // 130 x 280 real pixels are 10 x 10 real cells: for the told cell they are 100 x 200
        let argb = vec![0xFF33_6699u32; 130 * 280];
        let (made, width) = for_told_cells(&argb, 130, REAL, TOLD).expect("changed");
        assert_eq!((width, made.len() / width), (100, 200));
        assert!(made.iter().all(|&p| p == 0xFF33_6699));
    }

    #[test]
    fn what_is_seen_has_the_proportions_of_the_picture_once_the_terminal_stretches_it() {
        // the terminal puts `width / 10` x `height / 20` cells of 13 x 28 pixels where the picture is: 1.3 and 1.4 times each side
        let (width, height) = (800usize, 400usize);
        let (made, new_width) = for_told_cells(&vec![0xFFFF_FFFF; width * height], width, REAL, TOLD).expect("changed");
        let new_height = made.len() / new_width;
        let seen = (new_width as f64 / 10.0 * 13.0) / (new_height as f64 / 20.0 * 28.0);
        assert!((seen - width as f64 / height as f64).abs() < 0.02, "{seen}");
    }

    #[test]
    fn an_edge_between_two_colours_stays_where_it_is_in_proportion() {
        // the left half black, the right half white, 130 wide: after the change the edge is at the middle of 100
        let argb: Vec<u32> = (0..130 * 28)
            .map(|i| if i % 130 < 65 { 0xFF00_0000 } else { 0xFFFF_FFFF })
            .collect();
        let (made, width) = for_told_cells(&argb, 130, REAL, TOLD).expect("changed");
        assert_eq!(width, 100);
        assert_eq!(made[49] & 0xFF, 0);
        assert_eq!(made[50] & 0xFF, 255);
    }

    #[test]
    fn nothing_is_made_again_when_the_cells_are_the_same() {
        assert!(for_told_cells(&[0xFF00_0000; 100], 10, TOLD, TOLD).is_none());
        assert!(for_told_cells(&[], 0, REAL, TOLD).is_none());
    }
}
