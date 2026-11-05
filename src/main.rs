use std::io::{self, stdout};

use clap::Parser;
use crossterm::{
    clipboard::CopyToClipboard,
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
};
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    symbols::pixel::{QUADRANTS, SEXTANTS},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Widget},
};

const GRID_WIDTH: u16 = 7;
const GRID_HEIGHT: u16 = 9;
const APP_WIDTH: u16 = 43;
const APP_HEIGHT: u16 = 15;
const OUTPUT_CODEPOINT_COLUMN: usize = 10;
const OUTPUT_GLYPH_COLUMN: usize = 19;
const HOTKEY_COLOR: Color = Color::Indexed(11);
const GLYPH_COLOR: Color = Color::Indexed(10);
// Unicode dot order for row-major 2x4 cells: left 1, 2, 3, 7; right 4, 5, 6, 8.
const BRAILLE_DOT_BITS: [u8; 8] = [0, 3, 1, 4, 2, 5, 6, 7];

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
        let _input_modes = InputModes::enable()?;
        App::new(cli.scaling).run(terminal)
    })
}

#[derive(Default)]
struct App {
    cells: [[bool; 2]; 4],
    scaling: bool,
    glyph_background: bool,
    grid_area: Option<Rect>,
    select_all_area: Option<Rect>,
    select_none_area: Option<Rect>,
    should_quit: bool,
}

impl App {
    fn new(scaling: bool) -> Self {
        Self {
            scaling,
            glyph_background: true,
            ..Self::default()
        }
    }

    fn run(&mut self, terminal: &mut ratatui::DefaultTerminal) -> io::Result<()> {
        while !self.should_quit {
            terminal.draw(|frame| self.render(frame))?;
            if let Some(glyph) = self.handle_event(event::read()?) {
                execute!(
                    stdout(),
                    CopyToClipboard::to_clipboard_from(glyph.to_string())
                )?;
            }
        }
        Ok(())
    }

    fn render(&mut self, frame: &mut Frame) {
        let Some(layout) = AppLayout::new(frame.area()) else {
            self.grid_area = None;
            self.select_all_area = None;
            self.select_none_area = None;
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
        self.select_none_area = Some(layout.select_none);

        frame.render_widget(
            Paragraph::new("Octant Builder")
                .alignment(Alignment::Center)
                .style(Style::default().fg(Color::Indexed(15))),
            layout.title,
        );
        frame.render_widget(CellGrid::new(&self.cells), layout.grid);
        let mut glyph_style = Style::default().fg(GLYPH_COLOR);
        if self.glyph_background {
            glyph_style = glyph_style.bg(Color::Indexed(8));
        }
        let output_text = if self.scaling {
            scaled_outputs_text(&self.cells, glyph_style)
        } else {
            exact_outputs_text(self.mask(), glyph_style)
        };
        let outputs = Paragraph::new(output_text).block(Block::new().borders(Borders::ALL));
        frame.render_widget(outputs, layout.outputs);

        frame.render_widget(Paragraph::new(help_text()), layout.help);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("esc", Style::default().fg(HOTKEY_COLOR)),
                Span::raw(" quit"),
            ]))
            .alignment(Alignment::Center),
            layout.quit,
        );
    }

    fn handle_event(&mut self, event: Event) -> Option<char> {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                let ctrl_c =
                    key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
                if key.code == KeyCode::Esc || ctrl_c {
                    self.should_quit = true;
                } else if key.modifiers.is_empty()
                    && let KeyCode::Char(hotkey) = key.code
                {
                    if hotkey == 'g' {
                        self.glyph_background = !self.glyph_background;
                        return None;
                    }
                    if matches!(hotkey, 'a' | 'n') {
                        self.set_all(hotkey == 'a');
                        return None;
                    }
                    if hotkey == 'b' {
                        return Some(braille_glyph(self.mask()));
                    }
                    let outputs = if self.scaling {
                        scaled_outputs(&self.cells)
                    } else {
                        exact_outputs(self.mask())
                    };
                    if let Some(output) = outputs.into_iter().find(|output| output.hotkey == hotkey)
                    {
                        return Some(output.glyph);
                    }
                    self.load_glyph(hotkey);
                }
            }
            Event::Paste(text) => {
                let mut characters = text.trim_matches(['\r', '\n']).chars();
                if let Some(glyph) = characters.next()
                    && characters.next().is_none()
                {
                    self.load_glyph(glyph);
                }
            }
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            _ => {}
        }
        None
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return;
        }

        if self
            .select_all_area
            .is_some_and(|area| rect_contains(area, mouse.column, mouse.row))
        {
            self.set_all(true);
            return;
        }
        if self
            .select_none_area
            .is_some_and(|area| rect_contains(area, mouse.column, mouse.row))
        {
            self.set_all(false);
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

    fn load_glyph(&mut self, glyph: char) {
        if let Some(mask) = glyph_mask(glyph) {
            self.cells = std::array::from_fn(|row| {
                std::array::from_fn(|column| mask & (1 << (row * 2 + column)) != 0)
            });
        }
    }

    fn set_all(&mut self, filled: bool) {
        self.cells.fill([filled; 2]);
    }

    fn toggle_cell(&mut self, row: usize, column: usize) {
        let cell = &mut self.cells[row][column];
        *cell = !*cell;
    }
}

fn help_text() -> Line<'static> {
    let hotkey_style = Style::default().fg(HOTKEY_COLOR);
    Line::from(vec![
        Span::styled("b", hotkey_style),
        Span::raw("/"),
        Span::styled("q", hotkey_style),
        Span::raw("/"),
        Span::styled("s", hotkey_style),
        Span::raw("/"),
        Span::styled("c", hotkey_style),
        Span::raw(" copy • select "),
        Span::styled("a", hotkey_style),
        Span::raw("ll/"),
        Span::styled("n", hotkey_style),
        Span::raw("one • back"),
        Span::styled("g", hotkey_style),
        Span::raw("round"),
    ])
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
    hotkey: char,
    glyph: char,
}

fn exact_outputs(mask: u8) -> Vec<CharacterOutput> {
    let mut outputs = vec![CharacterOutput {
        name: "Braille",
        hotkey: 'b',
        glyph: braille_glyph(mask),
    }];

    if let Some(mask) = top_aligned_mask(mask, 2) {
        outputs.push(CharacterOutput {
            name: "Quadrant",
            hotkey: 'q',
            glyph: QUADRANTS[usize::from(mask)],
        });
    }
    if let Some(mask) = top_aligned_mask(mask, 3) {
        outputs.push(CharacterOutput {
            name: "Sextant",
            hotkey: 's',
            glyph: SEXTANTS[usize::from(mask)],
        });
    }
    outputs.push(CharacterOutput {
        name: "Octant",
        hotkey: 'c',
        glyph: octant_glyph(mask),
    });
    outputs
}

fn exact_outputs_text(mask: u8, glyph_style: Style) -> Text<'static> {
    outputs_text(exact_outputs(mask), glyph_style)
}

fn outputs_text(outputs: Vec<CharacterOutput>, glyph_style: Style) -> Text<'static> {
    let mut lines = Vec::new();
    for output in outputs {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        let mut line = Line::from(
            output
                .name
                .to_ascii_lowercase()
                .chars()
                .map(|letter| {
                    let style = if letter.eq_ignore_ascii_case(&output.hotkey) {
                        Style::default().fg(HOTKEY_COLOR)
                    } else {
                        Style::default()
                    };
                    Span::styled(letter.to_string(), style)
                })
                .collect::<Vec<_>>(),
        );
        line.spans.insert(0, Span::raw(" "));
        line.push_span(" ".repeat(OUTPUT_CODEPOINT_COLUMN.saturating_sub(line.width())));
        line.push_span(Span::styled(
            format!("U+{:04X}", output.glyph as u32),
            Style::default().fg(Color::Indexed(8)),
        ));
        line.push_span(" ".repeat(OUTPUT_GLYPH_COLUMN.saturating_sub(line.width())));
        line.push_span(Span::styled(output.glyph.to_string(), glyph_style));
        lines.push(line);
    }
    Text::from(lines)
}

fn scaled_outputs(cells: &[[bool; 2]; 4]) -> Vec<CharacterOutput> {
    vec![
        CharacterOutput {
            name: "Octant",
            hotkey: 'c',
            glyph: octant_glyph(cells_mask(cells)),
        },
        CharacterOutput {
            name: "Sextant",
            hotkey: 's',
            glyph: SEXTANTS[usize::from(vertically_scaled_mask(cells, 3))],
        },
        CharacterOutput {
            name: "Quadrant",
            hotkey: 'q',
            glyph: QUADRANTS[usize::from(vertically_scaled_mask(cells, 2))],
        },
    ]
}

fn scaled_outputs_text(cells: &[[bool; 2]; 4], glyph_style: Style) -> Text<'static> {
    let mut outputs = outputs_text(scaled_outputs(cells), glyph_style);
    let halfblock = halfblock_output(vertically_scaled_mask(cells, 2));
    outputs.lines.push(Line::default());
    let mut line = Line::from(" halfblock");
    line.push_span(" ".repeat(OUTPUT_GLYPH_COLUMN.saturating_sub(line.width())));
    line.push_span(Span::styled(halfblock, glyph_style));
    outputs.lines.push(line);
    outputs
}

fn top_aligned_mask(mask: u8, height: usize) -> Option<u8> {
    debug_assert!((1..=3).contains(&height));
    (mask >> (height * 2) == 0).then_some(mask)
}

fn braille_glyph(mask: u8) -> char {
    let dots = BRAILLE_DOT_BITS
        .into_iter()
        .enumerate()
        .fold(0, |dots, (cell_bit, dot_bit)| {
            dots | ((mask >> cell_bit & 1) << dot_bit)
        });
    char::from_u32(0x2800 + u32::from(dots)).expect("braille code point is valid")
}

fn glyph_mask(glyph: char) -> Option<u8> {
    if ('\u{2800}'..='\u{28FF}').contains(&glyph) {
        let dots = (glyph as u32 - 0x2800) as u8;
        return Some(
            BRAILLE_DOT_BITS
                .into_iter()
                .enumerate()
                .fold(0, |mask, (cell_bit, dot_bit)| {
                    mask | ((dots >> dot_bit & 1) << cell_bit)
                }),
        );
    }

    // Shared block glyphs retain their full 2x4 octant shape.
    (u8::MIN..=u8::MAX)
        .find(|mask| octant_glyph(*mask) == glyph)
        .or_else(|| {
            QUADRANTS
                .iter()
                .position(|candidate| *candidate == glyph)
                .or_else(|| SEXTANTS.iter().position(|candidate| *candidate == glyph))
                .map(|mask| mask as u8)
        })
}

struct InputModes;

impl InputModes {
    fn enable() -> io::Result<Self> {
        let modes = Self;
        execute!(stdout(), EnableMouseCapture, EnableBracketedPaste)?;
        Ok(modes)
    }
}

impl Drop for InputModes {
    fn drop(&mut self) {
        let _ = execute!(stdout(), DisableBracketedPaste, DisableMouseCapture);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AppLayout {
    title: Rect,
    grid: Rect,
    select_all: Rect,
    select_none: Rect,
    outputs: Rect,
    help: Rect,
    quit: Rect,
}

impl AppLayout {
    fn new(area: Rect) -> Option<Self> {
        if area.width < APP_WIDTH || area.height < APP_HEIGHT {
            return None;
        }

        let x = area.x + (area.width - APP_WIDTH) / 2;
        let y = area.y + (area.height - APP_HEIGHT) / 2;
        let select_all_x = x + Line::from("b/q/s/c copy • select ").width() as u16;
        Some(Self {
            title: Rect::new(x, y, APP_WIDTH, 1),
            grid: Rect::new(x + 4, y + 2, GRID_WIDTH, GRID_HEIGHT),
            select_all: Rect::new(select_all_x, y + 12, 3, 1),
            select_none: Rect::new(select_all_x + 4, y + 12, 4, 1),
            outputs: Rect::new(x + 14, y + 2, 24, 9),
            help: Rect::new(x, y + 12, APP_WIDTH, 1),
            quit: Rect::new(x, y + 14, APP_WIDTH, 1),
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
    fn pasted_and_unbracketed_glyphs_replace_the_grid() {
        for (glyph, expected) in [
            ('\u{2884}', 0x90),
            ('\u{2597}', 0xA0),
            ('\u{1FB1E}', 0x20),
            ('\u{1CD80}', 0x90),
            ('\u{2598}', 0x05),
            ('\u{2588}', 0xFF),
            ('\u{2800}', 0x00),
            (' ', 0x00),
            ('\u{00A0}', 0x00),
        ] {
            for event in [
                Event::Paste(glyph.to_string()),
                Event::Key(crossterm::event::KeyEvent::new(
                    KeyCode::Char(glyph),
                    KeyModifiers::NONE,
                )),
            ] {
                let mut app = App::default();
                app.set_all(true);

                assert_eq!(app.handle_event(event), None);
                assert_eq!(app.mask(), expected, "glyph {glyph:?}");
                assert!(!app.should_quit);
            }
        }
    }

    #[test]
    fn all_supported_glyphs_load_and_appear_in_exact_outputs() {
        for mask in u8::MIN..=u8::MAX {
            assert_eq!(glyph_mask(braille_glyph(mask)), Some(mask));
            assert_eq!(glyph_mask(octant_glyph(mask)), Some(mask));
        }

        let glyphs = QUADRANTS
            .into_iter()
            .chain(SEXTANTS)
            .chain((u8::MIN..=u8::MAX).map(octant_glyph));
        for glyph in glyphs {
            let mask = glyph_mask(glyph).expect("supported glyph decodes");
            assert!(
                exact_outputs(mask)
                    .iter()
                    .any(|output| output.glyph == glyph),
                "glyph {glyph:?} was not reproduced after loading mask {mask:#04x}",
            );
        }
    }

    #[test]
    fn paste_requires_one_supported_glyph_and_preserves_blank_characters() {
        let mut app = App::default();
        app.cells[3][1] = true;

        for text in [
            "",
            "hello",
            "n",
            "q",
            "c",
            "\u{2801}\u{2802}",
            " \u{2801}",
            "\u{001B}",
        ] {
            assert_eq!(app.handle_event(Event::Paste(text.to_owned())), None);
            assert_eq!(app.mask(), 0x80, "paste {text:?}");
            assert!(!app.should_quit);
        }

        app.handle_event(Event::Paste("\r\n\u{2884}\n".to_owned()));
        assert_eq!(app.mask(), 0x90);
        app.handle_event(Event::Paste("\u{00A0}\n".to_owned()));
        assert_eq!(app.mask(), 0x00);
    }

    #[test]
    fn exact_outputs_only_use_top_aligned_compatible_grids() {
        assert_eq!(
            exact_outputs(0x01),
            vec![
                CharacterOutput {
                    name: "Braille",
                    hotkey: 'b',
                    glyph: '\u{2801}',
                },
                CharacterOutput {
                    name: "Quadrant",
                    hotkey: 'q',
                    glyph: '\u{2598}',
                },
                CharacterOutput {
                    name: "Sextant",
                    hotkey: 's',
                    glyph: '\u{1FB00}',
                },
                CharacterOutput {
                    name: "Octant",
                    hotkey: 'c',
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
    fn selection_hotkeys_fill_and_clear_partial_grids() {
        let mut app = App::default();
        for hotkey in ['a', 'a', 'n', 'n'] {
            app.toggle_cell(2, 1);
            assert_eq!(
                app.handle_event(Event::Key(crossterm::event::KeyEvent::new(
                    KeyCode::Char(hotkey),
                    KeyModifiers::NONE,
                ))),
                None,
            );
            assert!(
                app.cells
                    .iter()
                    .flatten()
                    .all(|cell| *cell == (hotkey == 'a'))
            );
        }
    }

    #[test]
    fn footer_clicks_select_all_and_none() {
        let layout = AppLayout::new(Rect::new(0, 0, APP_WIDTH, APP_HEIGHT)).unwrap();
        let mut app = App {
            select_all_area: Some(layout.select_all),
            select_none_area: Some(layout.select_none),
            ..App::default()
        };
        for (area, filled) in [(layout.select_all, true), (layout.select_none, false)] {
            app.toggle_cell(2, 1);
            app.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: area.x,
                row: area.y,
                modifiers: KeyModifiers::NONE,
            });
            assert!(app.cells.iter().flatten().all(|cell| *cell == filled));
        }
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

        assert_eq!(layout.title, Rect::new(8, 4, APP_WIDTH, 1));
        assert_eq!(layout.grid, Rect::new(12, 6, GRID_WIDTH, GRID_HEIGHT));
        assert_eq!(layout.select_all, Rect::new(30, 16, 3, 1));
        assert_eq!(layout.select_none, Rect::new(34, 16, 4, 1));
        assert_eq!(layout.outputs, Rect::new(22, 6, 24, 9));
        assert_eq!(layout.quit, Rect::new(8, 18, APP_WIDTH, 1));
        assert!(AppLayout::new(Rect::new(0, 0, 42, 15)).is_none());
        assert!(AppLayout::new(Rect::new(0, 0, 43, 14)).is_none());
    }
}
