use std::io::{self, stdout};

mod eighths;
mod sixteenths;

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
const GRID_OUTPUT_GAP: u16 = 3;
const OUTPUT_CODEPOINT_COLUMN: usize = 14;
const OUTPUT_GLYPH_COLUMN: usize = 23;
const HOTKEY_COLOR: Color = Color::Indexed(11);
const GLYPH_COLOR: Color = Color::Indexed(10);
// Unicode dot order for row-major 2x4 cells: left 1, 2, 3, 7; right 4, 5, 6, 8.
const BRAILLE_DOT_BITS: [u8; 8] = [0, 3, 1, 4, 2, 5, 6, 7];

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum GridMode {
    #[default]
    Octants,
    Sixteenths,
    Eighths,
}

impl GridMode {
    fn next(self) -> Self {
        match self {
            Self::Octants => Self::Sixteenths,
            Self::Sixteenths => Self::Eighths,
            Self::Eighths => Self::Octants,
        }
    }

    fn grid_label(self) -> &'static str {
        match self {
            Self::Octants => "2×4 grid",
            Self::Sixteenths => "4×4 grid",
            Self::Eighths => "8×8 grid",
        }
    }

    fn grid_width(self) -> u16 {
        match self {
            Self::Octants => GRID_WIDTH,
            Self::Sixteenths => 13,
            Self::Eighths => 25,
        }
    }

    fn grid_height(self) -> u16 {
        match self {
            Self::Octants | Self::Sixteenths => GRID_HEIGHT,
            Self::Eighths => 17,
        }
    }

    fn minimum_width(self) -> u16 {
        self.grid_width() + GRID_OUTPUT_GAP + output_width(false)
    }

    fn output_height(self) -> u16 {
        match self {
            Self::Octants => 13,
            Self::Sixteenths => 15,
            Self::Eighths => 17,
        }
    }

    fn copy_hotkeys(self) -> &'static str {
        match self {
            Self::Octants => "b/q/Q/s/S/c",
            Self::Sixteenths => "b/q/Q/s/S/c/t",
            Self::Eighths => "b/q/Q/s/S/c/t/k",
        }
    }
}

#[derive(Debug, Parser)]
#[command(version, about = "Build Unicode terminal block characters")]
struct Cli {
    /// Scale the selected vertical bounds to each output's height in 2x4 mode
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
    octants: SelectionCanvas<2, 4>,
    sixteenths: SelectionCanvas<4, 4>,
    eighths: SelectionCanvas<8, 8>,
    mode: GridMode,
    scaling: bool,
    glyph_background: bool,
    grid_area: Option<Rect>,
    select_all_area: Option<Rect>,
    select_none_area: Option<Rect>,
    draw_stroke: Option<DrawStroke>,
    wheel_cursor: Option<WheelCursor>,
    should_quit: bool,
}

struct SelectionCanvas<const COLUMNS: usize, const ROWS: usize> {
    cells: Vec<bool>,
    top: usize,
    left: usize,
}

impl<const COLUMNS: usize, const ROWS: usize> Default for SelectionCanvas<COLUMNS, ROWS> {
    fn default() -> Self {
        let (top, left) = Self::CENTER;
        Self {
            cells: vec![false; Self::WIDTH * Self::HEIGHT],
            top,
            left,
        }
    }
}

impl<const COLUMNS: usize, const ROWS: usize> SelectionCanvas<COLUMNS, ROWS> {
    const WIDTH: usize = COLUMNS * 16;
    const HEIGHT: usize = ROWS * 16;
    const CENTER: (usize, usize) = ((Self::HEIGHT - ROWS) / 2, (Self::WIDTH - COLUMNS) / 2);

    fn index(&self, row: usize, column: usize) -> usize {
        (self.top + row) * Self::WIDTH + self.left + column
    }

    fn cell_mut(&mut self, row: usize, column: usize) -> &mut bool {
        let index = self.index(row, column);
        &mut self.cells[index]
    }

    fn visible(&self) -> [[bool; COLUMNS]; ROWS] {
        std::array::from_fn(|row| std::array::from_fn(|column| self.cells[self.index(row, column)]))
    }

    fn mask(&self) -> u64 {
        cells_mask(&self.visible())
    }

    fn replace_visible(&mut self, mask: u64) {
        for (row, cells) in cells_from_mask::<COLUMNS, ROWS>(mask)
            .into_iter()
            .enumerate()
        {
            let start = self.index(row, 0);
            self.cells[start..start + COLUMNS].copy_from_slice(&cells);
        }
    }

    fn reset(&mut self, filled: bool) {
        self.cells.fill(false);
        (self.top, self.left) = Self::CENTER;
        if filled {
            self.replace_visible(u64::MAX);
        }
    }

    fn shift(&mut self, row_delta: isize, column_delta: isize) {
        // Move the viewport opposite the requested tile movement.
        self.top = self
            .top
            .saturating_add_signed(-row_delta)
            .min(Self::HEIGHT - ROWS);
        self.left = self
            .left
            .saturating_add_signed(-column_delta)
            .min(Self::WIDTH - COLUMNS);
    }
}

#[derive(Clone, Copy)]
struct WheelCursor {
    mode: GridMode,
    row: usize,
    column: usize,
    filled: bool,
}

struct DrawStroke {
    filled: bool,
    last_cell: Option<(usize, usize)>,
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
        let layout = match AppLayout::new(frame.area(), self.mode) {
            Ok(layout) => layout,
            Err(size) => {
                self.draw_stroke = None;
                self.grid_area = None;
                self.select_all_area = None;
                self.select_none_area = None;
                frame.render_widget(
                    Paragraph::new(format!(
                        "Terminal too small (need {} x {})",
                        size.width, size.height,
                    ))
                    .alignment(Alignment::Center),
                    frame.area(),
                );
                return;
            }
        };
        if self.grid_area != Some(layout.grid) {
            self.draw_stroke = None;
        }
        self.grid_area = Some(layout.grid);
        self.select_all_area = Some(layout.select_all);
        self.select_none_area = Some(layout.select_none);

        let mut glyph_style = Style::default().fg(GLYPH_COLOR);
        if self.glyph_background {
            glyph_style = glyph_style.bg(Color::Indexed(8));
        }
        let output_text = match self.mode {
            GridMode::Octants => {
                let cells = self.octants.visible();
                frame.render_widget(CellGrid::new(&cells), layout.grid);
                if self.scaling {
                    scaled_outputs_text(&cells, glyph_style, layout.show_codepoints)
                } else {
                    exact_outputs_text(
                        cells_mask(&cells) as u8,
                        glyph_style,
                        layout.show_codepoints,
                    )
                }
            }
            GridMode::Sixteenths => {
                let cells = self.sixteenths.visible();
                frame.render_widget(CellGrid::new(&cells), layout.grid);
                sixteenth_outputs_text(
                    cells_mask(&cells) as u16,
                    glyph_style,
                    layout.show_codepoints,
                )
            }
            GridMode::Eighths => {
                let cells = self.eighths.visible();
                frame.render_widget(CellGrid::new(&cells), layout.grid);
                eighth_outputs_text(cells_mask(&cells), glyph_style, layout.show_codepoints)
            }
        };
        let block = Block::new().borders(Borders::ALL);
        let mut output_area = block.inner(layout.outputs);
        if self.mode != GridMode::Octants {
            let padding = output_area
                .height
                .saturating_sub(output_text.height() as u16)
                / 2;
            output_area.y += padding;
            output_area.height -= padding;
        }
        frame.render_widget(block, layout.outputs);
        frame.render_widget(Paragraph::new(output_text), output_area);

        frame.render_widget(
            Paragraph::new(layout.help_text).alignment(Alignment::Center),
            layout.help,
        );
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Shift+Arrows", Style::default().fg(HOTKEY_COLOR)),
                Span::raw(" move • "),
                Span::styled("esc", Style::default().fg(HOTKEY_COLOR)),
                Span::raw(" quit"),
            ]))
            .alignment(Alignment::Center),
            layout.footer,
        );
    }

    fn handle_event(&mut self, event: Event) -> Option<char> {
        // Keyboard input, paste, focus changes, and resizing end a mouse stroke.
        if !matches!(event, Event::Mouse(_)) {
            self.draw_stroke = None;
        }
        match event {
            Event::Key(key) => {
                if key.modifiers == KeyModifiers::SHIFT
                    && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
                {
                    let delta = match key.code {
                        KeyCode::Up => Some((-1, 0)),
                        KeyCode::Down => Some((1, 0)),
                        KeyCode::Left => Some((0, -1)),
                        KeyCode::Right => Some((0, 1)),
                        _ => None,
                    };
                    if let Some((row, column)) = delta {
                        self.shift_selection(row, column);
                        return None;
                    }
                }
                if key.kind != KeyEventKind::Press {
                    return None;
                }
                let ctrl_c =
                    key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
                if key.code == KeyCode::Esc || ctrl_c {
                    self.should_quit = true;
                } else if key.code == KeyCode::Tab && key.modifiers.is_empty() {
                    self.mode = self.mode.next();
                } else if (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
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
                    if self.mode == GridMode::Octants && hotkey == 'b' {
                        return Some(braille_glyph(self.mask()));
                    }
                    let outputs = if self.mode == GridMode::Eighths {
                        eighth_outputs(self.eighths.mask())
                    } else if self.mode == GridMode::Sixteenths {
                        sixteenth_outputs(self.sixteenths.mask() as u16)
                    } else if self.scaling {
                        scaled_outputs(&self.octants.visible())
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
        let cell = self
            .grid_area
            .and_then(|grid| grid_cell_at(grid, mouse.column, mouse.row));
        match mouse.kind {
            MouseEventKind::Down(button) => {
                self.draw_stroke = None;
                self.wheel_cursor = None;
                if button != MouseButton::Left {
                    return;
                }
                if self
                    .select_all_area
                    .is_some_and(|area| rect_contains(area, mouse.column, mouse.row))
                {
                    self.set_all(true);
                } else if self
                    .select_none_area
                    .is_some_and(|area| rect_contains(area, mouse.column, mouse.row))
                {
                    self.set_all(false);
                } else if let Some((row, column)) = cell {
                    self.toggle_cell(row, column);
                    let filled = *self.cell_mut(row, column);
                    self.draw_stroke = Some(DrawStroke {
                        filled,
                        last_cell: Some((row, column)),
                    });
                    self.wheel_cursor = Some(WheelCursor {
                        mode: self.mode,
                        row,
                        column,
                        filled,
                    });
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some((row, column)) = cell {
                    self.paint_to(row, column);
                } else if self
                    .grid_area
                    .is_none_or(|grid| !rect_contains(grid, mouse.column, mouse.row))
                    && let Some(stroke) = &mut self.draw_stroke
                {
                    // Re-entering the grid starts a new segment, without a line
                    // across cells the pointer skipped while outside the grid.
                    stroke.last_cell = None;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some((row, column)) = cell {
                    self.paint_to(row, column);
                }
                self.draw_stroke = None;
            }
            MouseEventKind::Moved => self.draw_stroke = None,
            MouseEventKind::ScrollUp => self.wheel_step(1, 0),
            MouseEventKind::ScrollDown => self.wheel_step(-1, 0),
            MouseEventKind::ScrollLeft => self.wheel_step(0, 1),
            MouseEventKind::ScrollRight => self.wheel_step(0, -1),
            _ => {}
        }
    }

    fn wheel_step(&mut self, row_delta: isize, column_delta: isize) {
        self.draw_stroke = None;
        let Some(mut cursor) = self.wheel_cursor else {
            return;
        };
        if cursor.mode != self.mode || self.grid_area.is_none() {
            return;
        }
        let (Some(row), Some(column)) = (
            cursor.row.checked_add_signed(row_delta),
            cursor.column.checked_add_signed(column_delta),
        ) else {
            return;
        };
        let rows = usize::from((self.mode.grid_height() - 1) / 2);
        let columns = usize::from((self.mode.grid_width() - 1) / 3);
        if row >= rows || column >= columns {
            return;
        }
        *self.cell_mut(row, column) = cursor.filled;
        cursor.row = row;
        cursor.column = column;
        self.wheel_cursor = Some(cursor);
    }

    fn paint_to(&mut self, row: usize, column: usize) {
        let Some(mut stroke) = self.draw_stroke.take() else {
            return;
        };
        let (start_row, start_column) = stroke.last_cell.unwrap_or((row, column));
        let (mut y, mut x) = (start_row as i32, start_column as i32);
        let (end_y, end_x) = (row as i32, column as i32);
        let (dx, dy) = ((end_x - x).abs(), -(end_y - y).abs());
        let (step_x, step_y) = ((end_x - x).signum(), (end_y - y).signum());
        let mut error = dx + dy;
        // Join sparse mouse reports so fast strokes do not leave holes.
        loop {
            *self.cell_mut(y as usize, x as usize) = stroke.filled;
            if (y, x) == (end_y, end_x) {
                break;
            }
            let doubled_error = 2 * error;
            if doubled_error >= dy {
                error += dy;
                x += step_x;
            }
            if doubled_error <= dx {
                error += dx;
                y += step_y;
            }
        }
        stroke.last_cell = Some((row, column));
        self.draw_stroke = Some(stroke);
    }

    fn mask(&self) -> u8 {
        self.octants.mask() as u8
    }

    fn load_glyph(&mut self, glyph: char) {
        // Shared glyphs keep the active grid; unique glyphs select their grid.
        let octants = glyph_mask(glyph);
        let sixteenths = sixteenths::mask(glyph);
        let eighths = eighths::mask(glyph);
        if self.mode == GridMode::Eighths
            && let Some(mask) = eighths
        {
            self.eighths.replace_visible(mask);
        } else if self.mode == GridMode::Sixteenths
            && let Some(mask) = sixteenths
        {
            self.sixteenths.replace_visible(u64::from(mask));
        } else if let Some(mask) = octants {
            self.mode = GridMode::Octants;
            self.octants.replace_visible(u64::from(mask));
        } else if let Some(mask) = sixteenths {
            self.mode = GridMode::Sixteenths;
            self.sixteenths.replace_visible(u64::from(mask));
        } else if let Some(mask) = eighths {
            self.mode = GridMode::Eighths;
            self.eighths.replace_visible(mask);
        }
    }

    fn set_all(&mut self, filled: bool) {
        match self.mode {
            GridMode::Octants => self.octants.reset(filled),
            GridMode::Sixteenths => self.sixteenths.reset(filled),
            GridMode::Eighths => self.eighths.reset(filled),
        }
    }

    fn shift_selection(&mut self, row_delta: isize, column_delta: isize) {
        match self.mode {
            GridMode::Octants => self.octants.shift(row_delta, column_delta),
            GridMode::Sixteenths => self.sixteenths.shift(row_delta, column_delta),
            GridMode::Eighths => self.eighths.shift(row_delta, column_delta),
        }
    }

    fn toggle_cell(&mut self, row: usize, column: usize) {
        let cell = self.cell_mut(row, column);
        *cell = !*cell;
    }

    fn cell_mut(&mut self, row: usize, column: usize) -> &mut bool {
        match self.mode {
            GridMode::Octants => self.octants.cell_mut(row, column),
            GridMode::Sixteenths => self.sixteenths.cell_mut(row, column),
            GridMode::Eighths => self.eighths.cell_mut(row, column),
        }
    }
}

fn help_segments(mode: GridMode) -> [Line<'static>; 4] {
    let hotkey_style = Style::default().fg(HOTKEY_COLOR);
    let mut spans: Vec<_> = mode
        .copy_hotkeys()
        .chars()
        .map(|letter| {
            let style = if letter == '/' {
                Style::default()
            } else {
                hotkey_style
            };
            Span::styled(letter.to_string(), style)
        })
        .collect();
    spans.push(Span::raw(" copy"));
    [
        Line::from(spans),
        Line::from(vec![
            Span::raw("select "),
            Span::styled("a", hotkey_style),
            Span::raw("ll/"),
            Span::styled("n", hotkey_style),
            Span::raw("one"),
        ]),
        Line::from(vec![
            Span::raw("back"),
            Span::styled("g", hotkey_style),
            Span::raw("round"),
        ]),
        Line::from(vec![
            Span::styled("tab", hotkey_style),
            Span::raw(format!(" {}", mode.next().grid_label())),
        ]),
    ]
}

struct KeyHints {
    text: Text<'static>,
    selection_row: u16,
    selection_column: u16,
}

fn key_hints(mode: GridMode, width: u16) -> KeyHints {
    let mut lines = Vec::new();
    let mut line = Line::default();
    let mut selection_row = 0;
    let mut selection_column = 0;
    let separator = " • ";
    let separator_width = Line::from(separator).width();
    for (index, segment) in help_segments(mode).into_iter().enumerate() {
        if line.width() > 0 && line.width() + separator_width + segment.width() > usize::from(width)
        {
            lines.push(line);
            line = Line::default();
        }
        if line.width() > 0 {
            line.push_span(separator);
        }
        if index == 1 {
            selection_row = lines.len() as u16;
            selection_column = (line.width() + Line::from("select ").width()) as u16;
        }
        line.spans.extend(segment.spans);
    }
    lines.push(line);
    KeyHints {
        text: Text::from(lines),
        selection_row,
        selection_column,
    }
}

fn cells_mask<const COLUMNS: usize, const ROWS: usize>(cells: &[[bool; COLUMNS]; ROWS]) -> u64 {
    cells
        .iter()
        .flatten()
        .enumerate()
        .fold(0, |mask, (index, filled)| {
            mask | (u64::from(*filled) << index)
        })
}

fn cells_from_mask<const COLUMNS: usize, const ROWS: usize>(mask: u64) -> [[bool; COLUMNS]; ROWS] {
    std::array::from_fn(|row| {
        std::array::from_fn(|column| mask & (1 << (row * COLUMNS + column)) != 0)
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
        outputs.push(CharacterOutput {
            name: "Quadrant sep",
            hotkey: 'Q',
            glyph: separated_mosaic_glyph(mask, 0x1CC20),
        });
    }
    if let Some(mask) = top_aligned_mask(mask, 3) {
        outputs.push(CharacterOutput {
            name: "Sextant",
            hotkey: 's',
            glyph: SEXTANTS[usize::from(mask)],
        });
        outputs.push(CharacterOutput {
            name: "Sextant sep",
            hotkey: 'S',
            glyph: separated_mosaic_glyph(mask, 0x1CE50),
        });
    }
    outputs.push(CharacterOutput {
        name: "Octant",
        hotkey: 'c',
        glyph: octant_glyph(mask),
    });
    outputs
}

fn exact_outputs_text(mask: u8, glyph_style: Style, show_codepoints: bool) -> Text<'static> {
    outputs_text(exact_outputs(mask), glyph_style, show_codepoints)
}

fn sixteenth_outputs(mask: u16) -> Vec<CharacterOutput> {
    let mut outputs = sixteenths::octant_mask(mask)
        .map(exact_outputs)
        .unwrap_or_default();
    if let Some(glyph) = sixteenths::glyph(mask) {
        outputs.push(CharacterOutput {
            name: "Sixteenth",
            hotkey: 't',
            glyph,
        });
    }
    outputs
}

fn sixteenth_outputs_text(mask: u16, glyph_style: Style, show_codepoints: bool) -> Text<'static> {
    let outputs = sixteenth_outputs(mask);
    if !outputs.is_empty() {
        outputs_text(outputs, glyph_style, show_codepoints)
    } else {
        unavailable_output_text("Sixteenth", 't', show_codepoints)
    }
}

fn eighth_outputs(mask: u64) -> Vec<CharacterOutput> {
    let mut outputs = eighths::sixteenth_mask(mask)
        .map(sixteenth_outputs)
        .unwrap_or_default();
    if let Some(glyph) = eighths::glyph(mask) {
        outputs.push(CharacterOutput {
            name: "Block",
            hotkey: 'k',
            glyph,
        });
    }
    outputs
}

fn eighth_outputs_text(mask: u64, glyph_style: Style, show_codepoints: bool) -> Text<'static> {
    let outputs = eighth_outputs(mask);
    if !outputs.is_empty() {
        outputs_text(outputs, glyph_style, show_codepoints)
    } else {
        unavailable_output_text("Block", 'k', show_codepoints)
    }
}

fn unavailable_output_text(name: &str, hotkey: char, show_codepoints: bool) -> Text<'static> {
    Text::from(vec![
        output_label(name, hotkey),
        Line::default(),
        Line::styled(
            if show_codepoints {
                " No single Unicode glyph"
            } else {
                " No Unicode glyph"
            },
            Style::default().fg(Color::Indexed(8)),
        ),
    ])
}

fn output_label(name: &str, hotkey: char) -> Line<'static> {
    let label = if hotkey.is_ascii_uppercase() {
        name.to_owned()
    } else {
        name.to_ascii_lowercase()
    };
    let mut highlighted = false;
    let mut line = Line::from(
        label
            .chars()
            .map(|letter| {
                let style = if !highlighted && letter == hotkey {
                    highlighted = true;
                    Style::default().fg(HOTKEY_COLOR)
                } else {
                    Style::default()
                };
                Span::styled(letter.to_string(), style)
            })
            .collect::<Vec<_>>(),
    );
    line.spans.insert(0, Span::raw(" "));
    line
}

fn output_glyph_column(show_codepoints: bool) -> usize {
    if show_codepoints {
        OUTPUT_GLYPH_COLUMN
    } else {
        OUTPUT_CODEPOINT_COLUMN
    }
}

fn output_width(show_codepoints: bool) -> u16 {
    // Borders, a glyph (up to two cells for halfblocks), and trailing padding.
    output_glyph_column(show_codepoints) as u16 + 5
}

fn outputs_text(
    outputs: Vec<CharacterOutput>,
    glyph_style: Style,
    show_codepoints: bool,
) -> Text<'static> {
    let mut lines = Vec::new();
    for output in outputs {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        let mut line = output_label(output.name, output.hotkey);
        if show_codepoints {
            line.push_span(" ".repeat(OUTPUT_CODEPOINT_COLUMN.saturating_sub(line.width())));
            line.push_span(Span::styled(
                format!("U+{:04X}", output.glyph as u32),
                Style::default().fg(Color::Indexed(8)),
            ));
        }
        line.push_span(
            " ".repeat(output_glyph_column(show_codepoints).saturating_sub(line.width())),
        );
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
            glyph: octant_glyph(cells_mask(cells) as u8),
        },
        CharacterOutput {
            name: "Sextant",
            hotkey: 's',
            glyph: SEXTANTS[usize::from(vertically_scaled_mask(cells, 3))],
        },
        CharacterOutput {
            name: "Sextant sep",
            hotkey: 'S',
            glyph: separated_mosaic_glyph(vertically_scaled_mask(cells, 3), 0x1CE50),
        },
        CharacterOutput {
            name: "Quadrant",
            hotkey: 'q',
            glyph: QUADRANTS[usize::from(vertically_scaled_mask(cells, 2))],
        },
        CharacterOutput {
            name: "Quadrant sep",
            hotkey: 'Q',
            glyph: separated_mosaic_glyph(vertically_scaled_mask(cells, 2), 0x1CC20),
        },
    ]
}

fn scaled_outputs_text(
    cells: &[[bool; 2]; 4],
    glyph_style: Style,
    show_codepoints: bool,
) -> Text<'static> {
    let mut outputs = outputs_text(scaled_outputs(cells), glyph_style, show_codepoints);
    let halfblock = halfblock_output(vertically_scaled_mask(cells, 2));
    outputs.lines.push(Line::default());
    let mut line = Line::from(" halfblock");
    line.push_span(" ".repeat(output_glyph_column(show_codepoints).saturating_sub(line.width())));
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

fn separated_mosaic_glyph(mask: u8, base: u32) -> char {
    // Nonempty separated quadrants and sextants follow row-major binary order.
    // The preceding code point belongs to another set, so reuse space for zero.
    if mask == 0 {
        ' '
    } else {
        char::from_u32(base + u32::from(mask)).expect("separated mosaic code point is valid")
    }
}

fn glyph_mask(glyph: char) -> Option<u8> {
    match glyph {
        '\u{1CC21}'..='\u{1CC2F}' => return Some((glyph as u32 - 0x1CC20) as u8),
        '\u{1CE51}'..='\u{1CE8F}' => return Some((glyph as u32 - 0x1CE50) as u8),
        _ => {}
    }

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct AppLayout {
    grid: Rect,
    select_all: Rect,
    select_none: Rect,
    outputs: Rect,
    help: Rect,
    help_text: Text<'static>,
    footer: Rect,
    show_codepoints: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MinimumSize {
    width: u16,
    height: u16,
}

impl AppLayout {
    fn new(area: Rect, mode: GridMode) -> Result<Self, MinimumSize> {
        let minimum_width = mode.minimum_width();
        let hints = key_hints(mode, area.width.max(minimum_width));
        let outputs_height = mode.output_height();
        let hints_height = hints.text.height() as u16;
        let height = outputs_height + 1 + hints_height + 1;
        if area.width < minimum_width || area.height < height {
            return Err(MinimumSize {
                width: area.width.max(minimum_width),
                height,
            });
        }

        let show_codepoints =
            area.width >= mode.grid_width() + GRID_OUTPUT_GAP + output_width(true);
        let outputs_width = output_width(show_codepoints);
        let body_width = mode.grid_width() + GRID_OUTPUT_GAP + outputs_width;
        let x = area.x + (area.width - body_width) / 2;
        let y = area.y + (area.height - height) / 2;
        let help = Rect::new(area.x, y + outputs_height + 1, area.width, hints_height);
        let selection_line_width =
            hints.text.lines[usize::from(hints.selection_row)].width() as u16;
        // Match Paragraph's centering when the line and area have different parity.
        let select_all_x =
            help.x + help.width / 2 - selection_line_width / 2 + hints.selection_column;
        let selection_y = help.y + hints.selection_row;
        Ok(Self {
            grid: Rect::new(
                x,
                y + (outputs_height - mode.grid_height()) / 2,
                mode.grid_width(),
                mode.grid_height(),
            ),
            select_all: Rect::new(select_all_x, selection_y, 3, 1),
            select_none: Rect::new(select_all_x + 4, selection_y, 4, 1),
            outputs: Rect::new(
                x + mode.grid_width() + GRID_OUTPUT_GAP,
                y,
                outputs_width,
                outputs_height,
            ),
            help,
            help_text: hints.text,
            footer: Rect::new(area.x, y + height - 1, area.width, 1),
            show_codepoints,
        })
    }
}

fn rect_contains(area: Rect, x: u16, y: u16) -> bool {
    x >= area.x && x < area.right() && y >= area.y && y < area.bottom()
}

struct CellGrid<'a, const COLUMNS: usize, const ROWS: usize> {
    cells: &'a [[bool; COLUMNS]; ROWS],
}

impl<'a, const COLUMNS: usize, const ROWS: usize> CellGrid<'a, COLUMNS, ROWS> {
    fn new(cells: &'a [[bool; COLUMNS]; ROWS]) -> Self {
        Self { cells }
    }
}

impl<const COLUMNS: usize, const ROWS: usize> Widget for CellGrid<'_, COLUMNS, ROWS> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.width < COLUMNS as u16 * 3 + 1 || area.height < ROWS as u16 * 2 + 1 {
            return;
        }

        let top = format!("┌{}┐", ["──"; COLUMNS].join("┬"));
        let middle = format!("├{}┤", ["──"; COLUMNS].join("┼"));
        let bottom = format!("└{}┘", ["──"; COLUMNS].join("┴"));
        let cell_row = format!("│{}│", ["  "; COLUMNS].join("│"));
        let border_style = Style::default().fg(Color::DarkGray);
        let filled_style = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD);

        for row in 0..=ROWS {
            let border = if row == 0 {
                &top
            } else if row == ROWS {
                &bottom
            } else {
                &middle
            };
            buffer.set_string(area.x, area.y + row as u16 * 2, border, border_style);
        }
        for (row, cells) in self.cells.iter().enumerate() {
            let y = area.y + 1 + row as u16 * 2;
            buffer.set_string(area.x, y, &cell_row, border_style);
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
    if local_y.is_multiple_of(2) || local_x.is_multiple_of(3) {
        return None;
    }

    Some(((local_y / 2) as usize, (local_x / 3) as usize))
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

    fn mouse_event(kind: MouseEventKind, column: u16, row: u16) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    }

    fn mouse_on_cell(app: &mut App, kind: MouseEventKind, row: usize, column: usize) {
        let grid = app.grid_area.unwrap();
        app.handle_event(mouse_event(
            kind,
            grid.x + column as u16 * 3 + 1,
            grid.y + row as u16 * 2 + 1,
        ));
    }

    fn active_mask(app: &App) -> u64 {
        match app.mode {
            GridMode::Octants => app.octants.mask(),
            GridMode::Sixteenths => app.sixteenths.mask(),
            GridMode::Eighths => app.eighths.mask(),
        }
    }

    fn shift_arrow(code: KeyCode) -> Event {
        Event::Key(crossterm::event::KeyEvent::new(code, KeyModifiers::SHIFT))
    }

    #[test]
    fn shift_arrows_recover_tiles_clipped_by_the_viewport() {
        for (mode, columns) in [
            (GridMode::Octants, 2),
            (GridMode::Sixteenths, 4),
            (GridMode::Eighths, 8),
        ] {
            let mut app = App {
                mode,
                ..App::default()
            };
            app.toggle_cell(0, 0);
            app.toggle_cell(1, columns - 1);
            let initial = 1 | (1 << (2 * columns - 1));
            let moved_down = initial << columns;
            for (arrow, expected) in [
                (KeyCode::Down, moved_down),
                (KeyCode::Right, 1 << (columns + 1)),
                (KeyCode::Left, moved_down),
                (KeyCode::Up, initial),
            ] {
                app.handle_event(shift_arrow(arrow));
                assert_eq!(active_mask(&app), expected, "{mode:?}, {arrow:?}");
            }
        }
    }

    #[test]
    fn viewport_starts_centered_and_stops_at_each_canvas_edge() {
        let mut canvas = SelectionCanvas::<2, 4>::default();
        assert_eq!(canvas.cells.len(), 32 * 64);
        assert_eq!((canvas.top, canvas.left), (30, 15));
        canvas.replace_visible(0b1001);

        canvas.shift(100, 100);
        assert_eq!((canvas.top, canvas.left), (0, 0));
        assert_eq!(canvas.mask(), 0);
        canvas.shift(-100, -100);
        assert_eq!((canvas.top, canvas.left), (60, 30));
        assert_eq!(canvas.mask(), 0);
        canvas.shift(30, 15);
        assert_eq!(canvas.mask(), 0b1001);
    }

    #[test]
    fn mouse_strokes_paint_and_erase_without_repeatedly_toggling_cells() {
        for mode in [GridMode::Octants, GridMode::Sixteenths, GridMode::Eighths] {
            let mut app = App {
                mode,
                grid_area: Some(Rect::new(5, 3, mode.grid_width(), mode.grid_height())),
                ..App::new(false)
            };
            let columns = usize::from((mode.grid_width() - 1) / 3);
            let rows = usize::from((mode.grid_height() - 1) / 2);
            // Painting through an already filled cell must leave it filled.
            app.toggle_cell(0, columns - 1);
            mouse_on_cell(&mut app, MouseEventKind::Down(MouseButton::Left), 0, 0);
            for column in [0, columns - 1, 0, columns - 1] {
                mouse_on_cell(&mut app, MouseEventKind::Drag(MouseButton::Left), 0, column);
            }
            mouse_on_cell(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                0,
                columns - 1,
            );
            assert_eq!(active_mask(&app), (1 << columns) - 1);
            assert!(app.draw_stroke.is_none());

            mouse_on_cell(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                0,
                columns - 1,
            );
            mouse_on_cell(&mut app, MouseEventKind::Drag(MouseButton::Left), 0, 0);
            mouse_on_cell(
                &mut app,
                MouseEventKind::Drag(MouseButton::Left),
                0,
                columns - 1,
            );
            mouse_on_cell(&mut app, MouseEventKind::Up(MouseButton::Left), 0, 0);
            assert_eq!(active_mask(&app), 0);

            // A normal click still toggles once; dragging after release does nothing.
            mouse_on_cell(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                rows - 1,
                columns - 1,
            );
            mouse_on_cell(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                rows - 1,
                columns - 1,
            );
            mouse_on_cell(&mut app, MouseEventKind::Drag(MouseButton::Left), 0, 0);
            assert_eq!(active_mask(&app), 1 << (rows * columns - 1));
        }
    }

    #[test]
    fn wheeling_repeats_the_clicked_paint_or_erase_operation_from_each_new_cell() {
        for mode in [GridMode::Octants, GridMode::Sixteenths, GridMode::Eighths] {
            let mut app = App {
                mode,
                grid_area: Some(Rect::new(10, 5, mode.grid_width(), mode.grid_height())),
                ..App::new(false)
            };
            let columns = usize::from((mode.grid_width() - 1) / 3);
            mouse_on_cell(&mut app, MouseEventKind::Down(MouseButton::Left), 0, 0);
            mouse_on_cell(&mut app, MouseEventKind::Up(MouseButton::Left), 0, 0);
            // Wheel coordinates and pointer motion do not replace the logical origin.
            app.handle_event(mouse_event(MouseEventKind::Moved, 70, 25));
            for kind in [
                MouseEventKind::ScrollUp,
                MouseEventKind::ScrollLeft,
                MouseEventKind::ScrollDown,
                MouseEventKind::ScrollRight,
            ] {
                app.handle_event(mouse_event(kind, 70, 25));
            }
            let square = 3 | (3 << columns);
            assert_eq!(active_mask(&app), square);
            let cursor = app.wheel_cursor.unwrap();
            assert_eq!((cursor.row, cursor.column), (0, 0));
            app.handle_event(mouse_event(MouseEventKind::ScrollUp, 70, 25));
            app.handle_event(mouse_event(MouseEventKind::ScrollUp, 70, 25));
            assert_eq!(active_mask(&app), square | (1 << (columns * 2)));

            // Clicking an existing cell changes the operation to erasing.
            mouse_on_cell(&mut app, MouseEventKind::Down(MouseButton::Left), 0, 0);
            mouse_on_cell(&mut app, MouseEventKind::Up(MouseButton::Left), 0, 0);
            for kind in [
                MouseEventKind::ScrollLeft,
                MouseEventKind::ScrollUp,
                MouseEventKind::ScrollRight,
                MouseEventKind::ScrollUp,
            ] {
                app.handle_event(mouse_event(kind, 70, 25));
            }
            assert_eq!(active_mask(&app), 0);
            assert!(!app.wheel_cursor.unwrap().filled);
        }
    }

    #[test]
    fn wheel_movement_stops_at_grid_edges_and_needs_a_cell_click() {
        for mode in [GridMode::Octants, GridMode::Sixteenths, GridMode::Eighths] {
            let mut app = App {
                mode,
                grid_area: Some(Rect::new(10, 5, mode.grid_width(), mode.grid_height())),
                select_none_area: Some(Rect::new(0, 0, 4, 1)),
                ..App::new(false)
            };
            let rows = usize::from((mode.grid_height() - 1) / 2);
            let columns = usize::from((mode.grid_width() - 1) / 3);
            app.handle_event(mouse_event(MouseEventKind::ScrollUp, 0, 0));
            assert_eq!(active_mask(&app), 0);
            mouse_on_cell(&mut app, MouseEventKind::Down(MouseButton::Left), 0, 0);
            mouse_on_cell(&mut app, MouseEventKind::Up(MouseButton::Left), 0, 0);
            for kind in [MouseEventKind::ScrollDown, MouseEventKind::ScrollRight] {
                app.handle_event(mouse_event(kind, 0, 0));
            }
            assert_eq!(active_mask(&app), 1);
            for _ in 0..rows + 2 {
                app.handle_event(mouse_event(MouseEventKind::ScrollUp, 0, 0));
            }
            for _ in 0..columns + 2 {
                app.handle_event(mouse_event(MouseEventKind::ScrollLeft, 0, 0));
            }
            let cursor = app.wheel_cursor.unwrap();
            assert_eq!((cursor.row, cursor.column), (rows - 1, columns - 1));
            app.handle_event(mouse_event(MouseEventKind::ScrollDown, 0, 0));
            let cursor = app.wheel_cursor.unwrap();
            assert_eq!((cursor.row, cursor.column), (rows - 2, columns - 1));

            // A footer click clears the grid and replaces the previous cell origin.
            app.handle_event(mouse_event(MouseEventKind::Down(MouseButton::Left), 0, 0));
            assert!(app.wheel_cursor.is_none());
            app.handle_event(mouse_event(MouseEventKind::ScrollDown, 0, 0));
            assert_eq!(active_mask(&app), 0);
        }
    }

    #[test]
    fn dragging_keeps_the_wheel_origin_at_the_last_clicked_cell() {
        let mut app = App {
            mode: GridMode::Eighths,
            grid_area: Some(Rect::new(0, 0, 25, 17)),
            ..App::new(false)
        };
        mouse_on_cell(&mut app, MouseEventKind::Down(MouseButton::Left), 0, 0);
        mouse_on_cell(&mut app, MouseEventKind::Drag(MouseButton::Left), 0, 7);
        mouse_on_cell(&mut app, MouseEventKind::Up(MouseButton::Left), 0, 7);
        app.handle_event(mouse_event(MouseEventKind::ScrollUp, 24, 16));
        assert_eq!(active_mask(&app), 0x01FF);
        mouse_on_cell(&mut app, MouseEventKind::Down(MouseButton::Left), 7, 7);
        mouse_on_cell(&mut app, MouseEventKind::Up(MouseButton::Left), 7, 7);
        app.handle_event(mouse_event(MouseEventKind::ScrollDown, 0, 0));
        assert_eq!(active_mask(&app), 0x8080_0000_0000_01FF);
    }

    #[test]
    fn wheel_origin_survives_redraw_and_copy_but_only_affects_its_grid() {
        let backend = ratatui::backend::TestBackend::new(80, 28);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut app = App {
            mode: GridMode::Eighths,
            ..App::new(false)
        };
        terminal.draw(|frame| app.render(frame)).unwrap();
        mouse_on_cell(&mut app, MouseEventKind::Down(MouseButton::Left), 1, 3);
        mouse_on_cell(&mut app, MouseEventKind::Up(MouseButton::Left), 1, 3);
        app.handle_event(Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('k'),
            KeyModifiers::NONE,
        )));
        terminal.backend_mut().resize(47, 21);
        terminal.draw(|frame| app.render(frame)).unwrap();
        app.handle_event(mouse_event(MouseEventKind::ScrollUp, 0, 0));
        assert_eq!(active_mask(&app), (1 << 11) | (1 << 19));
        for expected_mode in [GridMode::Octants, GridMode::Sixteenths] {
            app.handle_event(Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Tab,
                KeyModifiers::NONE,
            )));
            assert_eq!(app.mode, expected_mode);
            app.handle_event(mouse_event(MouseEventKind::ScrollLeft, 0, 0));
            assert_eq!(active_mask(&app), 0);
        }
        app.handle_event(Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Tab,
            KeyModifiers::NONE,
        )));
        app.handle_event(mouse_event(MouseEventKind::ScrollLeft, 0, 0));
        assert_eq!(active_mask(&app), (1 << 11) | (1 << 19) | (1 << 20));
    }

    #[test]
    fn sparse_strokes_join_vertical_and_diagonal_cells_and_include_the_release_position() {
        let mut app = App {
            mode: GridMode::Eighths,
            grid_area: Some(Rect::new(0, 0, 25, 17)),
            ..App::new(false)
        };
        mouse_on_cell(&mut app, MouseEventKind::Down(MouseButton::Left), 0, 4);
        mouse_on_cell(&mut app, MouseEventKind::Up(MouseButton::Left), 7, 4);
        assert_eq!(active_mask(&app), 0x1010_1010_1010_1010);
        assert_eq!(eighths::glyph(active_mask(&app)), Some('\u{1FB73}'));

        app.set_all(false);
        mouse_on_cell(&mut app, MouseEventKind::Down(MouseButton::Left), 0, 0);
        mouse_on_cell(&mut app, MouseEventKind::Drag(MouseButton::Left), 7, 7);
        mouse_on_cell(&mut app, MouseEventKind::Up(MouseButton::Left), 7, 7);
        assert_eq!(active_mask(&app), 0x8040_2010_0804_0201);
    }

    #[test]
    fn dragging_over_separators_and_outside_the_grid_does_not_activate_other_controls() {
        let grid = Rect::new(10, 10, 25, 17);
        let mut app = App {
            mode: GridMode::Eighths,
            grid_area: Some(grid),
            select_none_area: Some(Rect::new(0, 0, 4, 1)),
            ..App::new(false)
        };
        mouse_on_cell(&mut app, MouseEventKind::Down(MouseButton::Left), 0, 0);
        app.handle_event(mouse_event(
            MouseEventKind::Drag(MouseButton::Left),
            grid.x + 3,
            grid.y + 1,
        ));
        mouse_on_cell(&mut app, MouseEventKind::Drag(MouseButton::Left), 0, 2);
        assert_eq!(active_mask(&app), 7);
        app.handle_event(mouse_event(MouseEventKind::Drag(MouseButton::Left), 0, 0));
        assert_eq!(active_mask(&app), 7);
        mouse_on_cell(&mut app, MouseEventKind::Drag(MouseButton::Left), 7, 7);
        assert_eq!(active_mask(&app), (1 << 63) | 7);
        app.handle_event(mouse_event(MouseEventKind::Up(MouseButton::Left), 0, 0));
        mouse_on_cell(&mut app, MouseEventKind::Drag(MouseButton::Left), 6, 6);
        assert_eq!(active_mask(&app), (1 << 63) | 7);

        // A footer press performs its action, but cannot start a drawing stroke.
        app.handle_event(mouse_event(MouseEventKind::Down(MouseButton::Left), 0, 0));
        mouse_on_cell(&mut app, MouseEventKind::Drag(MouseButton::Left), 1, 1);
        assert_eq!(active_mask(&app), 0);
        mouse_on_cell(&mut app, MouseEventKind::Down(MouseButton::Right), 0, 0);
        mouse_on_cell(&mut app, MouseEventKind::Drag(MouseButton::Right), 7, 7);
        assert_eq!(active_mask(&app), 0);
    }

    #[test]
    fn non_mouse_input_and_layout_changes_cancel_drawing() {
        for event in [
            Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Tab,
                KeyModifiers::NONE,
            )),
            Event::Resize(47, 21),
            Event::Paste("\u{1FB73}".to_owned()),
            Event::FocusLost,
        ] {
            let mut app = App {
                mode: GridMode::Eighths,
                grid_area: Some(Rect::new(0, 0, 25, 17)),
                ..App::new(false)
            };
            mouse_on_cell(&mut app, MouseEventKind::Down(MouseButton::Left), 0, 0);
            assert!(app.draw_stroke.is_some());
            app.handle_event(event);
            assert!(app.draw_stroke.is_none());
            let mask = active_mask(&app);
            mouse_on_cell(&mut app, MouseEventKind::Drag(MouseButton::Left), 0, 1);
            assert_eq!(active_mask(&app), mask);
        }

        let backend = ratatui::backend::TestBackend::new(80, 28);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut app = App::new(false);
        terminal.draw(|frame| app.render(frame)).unwrap();
        mouse_on_cell(&mut app, MouseEventKind::Down(MouseButton::Left), 0, 0);
        assert!(app.draw_stroke.is_some());
        terminal.backend_mut().resize(47, 21);
        terminal.draw(|frame| app.render(frame)).unwrap();
        assert!(app.draw_stroke.is_none());
    }

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
        *app.octants.cell_mut(0, 0) = true;
        *app.octants.cell_mut(1, 1) = true;
        *app.octants.cell_mut(3, 1) = true;

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
            ('\u{1CC21}', 0x01),
            ('\u{1CC28}', 0x08),
            ('\u{1CC2F}', 0x0F),
            ('\u{1CE51}', 0x01),
            ('\u{1CE65}', 0x15),
            ('\u{1CE7A}', 0x2A),
            ('\u{1CE8F}', 0x3F),
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
            .chain((u8::MIN..=u8::MAX).map(octant_glyph))
            .chain('\u{1CC21}'..='\u{1CC2F}')
            .chain('\u{1CE51}'..='\u{1CE8F}');
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
        *app.octants.cell_mut(3, 1) = true;

        for text in [
            "",
            "hello",
            "n",
            "q",
            "c",
            "\u{2801}\u{2802}",
            " \u{2801}",
            "\u{001B}",
            "\u{1CC20}",
            "\u{1CC30}",
            "\u{1CE50}",
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
                    name: "Quadrant sep",
                    hotkey: 'Q',
                    glyph: '\u{1CC21}',
                },
                CharacterOutput {
                    name: "Sextant",
                    hotkey: 's',
                    glyph: '\u{1FB00}',
                },
                CharacterOutput {
                    name: "Sextant sep",
                    hotkey: 'S',
                    glyph: '\u{1CE51}',
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
            ["Braille", "Sextant", "Sextant sep", "Octant"]
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
            (0x00, "\u{2800}    \u{00A0}"),
            (0x01, "⠁▘𜰡🬀𜹑𜺨"),
            (0x03, "⠉▀𜰣🬂𜹓🮂"),
            (0x07, "⠋▛𜰧🬆𜹗𜴂"),
            (0x0F, "⠛█𜰯🬎𜹟▀"),
            (0x1F, "⠟🬝𜹯𜴗"),
            (0x3F, "⠿█𜺏🮅"),
            (0x7F, "⡿𜵰"),
            (0xFF, "⣿█"),
            (0xDF, "⣟𜷊"),
            (0x66, "⡪𜵗"),
            (0x90, "⢄𜶀"),
            (0x20, "⠠🬞𜹰𜴘"),
            (0x08, "⠐▗𜰨🬇𜹘𜴃"),
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
    fn copy_hotkeys_select_ordinary_and_separated_outputs() {
        for (scaling, quadrant, sextant, separated_quadrant, separated_sextant) in [
            (false, '▗', '\u{1FB07}', '\u{1CC28}', '\u{1CE58}'),
            (true, '▐', '▐', '\u{1CC2A}', '\u{1CE7A}'),
        ] {
            let mut app = App::new(scaling);
            *app.octants.cell_mut(1, 1) = true;
            for (hotkey, expected) in [
                ('b', '\u{2810}'),
                ('q', quadrant),
                ('Q', separated_quadrant),
                ('s', sextant),
                ('S', separated_sextant),
                ('c', '\u{1CD03}'),
            ] {
                let modifiers = if hotkey.is_ascii_uppercase() {
                    vec![KeyModifiers::NONE, KeyModifiers::SHIFT]
                } else {
                    vec![KeyModifiers::NONE]
                };
                for modifiers in modifiers {
                    assert_eq!(
                        app.handle_event(Event::Key(crossterm::event::KeyEvent::new(
                            KeyCode::Char(hotkey),
                            modifiers,
                        ))),
                        Some(expected),
                        "scaling {scaling}, hotkey {hotkey}, modifiers {modifiers:?}",
                    );
                    assert_eq!(app.mask(), 0x08);
                    assert!(!app.should_quit);
                }
            }
        }

        let mut app = App::new(false);
        *app.octants.cell_mut(3, 0) = true;
        for hotkey in ['q', 'Q', 's', 'S'] {
            assert_eq!(
                app.handle_event(Event::Key(crossterm::event::KeyEvent::new(
                    KeyCode::Char(hotkey),
                    KeyModifiers::NONE,
                ))),
                None,
            );
            assert_eq!(app.mask(), 0x40);
        }
    }

    #[test]
    fn tab_preserves_each_grids_selection_and_sixteenths_copy_exactly() {
        for scaling in [false, true] {
            let mut app = App::new(scaling);
            *app.octants.cell_mut(1, 1) = true;
            let tab = Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Tab,
                KeyModifiers::NONE,
            ));
            assert_eq!(app.handle_event(tab.clone()), None);
            assert_eq!(app.mode, GridMode::Sixteenths);
            app.toggle_cell(3, 0);
            let copy = Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char('t'),
                KeyModifiers::NONE,
            ));
            assert_eq!(app.handle_event(copy.clone()), Some('\u{1CE9C}'));
            app.toggle_cell(3, 1);
            assert_eq!(app.handle_event(copy.clone()), Some('\u{1CEA3}'));

            app.handle_event(tab.clone());
            assert_eq!(app.mode, GridMode::Eighths);
            app.toggle_cell(7, 7);
            app.handle_event(tab.clone());
            assert_eq!(app.mode, GridMode::Octants);
            assert_eq!(app.mask(), 0x08);
            app.handle_event(tab);
            assert_eq!(app.mode, GridMode::Sixteenths);
            assert_eq!(app.sixteenths.mask(), 0x3000);
            assert_eq!(app.eighths.mask(), 1 << 63);

            app.set_all(true);
            assert_eq!(app.handle_event(copy.clone()), Some('█'));
            app.set_all(false);
            assert_eq!(app.handle_event(copy.clone()), Some('\u{00A0}'));
            app.toggle_cell(0, 0);
            app.toggle_cell(3, 0);
            assert_eq!(app.handle_event(copy), None);
            assert!(
                sixteenth_outputs_text(app.sixteenths.mask() as u16, Style::default(), true)
                    .to_string()
                    .contains("No single Unicode glyph")
            );
            assert_eq!(app.mask(), 0x08);
            assert!(!app.should_quit);
        }
    }

    #[test]
    fn four_by_four_pairs_show_matching_categories_at_their_original_rows() {
        for (mask, braille, quadrant, sextant, separated_sextant) in [
            (0x0003, '⠁', Some('▘'), '\u{1FB00}', '\u{1CE51}'),
            (0x0030, '⠂', Some('▖'), '\u{1FB03}', '\u{1CE54}'),
            (0x00F0, '⠒', Some('▄'), '\u{1FB0B}', '\u{1CE5C}'),
            (0x0300, '⠄', None, '\u{1FB0F}', '\u{1CE60}'),
        ] {
            let outputs = sixteenth_outputs(mask);
            let glyph = |hotkey| {
                outputs
                    .iter()
                    .find(|output| output.hotkey == hotkey)
                    .map(|output| output.glyph)
            };
            assert_eq!(glyph('b'), Some(braille));
            assert_eq!(glyph('q'), quadrant);
            assert_eq!(glyph('s'), Some(sextant), "mask {mask:#06x}");
            assert_eq!(glyph('S'), Some(separated_sextant));
            assert_eq!(glyph('t'), sixteenths::glyph(mask));
            assert!(glyph('c').is_some());
        }

        for (mask, hotkeys) in [
            (0x0003, "bqQsSct"),
            (0x0300, "bsSct"),
            (0x3000, "bct"),
            (0x1000, "t"),
            (0x1001, ""),
        ] {
            assert_eq!(
                sixteenth_outputs(mask)
                    .into_iter()
                    .map(|output| output.hotkey)
                    .collect::<String>(),
                hotkeys,
                "mask {mask:#06x}",
            );
        }
    }

    #[test]
    fn four_by_four_copy_hotkeys_copy_the_visible_matching_categories() {
        for scaling in [false, true] {
            let mut app = App {
                mode: GridMode::Sixteenths,
                ..App::new(scaling)
            };
            app.octants.reset(true);
            app.sixteenths.replace_visible(0x00F0);
            for (hotkey, expected) in [
                ('b', '⠒'),
                ('q', '▄'),
                ('Q', '\u{1CC2C}'),
                ('s', '\u{1FB0B}'),
                ('S', '\u{1CE5C}'),
                ('c', '\u{1CD06}'),
                ('t', '\u{1CD06}'),
            ] {
                assert_eq!(
                    app.handle_event(Event::Key(crossterm::event::KeyEvent::new(
                        KeyCode::Char(hotkey),
                        if hotkey.is_ascii_uppercase() {
                            KeyModifiers::SHIFT
                        } else {
                            KeyModifiers::NONE
                        },
                    ))),
                    Some(expected),
                    "hotkey {hotkey}, scaling {scaling}",
                );
            }

            app.sixteenths.replace_visible(0x1000);
            for hotkey in ['b', 'q', 'Q', 's', 'S', 'c'] {
                assert_eq!(
                    app.handle_event(Event::Key(crossterm::event::KeyEvent::new(
                        KeyCode::Char(hotkey),
                        KeyModifiers::NONE,
                    ))),
                    None,
                );
            }
            assert_eq!(app.sixteenths.mask(), 0x1000);
            assert_eq!(app.mask(), 0xFF);
        }
    }

    #[test]
    fn sixteenth_paste_selects_the_grid_and_shared_glyphs_keep_the_active_grid() {
        let mut app = App::new(false);
        for event in [
            Event::Paste("\u{1CE9C}".to_owned()),
            Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char('\u{1CE9C}'),
                KeyModifiers::NONE,
            )),
        ] {
            app.mode = GridMode::Octants;
            app.handle_event(event);
            assert_eq!(app.mode, GridMode::Sixteenths);
            assert_eq!(app.sixteenths.mask(), 0x1000);
        }

        app.handle_event(Event::Paste("\u{1CEA3}".to_owned()));
        assert_eq!(app.mode, GridMode::Sixteenths);
        assert_eq!(app.sixteenths.mask(), 0x3000);
        app.handle_event(Event::Paste("▗".to_owned()));
        assert_eq!(app.mode, GridMode::Sixteenths);
        assert_eq!(app.sixteenths.mask(), 0xCC00);
        app.handle_event(Event::Paste("\u{2810}".to_owned()));
        assert_eq!(app.mode, GridMode::Octants);
        assert_eq!(app.mask(), 0x08);
        app.handle_event(Event::Paste("\u{1CEA3}".to_owned()));
        assert_eq!(app.mode, GridMode::Octants);
        assert_eq!(app.mask(), 0x40);
        assert_eq!(app.sixteenths.mask(), 0xCC00);
    }

    #[test]
    fn four_column_grid_hit_testing_and_rendering_include_every_cell() {
        let grid = Rect::new(10, 20, 13, GRID_HEIGHT);
        for row in 0..4 {
            for column in 0..4 {
                for offset in [1, 2] {
                    assert_eq!(
                        grid_cell_at(grid, 10 + column * 3 + offset, 21 + row * 2),
                        Some((row as usize, column as usize)),
                    );
                }
            }
        }
        for y in grid.y..grid.bottom() {
            for x in grid.x..grid.right() {
                if (x - grid.x).is_multiple_of(3) || (y - grid.y).is_multiple_of(2) {
                    assert_eq!(grid_cell_at(grid, x, y), None);
                }
            }
        }

        let cells = cells_from_mask::<4, 4>(0x9000);
        let mut buffer = Buffer::empty(grid);
        CellGrid::new(&cells).render(grid, &mut buffer);
        for x in [11, 12, 20, 21] {
            assert_eq!(buffer[(x, 27)].symbol(), "█");
        }
        assert_eq!(buffer[(19, 27)].symbol(), "│");
        assert_eq!(buffer[(22, 27)].symbol(), "│");
    }

    #[test]
    fn eight_by_eight_copy_matches_coarser_categories_and_ignores_partial_groups() {
        for scaling in [false, true] {
            let compact = 0x00F0;
            let expanded = eighths::expand_sixteenths(compact);
            let mut outputs = sixteenth_outputs(compact);
            outputs.push(CharacterOutput {
                name: "Block",
                hotkey: 'k',
                glyph: '\u{1CD06}',
            });
            assert_eq!(eighth_outputs(expanded), outputs);
            let mut app = App {
                mode: GridMode::Eighths,
                ..App::new(scaling)
            };
            app.eighths.replace_visible(expanded);
            let key = |hotkey| {
                Event::Key(crossterm::event::KeyEvent::new(
                    KeyCode::Char(hotkey),
                    if hotkey.is_ascii_uppercase() {
                        KeyModifiers::SHIFT
                    } else {
                        KeyModifiers::NONE
                    },
                ))
            };
            for output in outputs {
                assert_eq!(app.handle_event(key(output.hotkey)), Some(output.glyph));
            }
            app.eighths.replace_visible(0x1010_1010_1010_1010);
            assert_eq!(app.handle_event(key('k')), Some('\u{1FB73}'));
            for hotkey in ['b', 'q', 'Q', 's', 'S', 'c', 't'] {
                assert_eq!(app.handle_event(key(hotkey)), None);
            }
            app.toggle_cell(0, 4);
            assert_eq!(app.handle_event(key('k')), None);
            assert!(
                eighth_outputs_text(app.eighths.mask(), Style::default(), true)
                    .to_string()
                    .contains("No single Unicode glyph")
            );
            app.handle_event(key('a'));
            assert_eq!(app.eighths.mask(), u64::MAX);
            assert_eq!(app.handle_event(key('k')), Some('█'));
            app.handle_event(key('n'));
            assert_eq!(app.eighths.mask(), 0);
            assert_eq!(app.handle_event(key('k')), Some('\u{00A0}'));
        }
    }

    #[test]
    fn eighth_paste_loads_native_geometry_and_shared_glyphs_stay_on_the_grid() {
        let mut app = App::new(false);
        for (glyph, expected) in [
            ('\u{1FB70}', 0x0202_0202_0202_0202),
            ('\u{1FB76}', 0x0000_0000_0000_FF00),
            ('\u{1FB83}', 0x0000_0000_00FF_FFFF),
            ('\u{1FB7C}', 0xFF01_0101_0101_0101),
        ] {
            for event in [
                Event::Paste(glyph.to_string()),
                Event::Key(crossterm::event::KeyEvent::new(
                    KeyCode::Char(glyph),
                    KeyModifiers::NONE,
                )),
            ] {
                app.mode = GridMode::Octants;
                app.handle_event(event);
                assert_eq!(app.mode, GridMode::Eighths);
                assert_eq!(app.eighths.mask(), expected);
            }
        }
        for glyph in ['\u{1CE9C}', '▗', '█', ' ', '\u{00A0}'] {
            app.handle_event(Event::Paste(glyph.to_string()));
            assert_eq!(app.mode, GridMode::Eighths);
            assert_eq!(app.eighths.mask(), eighths::mask(glyph).unwrap());
        }
        app.handle_event(Event::Paste("\u{2810}".to_owned()));
        assert_eq!(app.mode, GridMode::Octants);
        assert_eq!(app.mask(), 0x08);
    }

    #[test]
    fn eight_by_eight_hit_testing_and_rendering_include_all_sixty_four_cells() {
        let grid = Rect::new(3, 5, 25, 17);
        let mut app = App {
            mode: GridMode::Eighths,
            grid_area: Some(grid),
            ..App::new(false)
        };
        for row in 0..8 {
            for column in 0..8 {
                let x = grid.x + column * 3 + 1;
                let y = grid.y + row * 2 + 1;
                assert_eq!(
                    grid_cell_at(grid, x, y),
                    Some((usize::from(row), usize::from(column)))
                );
                app.handle_mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: x,
                    row: y,
                    modifiers: KeyModifiers::NONE,
                });
            }
        }
        assert_eq!(app.eighths.mask(), u64::MAX);
        let mut buffer = Buffer::empty(grid);
        CellGrid::new(&app.eighths.visible()).render(grid, &mut buffer);
        for row in 0..8 {
            for column in 0..8 {
                for offset in [1, 2] {
                    assert_eq!(
                        buffer[(grid.x + column * 3 + offset, grid.y + row * 2 + 1)].symbol(),
                        "█"
                    );
                }
            }
        }
        for y in grid.y..grid.bottom() {
            for x in grid.x..grid.right() {
                if (x - grid.x).is_multiple_of(3) || (y - grid.y).is_multiple_of(2) {
                    assert_eq!(grid_cell_at(grid, x, y), None);
                }
            }
        }
        assert_eq!(buffer[(grid.right() - 1, grid.bottom() - 1)].symbol(), "┘");
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
            app.handle_event(shift_arrow(KeyCode::Down));
            *app.cell_mut(0, 1) = true;
            app.toggle_cell(2, 1);
            assert_eq!(
                app.handle_event(Event::Key(crossterm::event::KeyEvent::new(
                    KeyCode::Char(hotkey),
                    KeyModifiers::NONE,
                ))),
                None,
            );
            let expected = if hotkey == 'a' { u8::MAX } else { 0 };
            assert_eq!(app.mask(), expected);
            assert_eq!((app.octants.top, app.octants.left), (30, 15));
            app.handle_event(shift_arrow(KeyCode::Down));
            assert_eq!(app.mask(), expected << 2);
            app.handle_event(shift_arrow(KeyCode::Up));
        }
    }

    #[test]
    fn footer_clicks_select_all_and_none() {
        let layout = AppLayout::new(Rect::new(0, 0, 80, 24), GridMode::Octants).unwrap();
        let mut app = App {
            select_all_area: Some(layout.select_all),
            select_none_area: Some(layout.select_none),
            ..App::default()
        };
        for (area, filled) in [(layout.select_all, true), (layout.select_none, false)] {
            app.handle_event(shift_arrow(KeyCode::Right));
            *app.cell_mut(0, 0) = true;
            app.toggle_cell(2, 1);
            app.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: area.x,
                row: area.y,
                modifiers: KeyModifiers::NONE,
            });
            let expected = if filled { u8::MAX } else { 0 };
            assert_eq!(app.mask(), expected);
            assert_eq!((app.octants.top, app.octants.left), (30, 15));
            app.handle_event(shift_arrow(KeyCode::Right));
            assert_eq!(app.mask(), expected & 0b1010_1010);
            app.handle_event(shift_arrow(KeyCode::Left));
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
        let layout = AppLayout::new(Rect::new(0, 0, 59, 23), GridMode::Octants).unwrap();

        assert_eq!(layout.grid, Rect::new(10, 5, GRID_WIDTH, GRID_HEIGHT));
        assert_eq!(layout.select_all, Rect::new(32, 17, 3, 1));
        assert_eq!(layout.select_none, Rect::new(36, 17, 4, 1));
        assert_eq!(layout.outputs, Rect::new(20, 3, 28, 13));
        assert_eq!(layout.help, Rect::new(0, 17, 59, 2));
        assert_eq!(layout.footer, Rect::new(0, 19, 59, 1));
        for (mode, width, height, hint_rows) in [
            (GridMode::Octants, 29, 18, 3),
            (GridMode::Sixteenths, 35, 20, 3),
            (GridMode::Eighths, 47, 21, 2),
        ] {
            assert!(AppLayout::new(Rect::new(0, 0, width - 1, height), mode).is_err());
            assert!(AppLayout::new(Rect::new(0, 0, width, height - 1), mode).is_err());
            let layout = AppLayout::new(Rect::new(0, 0, width, height), mode).unwrap();
            assert!(!layout.show_codepoints);
            assert_eq!(layout.help.height, hint_rows);
            assert_eq!(layout.footer.bottom(), height);
        }
        let layout = AppLayout::new(Rect::new(0, 0, 59, 23), GridMode::Sixteenths).unwrap();
        assert_eq!(layout.grid, Rect::new(7, 5, 13, 9));
        assert_eq!(layout.outputs, Rect::new(23, 2, 28, 15));
        assert_eq!(layout.help, Rect::new(0, 18, 59, 2));
        assert_eq!(layout.footer, Rect::new(0, 20, 59, 1));
    }

    #[test]
    fn hints_wrap_only_between_whole_segments_and_keep_hotkey_colors() {
        for mode in [GridMode::Octants, GridMode::Sixteenths, GridMode::Eighths] {
            let segments = help_segments(mode);
            let expected: Vec<_> = segments.iter().map(ToString::to_string).collect();
            let combined = expected.join(" • ");
            let full_width = Line::from(combined.as_str()).width() as u16;
            assert_eq!(key_hints(mode, full_width).text.to_string(), combined);
            assert_eq!(key_hints(mode, full_width).text.height(), 1);
            assert!(key_hints(mode, full_width - 1).text.height() > 1);
            for width in mode.minimum_width()..=full_width {
                let hints = key_hints(mode, width);
                assert!(hints.text.width() <= usize::from(width));
                let actual: Vec<_> = hints
                    .text
                    .lines
                    .iter()
                    .flat_map(|line| {
                        let line = line.to_string();
                        assert!(!line.starts_with('•') && !line.ends_with('•'));
                        line.split(" • ").map(str::to_owned).collect::<Vec<_>>()
                    })
                    .collect();
                assert_eq!(actual, expected);
                let highlighted: String = hints
                    .text
                    .lines
                    .iter()
                    .flat_map(|line| &line.spans)
                    .filter(|span| span.style.fg == Some(HOTKEY_COLOR))
                    .map(|span| span.content.as_ref())
                    .collect();
                assert_eq!(
                    highlighted,
                    format!("{}angtab", mode.copy_hotkeys().replace('/', "")),
                );
            }
        }
    }

    #[test]
    fn codepoint_column_disappears_only_when_the_body_needs_less_width() {
        for mode in [GridMode::Octants, GridMode::Sixteenths, GridMode::Eighths] {
            let full_width = mode.grid_width() + GRID_OUTPUT_GAP + output_width(true);
            let full = AppLayout::new(Rect::new(0, 0, full_width, 30), mode).unwrap();
            assert!(full.show_codepoints);
            assert!(full.help.height > 1);
            let narrow = AppLayout::new(Rect::new(0, 0, full_width - 1, 30), mode).unwrap();
            assert!(!narrow.show_codepoints);
            assert_eq!(full.outputs.width - narrow.outputs.width, 9);
        }
        let glyph_style = Style::default().fg(GLYPH_COLOR).bg(Color::Indexed(8));
        for text in [
            exact_outputs_text(0x08, glyph_style, false),
            sixteenth_outputs_text(0x0003, glyph_style, false),
            eighth_outputs_text(0x1010_1010_1010_1010, glyph_style, false),
            scaled_outputs_text(&cells_from_mask(0x81), glyph_style, false),
        ] {
            assert!(!text.to_string().contains("U+"));
            assert!(text.width() <= usize::from(output_width(false) - 2));
            assert!(
                text.lines
                    .iter()
                    .any(|line| { line.spans.iter().any(|span| span.style == glyph_style) })
            );
        }
        let unsupported = sixteenth_outputs_text(0x1001, glyph_style, false);
        assert!(unsupported.width() <= usize::from(output_width(false) - 2));
    }

    #[test]
    fn wrapped_footer_hitboxes_track_the_rendered_all_and_none_words() {
        for mode in [GridMode::Octants, GridMode::Sixteenths, GridMode::Eighths] {
            for width in mode.minimum_width()..=80 {
                let backend = ratatui::backend::TestBackend::new(width, 30);
                let mut terminal = ratatui::Terminal::new(backend).unwrap();
                let mut app = App {
                    mode,
                    ..App::new(false)
                };
                terminal.draw(|frame| app.render(frame)).unwrap();
                let buffer = terminal.backend().buffer();
                for (area, word) in [
                    (app.select_all_area.unwrap(), "all"),
                    (app.select_none_area.unwrap(), "none"),
                ] {
                    let rendered: String = (area.x..area.right())
                        .map(|x| buffer[(x, area.y)].symbol())
                        .collect();
                    assert_eq!(rendered, word, "mode {mode:?}, width {width}");
                    app.handle_mouse(MouseEvent {
                        kind: MouseEventKind::Down(MouseButton::Left),
                        column: area.x,
                        row: area.y,
                        modifiers: KeyModifiers::NONE,
                    });
                    let selected = active_mask(&app);
                    let expected = match (word, mode) {
                        ("all", GridMode::Octants) => 0xFF,
                        ("all", GridMode::Sixteenths) => 0xFFFF,
                        ("all", GridMode::Eighths) => u64::MAX,
                        _ => 0,
                    };
                    assert_eq!(selected, expected);
                }
            }
        }
    }
}
