//! One terminal: a shell under a PTY (alacritty_terminal parses and keeps the
//! grid on its own thread) drawn cell by cell with the app's mono font.
//!
//! Keys with escape encodings go through `keys::encode`; text goes through the
//! input handler so IMEs and dead keys work. Mouse drags select (double click:
//! word, triple: line) unless the program asked for mouse reports.

use std::borrow::Cow;
use std::cell::Cell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::tty::{self, Options, Shell};
use alacritty_terminal::vte::ansi::CursorShape;
use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::{
    App, AppContext as _, Bounds, ClipboardItem, Context, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, Font, FontStyle, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Point, Render, ScrollWheelEvent,
    SharedString, StrikethroughStyle, Styled as _, TextAlign, TextRun, UTF16Selection,
    UnderlineStyle, Window, canvas, div, fill, outline, point, prelude::FluentBuilder as _, px,
    size,
};

use super::colors::{Palette, dim, hsla};
use super::keys::{self, KeyMode};
use crate::backend::TerminalSpec;
use crate::keymap::{TerminalClear, TerminalCopy, TerminalPaste, TerminalSelectAll};
use crate::theme::{MONO, Tokens};

pub const FONT_SIZE: Pixels = px(12.);
pub const LINE_HEIGHT: Pixels = px(16.);
/// Space between the panel's edge and the grid.
const PAD_X: Pixels = px(12.);
const PAD_Y: Pixels = px(6.);

/// alacritty wants a window id per terminal (it exports it to the shell).
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
struct Listener(mpsc::UnboundedSender<Event>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let _ = self.0.unbounded_send(event);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct GridSize {
    cols: usize,
    rows: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// Where the grid was last drawn: shared with the paint closures.
#[derive(Clone, Copy, Default)]
struct Geometry {
    origin: Point<Pixels>,
    cell: gpui::Size<Pixels>,
    cols: usize,
    rows: usize,
    cursor: Option<Bounds<Pixels>>,
}

pub enum TerminalEvent {
    /// The shell (or a program) set the title.
    TitleChanged,
    /// The process ended; `Some(0)` is a clean exit.
    Exited(Option<i32>),
    /// A key was pressed after the process ended: close this terminal.
    CloseRequested,
}

pub struct TerminalView {
    term: Arc<FairMutex<Term<Listener>>>,
    pty: EventLoopSender,
    focus: FocusHandle,
    default_title: String,
    title: Option<String>,
    exit: Option<Option<i32>>,
    /// IME composition in progress.
    marked: Option<String>,
    selecting: bool,
    /// Sub-line scroll distance not yet applied.
    scroll_rest: f32,
    geometry: Rc<Cell<Geometry>>,
    _pump: gpui::Task<()>,
}

impl EventEmitter<TerminalEvent> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Drop for TerminalView {
    fn drop(&mut self) {
        // The PTY thread drops the PTY, which hangs up on the shell.
        let _ = self.pty.send(Msg::Shutdown);
    }
}

impl TerminalView {
    /// Start `spec`'s program under a PTY.
    pub fn spawn(
        spec: &TerminalSpec,
        window: &mut Window,
        cx: &mut App,
    ) -> anyhow::Result<Entity<Self>> {
        let size = GridSize { cols: 80, rows: 24 };
        let options = Options {
            shell: Some(Shell::new(spec.program.clone(), spec.args.clone())),
            working_directory: Some(spec.cwd.clone()),
            drain_on_exit: true,
            env: spec.env.iter().cloned().collect::<HashMap<_, _>>(),
        };
        let pty = tty::new(
            &options,
            window_size(size, gpui::Size::default()),
            next_id(),
        )?;
        let (events_tx, mut events) = mpsc::unbounded();
        let listener = Listener(events_tx);
        let config = Config {
            scrolling_history: 10_000,
            ..Config::default()
        };
        let term = Arc::new(FairMutex::new(Term::new(config, &size, listener.clone())));
        let event_loop = EventLoop::new(term.clone(), listener, pty, true, false)?;
        let sender = event_loop.channel();
        event_loop.spawn();

        let view = cx.new(|cx| {
            let focus = cx.focus_handle();
            cx.on_focus(&focus, window, |this: &mut Self, _, cx| {
                this.report_focus(true, cx);
            })
            .detach();
            cx.on_blur(&focus, window, |this: &mut Self, _, cx| {
                this.report_focus(false, cx);
            })
            .detach();
            let pump = cx.spawn(async move |this, cx| {
                while let Some(first) = events.next().await {
                    let mut batch = vec![first];
                    while let Ok(more) = events.try_recv() {
                        batch.push(more);
                    }
                    if this
                        .update(cx, |this, cx| this.on_events(batch, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            });
            Self {
                term,
                pty: sender,
                focus,
                default_title: spec.title.clone(),
                title: None,
                exit: None,
                marked: None,
                selecting: false,
                scroll_rest: 0.,
                geometry: Rc::default(),
                _pump: pump,
            }
        });
        Ok(view)
    }

    pub fn title(&self) -> &str {
        self.title.as_deref().unwrap_or(&self.default_title)
    }

    pub fn exited(&self) -> Option<Option<i32>> {
        self.exit
    }

    /// Send bytes to the program, as typing does: back to the bottom, no selection.
    pub fn input(&mut self, bytes: impl Into<Cow<'static, [u8]>>, cx: &mut Context<'_, Self>) {
        {
            let mut term = self.term.lock();
            if term.grid().display_offset() != 0 {
                term.scroll_display(Scroll::Bottom);
            }
            term.selection = None;
        }
        let _ = self.pty.send(Msg::Input(bytes.into()));
        cx.notify();
    }

    fn on_events(&mut self, events: Vec<Event>, cx: &mut Context<'_, Self>) {
        for event in events {
            match event {
                Event::Title(title) => {
                    self.title = Some(title).filter(|t| !t.trim().is_empty());
                    cx.emit(TerminalEvent::TitleChanged);
                }
                Event::ResetTitle => {
                    self.title = None;
                    cx.emit(TerminalEvent::TitleChanged);
                }
                Event::PtyWrite(text) => {
                    let _ = self.pty.send(Msg::Input(text.into_bytes().into()));
                }
                Event::ClipboardStore(_, text) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
                Event::ColorRequest(index, format) => {
                    let t = Tokens::get(cx);
                    let rgb = Palette::new(&t, t.rail).by_index(index);
                    let _ = self.pty.send(Msg::Input(format(rgb).into_bytes().into()));
                }
                Event::TextAreaSizeRequest(format) => {
                    let g = self.geometry.get();
                    let size = GridSize {
                        cols: g.cols,
                        rows: g.rows,
                    };
                    let reply = format(window_size(size, g.cell));
                    let _ = self.pty.send(Msg::Input(reply.into_bytes().into()));
                }
                Event::ChildExit(status) => {
                    self.exit = Some(status.code());
                    cx.emit(TerminalEvent::Exited(status.code()));
                }
                // Reading the clipboard from an escape sequence is not allowed
                // (alacritty's default OSC 52 mode only copies).
                Event::ClipboardLoad(..)
                | Event::Wakeup
                | Event::Bell
                | Event::Exit
                | Event::MouseCursorDirty
                | Event::CursorBlinkingChange => {}
            }
        }
        cx.notify();
    }

    fn report_focus(&mut self, focused: bool, cx: &mut Context<'_, Self>) {
        if self.term.lock().mode().contains(TermMode::FOCUS_IN_OUT) {
            let seq: &'static [u8] = if focused { b"\x1b[I" } else { b"\x1b[O" };
            let _ = self.pty.send(Msg::Input(seq.into()));
        }
        cx.notify();
    }

    fn key_down(&mut self, e: &KeyDownEvent, _: &mut Window, cx: &mut Context<'_, Self>) {
        if self.exit.is_some() {
            if !e.keystroke.modifiers.platform {
                cx.emit(TerminalEvent::CloseRequested);
                cx.stop_propagation();
            }
            return;
        }
        // The IME owns keys while it composes.
        if self.marked.is_some() {
            return;
        }
        let m = &e.keystroke.modifiers;
        if m.shift && !m.platform && matches!(e.keystroke.key.as_str(), "pageup" | "pagedown") {
            let page = if e.keystroke.key == "pageup" {
                Scroll::PageUp
            } else {
                Scroll::PageDown
            };
            self.term.lock().scroll_display(page);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let mode = *self.term.lock().mode();
        let key_mode = KeyMode {
            app_cursor: mode.contains(TermMode::APP_CURSOR),
            newline: mode.contains(TermMode::LINE_FEED_NEW_LINE),
        };
        if let Some(bytes) = keys::encode(&e.keystroke, key_mode) {
            self.input(bytes, cx);
            cx.stop_propagation();
        }
    }

    fn copy(&mut self, _: &TerminalCopy, _: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(text) = self.term.lock().selection_to_string() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, _: &TerminalPaste, _: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) else {
            return;
        };
        let bracketed = self.term.lock().mode().contains(TermMode::BRACKETED_PASTE);
        self.input(paste_bytes(&text, bracketed), cx);
    }

    /// ⌘K, as in VS Code: drop the scrollback and keep the current line at
    /// the top. Full-screen programs keep their screen.
    pub fn clear(&mut self, _: &TerminalClear, _: &mut Window, cx: &mut Context<'_, Self>) {
        {
            let mut term = self.term.lock();
            if !term.mode().contains(TermMode::ALT_SCREEN) {
                let rows = term.screen_lines();
                let grid = term.grid_mut();
                let line = usize::try_from(grid.cursor.point.line.0).unwrap_or(0);
                if line > 0 {
                    grid.scroll_up(
                        &(Line(0)..Line(i32::try_from(rows).unwrap_or(i32::MAX))),
                        line,
                    );
                    grid.cursor.point.line = Line(0);
                }
            }
            term.grid_mut().clear_history();
            term.selection = None;
        }
        cx.notify();
    }

    fn select_all(&mut self, _: &TerminalSelectAll, _: &mut Window, cx: &mut Context<'_, Self>) {
        {
            let mut term = self.term.lock();
            let (top, bottom) = (term.topmost_line(), term.bottommost_line());
            let last = term.last_column();
            let mut selection = Selection::new(
                SelectionType::Simple,
                GridPoint::new(top, Column(0)),
                Side::Left,
            );
            selection.update(GridPoint::new(bottom, last), Side::Right);
            term.selection = Some(selection);
        }
        cx.notify();
    }

    /// The grid cell under a window position, and which half of it.
    fn cell_at(&self, position: Point<Pixels>, display_offset: usize) -> (GridPoint, Side) {
        let g = self.geometry.get();
        let rel = position - g.origin;
        let (w, h) = (
            f32::from(g.cell.width).max(1.),
            f32::from(g.cell.height).max(1.),
        );
        let x = (f32::from(rel.x) / w).max(0.);
        let y = (f32::from(rel.y) / h).max(0.);
        // Both are small, non-negative cell counts.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (col, row) = (
            (x as usize).min(g.cols.saturating_sub(1)),
            (y as usize).min(g.rows.saturating_sub(1)),
        );
        let side = if x.fract() < 0.5 {
            Side::Left
        } else {
            Side::Right
        };
        let line = i32::try_from(row).unwrap_or(0) - i32::try_from(display_offset).unwrap_or(0);
        (GridPoint::new(Line(line), Column(col)), side)
    }

    /// Mouse reporting wins over selection, unless shift is held.
    fn reports_mouse(&self, shift: bool) -> Option<TermMode> {
        let mode = *self.term.lock().mode();
        (mode.intersects(TermMode::MOUSE_MODE) && !shift).then_some(mode)
    }

    fn report(&self, mode: TermMode, button: u8, at: GridPoint, pressed: bool) {
        let col = at.column.0 + 1;
        let row = usize::try_from(at.line.0).unwrap_or(0) + 1;
        let bytes = if mode.contains(TermMode::SGR_MOUSE) {
            let end = if pressed { 'M' } else { 'm' };
            format!("\x1b[<{button};{col};{row}{end}").into_bytes()
        } else {
            // X10 encoding: one byte per value, so positions stop at 223.
            let b = if pressed { button } else { 3 };
            let byte = |v: usize| u8::try_from(32 + v.min(223)).unwrap_or(255);
            vec![0x1b, b'[', b'M', 32 + b, byte(col), byte(row)]
        };
        let _ = self.pty.send(Msg::Input(bytes.into()));
    }

    fn mouse_down(&mut self, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.focus.focus(window, cx);
        let offset = self.term.lock().grid().display_offset();
        let (at, side) = self.cell_at(e.position, offset);
        if let Some(mode) = self.reports_mouse(e.modifiers.shift) {
            let button = match e.button {
                MouseButton::Left => 0,
                MouseButton::Middle => 1,
                _ => 2,
            };
            self.report(mode, button + modifier_bits(&e.modifiers), at, true);
            return;
        }
        if e.button != MouseButton::Left {
            return;
        }
        let ty = match e.click_count {
            2 => SelectionType::Semantic,
            n if n >= 3 => SelectionType::Lines,
            _ => SelectionType::Simple,
        };
        self.term.lock().selection = Some(Selection::new(ty, at, side));
        self.selecting = true;
        cx.notify();
    }

    fn mouse_move(&mut self, e: &MouseMoveEvent, _: &mut Window, cx: &mut Context<'_, Self>) {
        let offset = self.term.lock().grid().display_offset();
        let (at, side) = self.cell_at(e.position, offset);
        if let Some(mode) = self.reports_mouse(e.modifiers.shift) {
            let held = e.pressed_button.is_some();
            if mode.contains(TermMode::MOUSE_MOTION)
                || (held && mode.contains(TermMode::MOUSE_DRAG))
            {
                let button = match e.pressed_button {
                    Some(MouseButton::Left) => 0,
                    Some(MouseButton::Middle) => 1,
                    Some(_) => 2,
                    None => 3,
                };
                self.report(mode, 32 + button + modifier_bits(&e.modifiers), at, true);
            }
            return;
        }
        if self.selecting && e.pressed_button == Some(MouseButton::Left) {
            if let Some(selection) = self.term.lock().selection.as_mut() {
                selection.update(at, side);
            }
            cx.notify();
        }
    }

    fn mouse_up(&mut self, e: &MouseUpEvent, _: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(mode) = self.reports_mouse(e.modifiers.shift) {
            let offset = self.term.lock().grid().display_offset();
            let (at, _) = self.cell_at(e.position, offset);
            let button = match e.button {
                MouseButton::Left => 0,
                MouseButton::Middle => 1,
                _ => 2,
            };
            self.report(mode, button + modifier_bits(&e.modifiers), at, false);
            return;
        }
        if self.selecting {
            self.selecting = false;
            let mut term = self.term.lock();
            if term.selection.as_ref().is_some_and(Selection::is_empty) {
                term.selection = None;
            }
            cx.notify();
        }
    }

    fn scroll(&mut self, e: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<'_, Self>) {
        let delta = e.delta.pixel_delta(LINE_HEIGHT);
        self.scroll_rest += f32::from(delta.y) / f32::from(LINE_HEIGHT);
        let lines = self.scroll_rest.trunc();
        if lines == 0. {
            return;
        }
        self.scroll_rest -= lines;
        // A wheel moves a handful of lines.
        #[allow(clippy::cast_possible_truncation)]
        let lines = lines as i32;
        let mode = *self.term.lock().mode();
        if let Some(mode) = self.reports_mouse(e.modifiers.shift) {
            let offset = self.term.lock().grid().display_offset();
            let (at, _) = self.cell_at(e.position, offset);
            let button = if lines > 0 { 64 } else { 65 };
            for _ in 0..lines.unsigned_abs() {
                self.report(mode, button + modifier_bits(&e.modifiers), at, true);
            }
        } else if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
            // Full-screen programs without mouse support scroll with arrows.
            let arrow: &[u8] = match (lines > 0, mode.contains(TermMode::APP_CURSOR)) {
                (true, true) => b"\x1bOA",
                (true, false) => b"\x1b[A",
                (false, true) => b"\x1bOB",
                (false, false) => b"\x1b[B",
            };
            let bytes = arrow.repeat(lines.unsigned_abs() as usize);
            let _ = self.pty.send(Msg::Input(bytes.into()));
        } else {
            self.term.lock().scroll_display(Scroll::Delta(lines));
            cx.notify();
        }
    }
}

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

fn window_size(size: GridSize, cell: gpui::Size<Pixels>) -> WindowSize {
    let px = |p: Pixels| {
        // Cell sizes are a few dozen pixels.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let v = f32::from(p).round() as u16;
        v
    };
    WindowSize {
        num_lines: u16::try_from(size.rows).unwrap_or(u16::MAX),
        num_cols: u16::try_from(size.cols).unwrap_or(u16::MAX),
        cell_width: px(cell.width),
        cell_height: px(cell.height),
    }
}

/// xterm's modifier bits in mouse reports.
fn modifier_bits(m: &gpui::Modifiers) -> u8 {
    4 * u8::from(m.shift) + 8 * u8::from(m.alt) + 16 * u8::from(m.control)
}

/// Pasted text as the program should get it: newlines as Return, and wrapped
/// in bracketed-paste markers when the program asked (with any markers inside
/// removed, so a paste can't end itself early).
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let inner = text.replace("\x1b[201~", "").replace("\x1b[200~", "");
        format!("\x1b[200~{inner}\x1b[201~").into_bytes()
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

/// One run of same-styled cells, ready to shape.
struct Run {
    origin: Point<Pixels>,
    text: String,
    /// Cells per glyph: 2 for wide characters.
    width: usize,
    color: Hsla,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
}

struct CursorPaint {
    bounds: Bounds<Pixels>,
    shape: CursorShape,
    /// The character under a block cursor, redrawn in the ground color.
    under: Option<Run>,
}

/// A frame of the grid, built under the lock and painted after.
struct Frame {
    backgrounds: Vec<(Bounds<Pixels>, Hsla)>,
    selection: Vec<(Bounds<Pixels>, Hsla)>,
    runs: Vec<Run>,
    cursor: Option<CursorPaint>,
}

fn cell_bounds(
    origin: Point<Pixels>,
    cell: gpui::Size<Pixels>,
    row: usize,
    col: usize,
    w: usize,
) -> Bounds<Pixels> {
    // Row and column counts are small.
    #[allow(clippy::cast_precision_loss)]
    let (r, c, w) = (row as f32, col as f32, w as f32);
    Bounds::new(
        point(origin.x + cell.width * c, origin.y + cell.height * r),
        size(cell.width * w, cell.height),
    )
}

/// Merge a cell rectangle into the last one when they touch on the same row.
fn push_rect(rects: &mut Vec<(Bounds<Pixels>, Hsla)>, b: Bounds<Pixels>, color: Hsla) {
    if let Some((last, c)) = rects.last_mut()
        && *c == color
        && last.origin.y == b.origin.y
        && last.right() == b.origin.x
    {
        last.size.width += b.size.width;
        return;
    }
    rects.push((b, color));
}

fn build_frame(
    term: &Term<Listener>,
    palette: &Palette,
    origin: Point<Pixels>,
    cell: gpui::Size<Pixels>,
    focused: bool,
) -> Frame {
    let content = term.renderable_content();
    let offset = i32::try_from(content.display_offset).unwrap_or(0);
    let mut frame = Frame {
        backgrounds: Vec::new(),
        selection: Vec::new(),
        runs: Vec::new(),
        cursor: None,
    };
    let cursor = content.cursor;
    let cursor_row = usize::try_from(cursor.point.line.0 + offset).ok();
    let mut current: Option<Run> = None;
    let mut current_row = usize::MAX;
    let mut cursor_cell: Option<(String, Hsla, Flags)> = None;

    for indexed in content.display_iter {
        let Ok(row) = usize::try_from(indexed.point.line.0 + offset) else {
            continue;
        };
        let col = indexed.point.column.0;
        let c = indexed.cell;
        if row != current_row {
            frame.runs.extend(current.take());
            current_row = row;
        }
        if c.flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }
        let wide = c.flags.contains(Flags::WIDE_CHAR);
        let width = if wide { 2 } else { 1 };
        let mut fg = palette.resolve(c.fg, content.colors);
        let mut bg = palette.resolve(c.bg, content.colors);
        if c.flags.contains(Flags::DIM) {
            fg = dim(fg);
        }
        if c.flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if c.flags.contains(Flags::HIDDEN) {
            fg = bg;
        }
        let bounds = cell_bounds(origin, cell, row, col, width);
        if bg != palette.bg {
            push_rect(&mut frame.backgrounds, bounds, hsla(bg));
        }
        if content
            .selection
            .as_ref()
            .is_some_and(|s| s.contains(indexed.point))
        {
            push_rect(&mut frame.selection, bounds, palette.selection);
        }

        let mut text = String::new();
        text.push(c.c);
        if let Some(zw) = c.zerowidth() {
            text.extend(zw);
        }
        if cursor_row == Some(row) && cursor.point.column.0 == col {
            cursor_cell = Some((text.clone(), hsla(bg), c.flags));
        }
        let color = hsla(fg);
        let (bold, italic) = (
            c.flags.contains(Flags::BOLD),
            c.flags.contains(Flags::ITALIC),
        );
        let underline = c.flags.intersects(Flags::ALL_UNDERLINES);
        let strike = c.flags.contains(Flags::STRIKEOUT);
        let blank = c.c == ' ' && !underline && !strike;
        let same = current.as_ref().is_some_and(|r| {
            r.width == 1
                && !wide
                && r.color == color
                && r.bold == bold
                && r.italic == italic
                && r.underline == underline
                && r.strike == strike
        });
        if same {
            if let Some(r) = current.as_mut() {
                r.text.push_str(&text);
            }
            continue;
        }
        frame.runs.extend(current.take());
        if blank {
            continue;
        }
        current = Some(Run {
            origin: bounds.origin,
            text,
            width,
            color,
            bold,
            italic,
            underline,
            strike,
        });
    }
    frame.runs.extend(current);
    for run in &mut frame.runs {
        let trimmed = run.text.trim_end_matches(' ').len();
        run.text.truncate(trimmed);
    }
    frame.runs.retain(|r| !r.text.is_empty());

    let shape = if content.mode.contains(TermMode::SHOW_CURSOR) {
        cursor.shape
    } else {
        CursorShape::Hidden
    };
    if let Some(row) = cursor_row
        && shape != CursorShape::Hidden
    {
        let col = cursor.point.column.0;
        let wide = cursor_cell
            .as_ref()
            .is_some_and(|(_, _, f)| f.contains(Flags::WIDE_CHAR));
        let bounds = cell_bounds(origin, cell, row, col, if wide { 2 } else { 1 });
        let under = cursor_cell
            .filter(|(c, ..)| focused && shape == CursorShape::Block && c.trim() != "")
            .map(|(text, bg, flags)| Run {
                origin: bounds.origin,
                text,
                width: if wide { 2 } else { 1 },
                // Inverted: the cell's ground on the cursor's color.
                color: bg,
                bold: flags.contains(Flags::BOLD),
                italic: flags.contains(Flags::ITALIC),
                underline: false,
                strike: false,
            });
        frame.cursor = Some(CursorPaint {
            bounds,
            shape,
            under,
        });
    }
    frame
}

fn paint_run(run: &Run, cell: gpui::Size<Pixels>, window: &mut Window, cx: &mut App) {
    let font = Font {
        weight: if run.bold {
            FontWeight::BOLD
        } else {
            FontWeight::NORMAL
        },
        style: if run.italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        },
        ..gpui::font(MONO)
    };
    let style = TextRun {
        len: run.text.len(),
        font,
        color: run.color,
        background_color: None,
        underline: run.underline.then_some(UnderlineStyle {
            thickness: px(1.),
            color: Some(run.color),
            wavy: false,
        }),
        strikethrough: run.strike.then_some(StrikethroughStyle {
            thickness: px(1.),
            color: Some(run.color),
        }),
    };
    // A fixed advance keeps every glyph on its cell, fallback fonts included.
    #[allow(clippy::cast_precision_loss)]
    let advance = cell.width * run.width as f32;
    let shaped = window.text_system().shape_line(
        SharedString::from(run.text.clone()),
        FONT_SIZE,
        &[style],
        Some(advance),
    );
    let _ = shaped.paint(run.origin, cell.height, TextAlign::Left, None, window, cx);
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let t = Tokens::get(cx);
        let ground = t.rail;
        let palette = Palette::new(&t, ground);
        let focused = self.focus.is_focused(window);
        let term = self.term.clone();
        let pty = self.pty.clone();
        let geometry = self.geometry.clone();
        let paint_geometry = self.geometry.clone();
        let view = cx.entity();
        let focus = self.focus.clone();
        let marked = self.marked.clone();
        let exit = self.exit;

        let grid = canvas(
            move |bounds, window, _cx| {
                let font_id = window.text_system().resolve_font(&gpui::font(MONO));
                let width = window
                    .text_system()
                    .advance(font_id, FONT_SIZE, 'm')
                    .map_or(px(7.2), |s| s.width);
                let cell = size(width, LINE_HEIGHT);
                let (w, h) = (
                    f32::from(bounds.size.width) / f32::from(cell.width),
                    f32::from(bounds.size.height) / f32::from(cell.height),
                );
                // Whole cells that fit; never fewer than a couple.
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let new = GridSize {
                    cols: (w.floor() as usize).max(2),
                    rows: (h.floor() as usize).max(1),
                };
                let mut g = geometry.get();
                let mut term = term.lock();
                if (g.cols, g.rows) != (new.cols, new.rows) || g.cell != cell {
                    term.resize(new);
                    let _ = pty.send(Msg::Resize(window_size(new, cell)));
                }
                g.origin = bounds.origin;
                g.cell = cell;
                g.cols = new.cols;
                g.rows = new.rows;
                geometry.set(g);
                build_frame(&term, &palette, bounds.origin, cell, focused)
            },
            move |bounds, frame, window, cx| {
                for (b, color) in &frame.backgrounds {
                    window.paint_quad(fill(*b, *color));
                }
                for (b, color) in &frame.selection {
                    window.paint_quad(fill(*b, *color));
                }
                let g = paint_geometry.get();
                for run in &frame.runs {
                    paint_run(run, g.cell, window, cx);
                }
                let mut cursor_bounds = None;
                if let Some(c) = &frame.cursor {
                    cursor_bounds = Some(c.bounds);
                    let color = palette.cursor;
                    match (c.shape, focused && exit.is_none()) {
                        (CursorShape::Block, true) => {
                            window.paint_quad(fill(c.bounds, color));
                            if let Some(under) = &c.under {
                                paint_run(under, g.cell, window, cx);
                            }
                        }
                        (CursorShape::Beam, true) => {
                            let mut b = c.bounds;
                            b.size.width = px(2.);
                            window.paint_quad(fill(b, color));
                        }
                        (CursorShape::Underline, true) => {
                            let mut b = c.bounds;
                            b.origin.y = b.bottom() - px(2.);
                            b.size.height = px(2.);
                            window.paint_quad(fill(b, color));
                        }
                        _ => window.paint_quad(outline(c.bounds, color, gpui::BorderStyle::Solid)),
                    }
                }
                if let (Some(text), Some(at)) = (&marked, cursor_bounds) {
                    let run = Run {
                        origin: at.origin,
                        text: text.clone(),
                        width: 1,
                        color: palette.cursor,
                        bold: false,
                        italic: false,
                        underline: true,
                        strike: false,
                    };
                    let shaped_width = g.cell.width
                        * f32::from(u16::try_from(text.chars().count()).unwrap_or(u16::MAX));
                    window.paint_quad(fill(
                        Bounds::new(at.origin, size(shaped_width, g.cell.height)),
                        hsla(palette.bg),
                    ));
                    paint_run(&run, g.cell, window, cx);
                }
                let mut g = g;
                g.cursor = cursor_bounds;
                paint_geometry.set(g);
                window.handle_input(&focus, ElementInputHandler::new(bounds, view), cx);
            },
        )
        .size_full();

        div()
            .id("terminal")
            .key_context("Terminal")
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .bg(ground)
            .px(PAD_X)
            .py(PAD_Y)
            .cursor_text()
            .on_key_down(cx.listener(Self::key_down))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::clear))
            .on_action(cx.listener(Self::select_all))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::mouse_up))
            .on_scroll_wheel(cx.listener(Self::scroll))
            .child(grid)
            .when_some(exit.filter(|code| *code != Some(0)), |d, code| {
                let what = code.map_or_else(
                    || "The process was stopped".to_owned(),
                    |c| format!("The process exited with code {c}"),
                );
                d.child(
                    div()
                        .absolute()
                        .left(PAD_X)
                        .right(PAD_X)
                        .bottom(PAD_Y)
                        .px(px(8.))
                        .py(px(4.))
                        .rounded(px(4.))
                        .bg(t.raised)
                        .text_color(t.text2)
                        .text_size(px(12.))
                        .child(format!("{what} · press any key to close")),
                )
            })
    }
}

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<'_, Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<'_, Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<'_, Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|m| 0..m.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) {
        self.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.marked = None;
        if self.exit.is_some() || text.is_empty() {
            cx.notify();
            return;
        }
        self.input(text.as_bytes().to_vec(), cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.marked = Some(text.to_owned()).filter(|t| !t.is_empty());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<'_, Self>,
    ) -> Option<Bounds<Pixels>> {
        self.geometry.get().cursor
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<'_, Self>,
    ) -> Option<usize> {
        None
    }

    fn accepts_text_input(&self, _: &mut Window, _: &mut Context<'_, Self>) -> bool {
        self.exit.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::paste_bytes;

    #[test]
    fn paste_turns_newlines_into_returns() {
        assert_eq!(paste_bytes("a\nb\r\nc", false), b"a\rb\rc".to_vec());
    }

    #[test]
    fn bracketed_paste_cannot_end_itself() {
        assert_eq!(
            paste_bytes("x\x1b[201~y", true),
            b"\x1b[200~xy\x1b[201~".to_vec()
        );
    }
}
