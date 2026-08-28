use std::io::{self, stdout};

use clap::Parser;
use ratatui::{
    Frame,
    buffer::Buffer,
    crossterm::{
        event::{
            self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind,
            KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
        },
        execute,
    },
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    symbols::pixel::{QUADRANTS, SEXTANTS},
    widgets::{Block, Borders, Paragraph, Widget},
};

const GRID_WIDTH: u16 = 7;
const GRID_HEIGHT: u16 = 9;
const APP_WIDTH: u16 = 39;
const APP_HEIGHT: u16 = 13;

#[derive(Debug, Parser)]
#[command(version, about = "Build Unicode terminal block characters")]
struct Cli {
    /// Scale the selected vertical bounds to each output character's height
    #[arg(long)]
    scaling: bool,
}

fn main() -> io::Result<()> {
    let cli = Cli::parse();
    ratatui::run(|terminal| {
        let _mouse_capture = MouseCapture::enable()?;
        App::new(cli.scaling).run(terminal)
    })
}

#[derive(Default)]
struct App {
    cells: [[bool; 2]; 4],
    scaling: bool,
    last_cell_action: Option<CellAction>,
    grid_area: Option<Rect>,
    select_all_area: Option<Rect>,
    should_quit: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CellAction {
    Fill,
    Clear,
}

impl App {
    fn new(scaling: bool) -> Self {
        Self {
            scaling,
            ..Self::default()
        }
    }

    fn run(&mut self, terminal: &mut ratatui::DefaultTerminal) -> io::Result<()> {
        while !self.should_quit {
            terminal.draw(|frame| self.render(frame))?;
            self.handle_event(event::read()?);
        }
        Ok(())
    }

    fn render(&mut self, frame: &mut Frame) {
        let Some(layout) = AppLayout::new(frame.area()) else {
            self.grid_area = None;
            self.select_all_area = None;
            frame.render_widget(
                Paragraph::new(format!(
                    "Terminal too small (need {APP_WIDTH} x {APP_HEIGHT})"
                ))
                .alignment(Alignment::Center),
                frame.area(),
            );
            return;
        };
        self.grid_area = Some(layout.grid);
        self.select_all_area = Some(layout.select_all);

        frame.render_widget(
            Paragraph::new("Octant builder")
                .alignment(Alignment::Center)
                .style(Style::default().add_modifier(Modifier::BOLD)),
            layout.title,
        );
        frame.render_widget(CellGrid::new(&self.cells), layout.grid);
        frame.render_widget(
            Paragraph::new("All").style(Style::default().fg(Color::DarkGray)),
            layout.select_all_label,
        );
        frame.render_widget(
            Paragraph::new(self.select_all_text()).style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            layout.select_all,
        );

        let (output_title, output_text) = if self.scaling {
            (" Scaled outputs ", scaled_outputs_text(&self.cells))
        } else {
            (" Exact outputs ", exact_outputs_text(self.mask()))
        };
        let outputs = Paragraph::new(output_text)
            .style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .block(Block::new().title(output_title).borders(Borders::ALL));
        frame.render_widget(outputs, layout.outputs);

        frame.render_widget(
            Paragraph::new("Click cells  •  q/Esc quits")
                .alignment(Alignment::Center)
                .style(Style::default().fg(Color::DarkGray)),
            layout.help,
        );
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                let ctrl_c =
                    key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
                if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) || ctrl_c {
                    self.should_quit = true;
                }
            }
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            _ => {}
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return;
        }

        if self
            .select_all_area
            .is_some_and(|area| rect_contains(area, mouse.column, mouse.row))
        {
            self.toggle_all();
            return;
        }

        let Some(grid) = self.grid_area else {
            return;
        };
        if let Some((row, column)) = grid_cell_at(grid, mouse.column, mouse.row) {
            self.toggle_cell(row, column);
        }
    }

    fn mask(&self) -> u8 {
        cells_mask(&self.cells)
    }

    fn select_all_text(&self) -> &'static str {
        let mut cells = self.cells.iter().flatten();
        let Some(first) = cells.next() else {
            return "[   ]";
        };
        if cells.all(|cell| cell == first) {
            if *first { "[ x ]" } else { "[   ]" }
        } else {
            "[ - ]"
        }
    }

    fn toggle_all(&mut self) {
        let all_filled = self.cells.iter().flatten().all(|cell| *cell);
        let any_filled = self.cells.iter().flatten().any(|cell| *cell);
        let fill = match (all_filled, any_filled, self.last_cell_action) {
            (true, _, _) => false,
            (false, false, _) => true,
            (false, true, Some(CellAction::Clear)) => false,
            (false, true, _) => true,
        };
        self.cells
            .iter_mut()
            .flatten()
            .for_each(|cell| *cell = fill);
    }

    fn toggle_cell(&mut self, row: usize, column: usize) {
        let cell = &mut self.cells[row][column];
        *cell = !*cell;
        self.last_cell_action = Some(if *cell {
            CellAction::Fill
        } else {
            CellAction::Clear
        });
    }
}

fn cells_mask(cells: &[[bool; 2]; 4]) -> u8 {
    cells
        .iter()
        .flatten()
        .enumerate()
        .fold(0, |mask, (index, filled)| {
            mask | (u8::from(*filled) << index)
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CharacterOutput {
    name: &'static str,
    glyph: char,
}

fn exact_outputs(mask: u8) -> Vec<CharacterOutput> {
    let mut outputs = vec![CharacterOutput {
        name: "Braille",
        glyph: braille_glyph(mask),
    }];

    if let Some(mask) = top_aligned_mask(mask, 2) {
        outputs.push(CharacterOutput {
            name: "Quadrant",
            glyph: QUADRANTS[usize::from(mask)],
        });
    }
    if let Some(mask) = top_aligned_mask(mask, 3) {
        outputs.push(CharacterOutput {
            name: "Sextant",
            glyph: SEXTANTS[usize::from(mask)],
        });
    }
    outputs.push(CharacterOutput {
        name: "Octant",
        glyph: octant_glyph(mask),
    });
    outputs
}

fn exact_outputs_text(mask: u8) -> String {
    exact_outputs(mask)
        .into_iter()
        .map(|output| {
            format!(
                "{:<8} {} U+{:04X}",
                output.name, output.glyph, output.glyph as u32
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn scaled_outputs_text(cells: &[[bool; 2]; 4]) -> String {
    let octant = octant_glyph(cells_mask(cells));
    let sextant = SEXTANTS[usize::from(vertically_scaled_mask(cells, 3))];
    let quadrant_mask = vertically_scaled_mask(cells, 2);
    let quadrant = QUADRANTS[usize::from(quadrant_mask)];
    let halfblock = halfblock_output(quadrant_mask);

    format!(
        "Octant    {octant} U+{:04X}\n\n\
         Sextant   {sextant} U+{:04X}\n\n\
         Quadrant  {quadrant} U+{:04X}\n\n\
         Halfblock {halfblock}",
        octant as u32, sextant as u32, quadrant as u32,
    )
}

fn top_aligned_mask(mask: u8, height: usize) -> Option<u8> {
    debug_assert!((1..=3).contains(&height));
    (mask >> (height * 2) == 0).then_some(mask)
}

fn braille_glyph(mask: u8) -> char {
    // Convert row-major 2x4 bits to Unicode's dot order: left dots 1, 2, 3, 7
    // and right dots 4, 5, 6, 8.
    const DOT_BITS: [u8; 8] = [0, 3, 1, 4, 2, 5, 6, 7];
    let dots = DOT_BITS
        .into_iter()
        .enumerate()
        .fold(0, |dots, (cell_bit, dot_bit)| {
            dots | ((mask >> cell_bit & 1) << dot_bit)
        });
    char::from_u32(0x2800 + u32::from(dots)).expect("braille code point is valid")
}

struct MouseCapture;

impl MouseCapture {
    fn enable() -> io::Result<Self> {
        execute!(stdout(), EnableMouseCapture)?;
        Ok(Self)
    }
}

impl Drop for MouseCapture {
    fn drop(&mut self) {
        let _ = execute!(stdout(), DisableMouseCapture);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AppLayout {
    title: Rect,
    grid: Rect,
    select_all_label: Rect,
    select_all: Rect,
    outputs: Rect,
    help: Rect,
}

impl AppLayout {
    fn new(area: Rect) -> Option<Self> {
        if area.width < APP_WIDTH || area.height < APP_HEIGHT {
            return None;
        }

        let x = area.x + (area.width - APP_WIDTH) / 2;
        let y = area.y + (area.height - APP_HEIGHT) / 2;
        Some(Self {
            title: Rect::new(x, y, APP_WIDTH, 1),
            grid: Rect::new(x + 2, y + 2, GRID_WIDTH, GRID_HEIGHT),
            select_all_label: Rect::new(x + 1, y + 11, 3, 1),
            select_all: Rect::new(x + 5, y + 11, 5, 1),
            outputs: Rect::new(x + 12, y + 2, 25, 9),
            help: Rect::new(x, y + 12, APP_WIDTH, 1),
        })
    }
}

fn rect_contains(area: Rect, x: u16, y: u16) -> bool {
    x >= area.x && x < area.right() && y >= area.y && y < area.bottom()
}

struct CellGrid<'a> {
    cells: &'a [[bool; 2]; 4],
}

impl<'a> CellGrid<'a> {
    fn new(cells: &'a [[bool; 2]; 4]) -> Self {
        Self { cells }
    }
}

impl Widget for CellGrid<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.width < GRID_WIDTH || area.height < GRID_HEIGHT {
            return;
        }

        const BORDER_ROWS: [&str; 5] = ["┌──┬──┐", "├──┼──┤", "├──┼──┤", "├──┼──┤", "└──┴──┘"];
        let border_style = Style::default().fg(Color::DarkGray);
        let filled_style = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD);

        for (row, border) in BORDER_ROWS.iter().enumerate() {
            buffer.set_string(area.x, area.y + row as u16 * 2, *border, border_style);
        }
        for (row, cells) in self.cells.iter().enumerate() {
            let y = area.y + 1 + row as u16 * 2;
            buffer.set_string(area.x, y, "│  │  │", border_style);
            for (column, filled) in cells.iter().enumerate() {
                if *filled {
                    buffer.set_string(area.x + 1 + column as u16 * 3, y, "██", filled_style);
                }
            }
        }
    }
}

fn grid_cell_at(grid: Rect, x: u16, y: u16) -> Option<(usize, usize)> {
    if x <= grid.x || y <= grid.y || x >= grid.right() || y >= grid.bottom() {
        return None;
    }

    let local_x = x - grid.x;
    let local_y = y - grid.y;
    if local_y.is_multiple_of(2) {
        return None;
    }

    let column = match local_x {
        1..=2 => 0,
        4..=5 => 1,
        _ => return None,
    };
    Some(((local_y / 2) as usize, column))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CellBounds {
    min_column: usize,
    max_column: usize,
    min_row: usize,
    max_row: usize,
}

fn selected_bounds(cells: &[[bool; 2]; 4]) -> Option<CellBounds> {
    let mut bounds: Option<CellBounds> = None;
    for (row, columns) in cells.iter().enumerate() {
        for (column, selected) in columns.iter().enumerate() {
            if !selected {
                continue;
            }
            bounds = Some(match bounds {
                Some(bounds) => CellBounds {
                    min_column: bounds.min_column.min(column),
                    max_column: bounds.max_column.max(column),
                    min_row: bounds.min_row.min(row),
                    max_row: bounds.max_row.max(row),
                },
                None => CellBounds {
                    min_column: column,
                    max_column: column,
                    min_row: row,
                    max_row: row,
                },
            });
        }
    }
    bounds
}

/// Resamples the selected cells' vertical AABB range while retaining their
/// original left/right column positions.
fn vertically_scaled_mask(cells: &[[bool; 2]; 4], target_height: usize) -> u8 {
    debug_assert!((1..=4).contains(&target_height));
    let Some(bounds) = selected_bounds(cells) else {
        return 0;
    };

    let source_height = bounds.max_row - bounds.min_row + 1;
    let mut mask = 0;
    for target_row in 0..target_height {
        // Endpoint-aligned nearest-neighbor scaling keeps both AABB extremes.
        let source_offset = if target_height == 1 {
            (source_height - 1) / 2
        } else {
            (target_row * (source_height - 1) + (target_height - 1) / 2) / (target_height - 1)
        };
        let source_row = bounds.min_row + source_offset;
        for (column, selected) in cells[source_row]
            .iter()
            .enumerate()
            .take(bounds.max_column + 1)
            .skip(bounds.min_column)
        {
            if *selected {
                mask |= 1 << (target_row * 2 + column);
            }
        }
    }
    mask
}

fn halfblock_output(mask: u8) -> &'static str {
    // Exact 2x2 renderings using horizontal/vertical halves when one glyph is
    // sufficient, or two vertical half-block cells for the remaining masks.
    const HALF_BLOCKS: [&str; 16] = [
        "\u{00A0}",
        "▀\u{00A0}",
        "\u{00A0}▀",
        "▀",
        "▄\u{00A0}",
        "▌",
        "▄▀",
        "█▀",
        "\u{00A0}▄",
        "▀▄",
        "▐",
        "▀█",
        "▄",
        "█▄",
        "▄█",
        "█",
    ];
    HALF_BLOCKS[usize::from(mask)]
}

/// Converts the row-major octant bits (top-left is bit 0) to the Unicode glyph.
fn octant_glyph(mask: u8) -> char {
    // Unicode assigns 230 octant patterns contiguously at U+1CD00. The other
    // 26 patterns reuse earlier block-element characters.
    const REUSED: [(u8, char); 26] = [
        (0x00, '\u{00A0}'),
        (0x01, '\u{1CEA8}'),
        (0x02, '\u{1CEAB}'),
        (0x03, '\u{1FB82}'),
        (0x05, '\u{2598}'),
        (0x0A, '\u{259D}'),
        (0x0F, '\u{2580}'),
        (0x14, '\u{1FBE6}'),
        (0x28, '\u{1FBE7}'),
        (0x3F, '\u{1FB85}'),
        (0x40, '\u{1CEA3}'),
        (0x50, '\u{2596}'),
        (0x55, '\u{258C}'),
        (0x5A, '\u{259E}'),
        (0x5F, '\u{259B}'),
        (0x80, '\u{1CEA0}'),
        (0xA0, '\u{2597}'),
        (0xA5, '\u{259A}'),
        (0xAA, '\u{2590}'),
        (0xAF, '\u{259C}'),
        (0xC0, '\u{2582}'),
        (0xF0, '\u{2584}'),
        (0xF5, '\u{2599}'),
        (0xFA, '\u{259F}'),
        (0xFC, '\u{2586}'),
        (0xFF, '\u{2588}'),
    ];

    match REUSED.binary_search_by_key(&mask, |(pattern, _)| *pattern) {
        Ok(index) => REUSED[index].1,
        Err(skipped) => char::from_u32(0x1CD00 + u32::from(mask) - skipped as u32)
            .expect("octant code point is valid"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn scaling_is_opt_in() {
        assert!(!Cli::try_parse_from(["tblocks-rs"]).unwrap().scaling);
        assert!(
            Cli::try_parse_from(["tblocks-rs", "--scaling"])
                .unwrap()
                .scaling
        );
    }

    #[test]
    fn mask_uses_row_major_octant_numbering() {
        let mut app = App::default();
        app.cells[0][0] = true;
        app.cells[1][1] = true;
        app.cells[3][1] = true;

        assert_eq!(app.mask(), 0b1000_1001);
    }

    #[test]
    fn braille_mapping_uses_unicode_dot_order() {
        let examples = [
            (0x01, '\u{2801}'),
            (0x03, '\u{2809}'),
            (0x07, '\u{280B}'),
            (0x0F, '\u{281B}'),
            (0x1F, '\u{281F}'),
            (0x3F, '\u{283F}'),
            (0x7F, '\u{287F}'),
            (0xFF, '\u{28FF}'),
            (0xDF, '\u{28DF}'),
            (0x66, '\u{286A}'),
            (0x90, '\u{2884}'),
        ];

        for (mask, expected) in examples {
            assert_eq!(braille_glyph(mask), expected, "mask {mask:#04x}");
        }
    }

    #[test]
    fn exact_outputs_only_use_top_aligned_compatible_grids() {
        assert_eq!(
            exact_outputs(0x01),
            vec![
                CharacterOutput {
                    name: "Braille",
                    glyph: '\u{2801}',
                },
                CharacterOutput {
                    name: "Quadrant",
                    glyph: '\u{2598}',
                },
                CharacterOutput {
                    name: "Sextant",
                    glyph: '\u{1FB00}',
                },
                CharacterOutput {
                    name: "Octant",
                    glyph: '\u{1CEA8}',
                },
            ]
        );

        assert_eq!(
            exact_outputs(0x1F)
                .into_iter()
                .map(|output| output.name)
                .collect::<Vec<_>>(),
            ["Braille", "Sextant", "Octant"]
        );
        assert_eq!(
            exact_outputs(0x90)
                .into_iter()
                .map(|output| output.name)
                .collect::<Vec<_>>(),
            ["Braille", "Octant"]
        );
    }

    #[test]
    fn exact_outputs_match_representative_patterns() {
        let examples = [
            (0x01, "⠁▘🬀𜺨"),
            (0x03, "⠉▀🬂🮂"),
            (0x07, "⠋▛🬆𜴂"),
            (0x0F, "⠛█🬎▀"),
            (0x1F, "⠟🬝𜴗"),
            (0x3F, "⠿█🮅"),
            (0x7F, "⡿𜵰"),
            (0xFF, "⣿█"),
            (0xDF, "⣟𜷊"),
            (0x66, "⡪𜵗"),
            (0x90, "⢄𜶀"),
            (0x20, "⠠🬞𜴘"),
            (0x08, "⠐▗🬇𜴃"),
        ];

        for (mask, expected) in examples {
            let actual = exact_outputs(mask)
                .into_iter()
                .map(|output| output.glyph)
                .collect::<String>();
            assert_eq!(actual, expected, "mask {mask:#04x}");
        }
    }

    #[test]
    fn hit_testing_accepts_only_cell_interiors() {
        let grid = Rect::new(10, 20, GRID_WIDTH, GRID_HEIGHT);

        assert_eq!(grid_cell_at(grid, 11, 21), Some((0, 0)));
        assert_eq!(grid_cell_at(grid, 12, 21), Some((0, 0)));
        assert_eq!(grid_cell_at(grid, 14, 27), Some((3, 1)));
        assert_eq!(grid_cell_at(grid, 15, 27), Some((3, 1)));
        assert_eq!(grid_cell_at(grid, 13, 21), None);
        assert_eq!(grid_cell_at(grid, 11, 22), None);
        assert_eq!(grid_cell_at(grid, 10, 20), None);
    }

    #[test]
    fn filled_cells_render_as_two_full_blocks() {
        let mut cells = [[false; 2]; 4];
        cells[0][1] = true;
        let area = Rect::new(0, 0, GRID_WIDTH, GRID_HEIGHT);
        let mut buffer = Buffer::empty(area);

        CellGrid::new(&cells).render(area, &mut buffer);

        assert_eq!(buffer[(3, 1)].symbol(), "│");
        assert_eq!(buffer[(4, 1)].symbol(), "█");
        assert_eq!(buffer[(5, 1)].symbol(), "█");
        assert_eq!(buffer[(6, 1)].symbol(), "│");
    }

    #[test]
    fn select_all_control_cycles_empty_and_full() {
        let mut app = App::default();
        assert_eq!(app.select_all_text(), "[   ]");

        app.toggle_all();
        assert!(app.cells.iter().flatten().all(|cell| *cell));
        assert_eq!(app.select_all_text(), "[ x ]");

        app.toggle_all();
        assert!(app.cells.iter().flatten().all(|cell| !cell));
        assert_eq!(app.select_all_text(), "[   ]");
    }

    #[test]
    fn partial_select_all_control_repeats_last_fill() {
        let mut app = App::default();
        app.toggle_cell(2, 1);
        assert_eq!(app.select_all_text(), "[ - ]");
        assert_eq!(app.last_cell_action, Some(CellAction::Fill));

        app.toggle_all();
        assert!(app.cells.iter().flatten().all(|cell| *cell));
        assert_eq!(app.select_all_text(), "[ x ]");
    }

    #[test]
    fn partial_select_all_control_repeats_last_clear() {
        let mut app = App::default();
        app.toggle_all();
        app.toggle_cell(2, 1);
        assert_eq!(app.select_all_text(), "[ - ]");
        assert_eq!(app.last_cell_action, Some(CellAction::Clear));

        app.toggle_all();
        assert!(app.cells.iter().flatten().all(|cell| !cell));
        assert_eq!(app.select_all_text(), "[   ]");
    }

    #[test]
    fn octant_mapping_has_all_256_distinct_patterns() {
        let glyphs = (u8::MIN..=u8::MAX)
            .map(octant_glyph)
            .collect::<HashSet<_>>();

        assert_eq!(glyphs.len(), 256);
        assert_eq!(octant_glyph(0x00), '\u{00A0}');
        assert_eq!(octant_glyph(0x04), '\u{1CD00}');
        assert_eq!(octant_glyph(0x06), '\u{1CD01}');
        assert_eq!(octant_glyph(0xFE), '\u{1CDE5}');
        assert_eq!(octant_glyph(0xFF), '█');
    }

    #[test]
    fn preview_scaling_uses_the_selected_vertical_bounds() {
        let mut cells = [[false; 2]; 4];
        cells[2][1] = true;

        assert_eq!(selected_bounds(&cells).unwrap().min_row, 2);
        assert_eq!(vertically_scaled_mask(&cells, 3), 0b10_10_10);
        assert_eq!(vertically_scaled_mask(&cells, 2), 0b10_10);
        assert_eq!(
            SEXTANTS[usize::from(vertically_scaled_mask(&cells, 3))],
            '▐'
        );
        assert_eq!(
            QUADRANTS[usize::from(vertically_scaled_mask(&cells, 2))],
            '▐'
        );
    }

    #[test]
    fn preview_scaling_keeps_aabb_endpoints() {
        let mut cells = [[false; 2]; 4];
        cells[0][0] = true;
        cells[3][1] = true;

        assert_eq!(vertically_scaled_mask(&cells, 3), 0b10_00_01);
        assert_eq!(vertically_scaled_mask(&cells, 2), 0b10_01);
        assert_eq!(
            QUADRANTS[usize::from(vertically_scaled_mask(&cells, 2))],
            '▚'
        );
    }

    #[test]
    fn halfblock_output_exactly_represents_two_by_two_masks() {
        assert_eq!(halfblock_output(0b0011), "▀");
        assert_eq!(halfblock_output(0b1100), "▄");
        assert_eq!(halfblock_output(0b0101), "▌");
        assert_eq!(halfblock_output(0b1010), "▐");
        assert_eq!(halfblock_output(0b1001), "▀▄");
        assert_eq!(halfblock_output(0b0111), "█▀");
    }

    #[test]
    fn layout_is_centered_and_rejects_small_terminals() {
        let layout = AppLayout::new(Rect::new(0, 0, 59, 23)).unwrap();

        assert_eq!(layout.title, Rect::new(10, 5, APP_WIDTH, 1));
        assert_eq!(layout.grid, Rect::new(12, 7, GRID_WIDTH, GRID_HEIGHT));
        assert_eq!(layout.select_all, Rect::new(15, 16, 5, 1));
        assert_eq!(layout.outputs, Rect::new(22, 7, 25, 9));
        assert!(AppLayout::new(Rect::new(0, 0, 38, 13)).is_none());
        assert!(AppLayout::new(Rect::new(0, 0, 39, 12)).is_none());
    }
}
