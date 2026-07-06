use std::io::{self, Stdout, Write};
use std::time::{Duration, Instant};

use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
        MouseButton, MouseEventKind,
    },
    execute, queue,
    style::{
        Attribute, Color, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor,
    },
    terminal::{
        self, disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::settings::Settings;

pub struct TerminalUi {
    stdout: Stdout,
    history: Vec<HistoryItem>,
    input: String,
    cursor: usize,
    suggestion: String,
    cursor_visible: bool,
    last_blink: Instant,
    working: Option<WorkingState>,
    history_scroll_offset: usize,
    active: bool,
}

enum HistoryItem {
    Status(String),
    User(String),
    Assistant(String),
}

struct WorkingState {
    started: Instant,
    frame: usize,
    last_tick: Instant,
}

impl TerminalUi {
    pub fn start() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture, Hide)?;
        Ok(Self {
            stdout,
            history: Vec::new(),
            input: String::new(),
            cursor: 0,
            suggestion: String::new(),
            cursor_visible: true,
            last_blink: Instant::now(),
            working: None,
            history_scroll_offset: 0,
            active: true,
        })
    }

    pub fn read_line(
        &mut self,
        settings: &Settings,
        suggest: &mut dyn FnMut(&str, &Settings) -> Option<String>,
        on_idle: &mut dyn FnMut() -> Vec<String>,
    ) -> io::Result<Option<String>> {
        self.input.clear();
        self.cursor = 0;
        self.suggestion.clear();
        self.cursor_visible = true;
        self.last_blink = Instant::now();
        self.draw(settings)?;

        loop {
            if !event::poll(Duration::from_millis(250))? {
                let idle_messages = on_idle();
                let mut should_draw = false;
                for message in idle_messages {
                    self.push_status(message);
                    should_draw = true;
                }
                let suggestion_changed = self.refresh_suggestion(settings, suggest);
                if suggestion_changed || should_draw {
                    self.draw(settings)?;
                } else {
                    self.tick_cursor(settings)?;
                }
                continue;
            }
            match event::read()? {
                Event::Key(key) => match self.handle_key(key) {
                    InputAction::Continue => {
                        self.cursor_visible = true;
                        self.last_blink = Instant::now();
                        self.refresh_suggestion(settings, suggest);
                        self.draw(settings)?;
                    }
                    InputAction::Submit => {
                        let line = self.input.trim().to_string();
                        self.input.clear();
                        self.cursor = 0;
                        self.suggestion.clear();
                        self.draw(settings)?;
                        return Ok(Some(line));
                    }
                    InputAction::Exit => return Ok(None),
                },
                Event::Mouse(mouse) => {
                    if matches!(
                        mouse.kind,
                        MouseEventKind::Down(MouseButton::Left)
                            | MouseEventKind::Drag(MouseButton::Left)
                    ) {
                        self.move_cursor_from_mouse(mouse.column, mouse.row)?;
                        self.cursor_visible = true;
                        self.last_blink = Instant::now();
                        self.refresh_suggestion(settings, suggest);
                        self.draw(settings)?;
                    } else if matches!(mouse.kind, MouseEventKind::ScrollUp) {
                        self.scroll_history(4);
                        self.draw(settings)?;
                    } else if matches!(mouse.kind, MouseEventKind::ScrollDown) {
                        self.scroll_history(-4);
                        self.draw(settings)?;
                    }
                }
                Event::Resize(_, _) => self.draw(settings)?,
                _ => {}
            }
        }
    }

    pub fn push_user(&mut self, text: &str) {
        self.history_scroll_offset = 0;
        self.history.push(HistoryItem::User(text.to_string()));
    }

    pub fn push_status(&mut self, text: impl Into<String>) {
        self.history_scroll_offset = 0;
        self.history.push(HistoryItem::Status(text.into()));
    }

    pub fn start_assistant_message(&mut self) {
        self.history.push(HistoryItem::Assistant(String::new()));
    }

    pub fn append_assistant_delta(&mut self, delta: &str, settings: &Settings) -> io::Result<()> {
        if !matches!(self.history.last(), Some(HistoryItem::Assistant(_))) {
            self.start_assistant_message();
        }
        if let Some(HistoryItem::Assistant(text)) = self.history.last_mut() {
            text.push_str(delta);
        }
        self.history_scroll_offset = 0;
        self.draw(settings)
    }

    pub fn start_working(&mut self, settings: &Settings) -> io::Result<()> {
        self.working = Some(WorkingState {
            started: Instant::now(),
            frame: 0,
            last_tick: Instant::now(),
        });
        self.draw(settings)
    }

    pub fn stop_working(&mut self, settings: &Settings) -> io::Result<()> {
        self.working = None;
        self.draw(settings)
    }

    pub fn tick_working(&mut self, settings: &Settings) -> io::Result<()> {
        let Some(working) = self.working.as_mut() else {
            return Ok(());
        };
        if working.last_tick.elapsed() >= Duration::from_millis(180) {
            working.frame = working.frame.wrapping_add(1);
            working.last_tick = Instant::now();
            self.draw(settings)?;
        }
        Ok(())
    }

    pub fn poll_working_interrupt(&mut self) -> io::Result<bool> {
        if !event::poll(Duration::from_millis(40))? {
            return Ok(false);
        }
        match event::read()? {
            Event::Key(key)
                if key.code == KeyCode::Esc
                    || (key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)) =>
            {
                Ok(true)
            }
            Event::Resize(_, _) => Ok(false),
            _ => Ok(false),
        }
    }

    pub fn confirm(&mut self, prompt: &str, settings: &Settings) -> io::Result<bool> {
        self.push_status(format!(
            "{prompt}\n\nPress y to approve, n or esc to reject."
        ));
        self.draw(settings)?;
        loop {
            match event::read()? {
                Event::Key(key) => match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => return Ok(true),
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => return Ok(false),
                    _ => {}
                },
                Event::Mouse(mouse) => {
                    if matches!(mouse.kind, MouseEventKind::ScrollUp) {
                        self.scroll_history(4);
                        self.draw(settings)?;
                    } else if matches!(mouse.kind, MouseEventKind::ScrollDown) {
                        self.scroll_history(-4);
                        self.draw(settings)?;
                    }
                }
                Event::Resize(_, _) => self.draw(settings)?,
                _ => {}
            }
        }
    }

    pub fn draw(&mut self, settings: &Settings) -> io::Result<()> {
        let (cols, rows) = terminal::size()?;
        queue!(self.stdout, Clear(ClearType::All), MoveTo(0, 0))?;
        self.draw_header(cols, settings)?;
        self.draw_history(cols, rows)?;
        self.draw_working(cols, rows)?;
        self.draw_input(cols, rows)?;
        self.place_cursor(cols, rows)?;
        self.stdout.flush()
    }

    fn draw_header(&mut self, cols: u16, settings: &Settings) -> io::Result<()> {
        let width = cols.min(92).saturating_sub(1);
        if width < 24 {
            return Ok(());
        }

        let horizontal = "─".repeat(width.saturating_sub(2) as usize);
        queue!(
            self.stdout,
            MoveTo(0, 0),
            SetForegroundColor(Color::DarkGrey),
            Print(format!("┌{horizontal}┐")),
            MoveTo(0, 1),
            Print("│"),
            SetForegroundColor(Color::White),
            SetAttribute(Attribute::Bold),
            Print("  pwcli"),
            ResetColor,
            SetForegroundColor(Color::DarkGrey),
            Print(format!(
                "{:>pad$}│",
                "",
                pad = width.saturating_sub(9) as usize
            )),
            MoveTo(0, 2),
            Print("│  provider: "),
            SetForegroundColor(Color::Cyan),
            Print(truncate_to_width(
                &settings.provider,
                width.saturating_sub(15) as usize
            )),
            ResetColor,
            SetForegroundColor(Color::DarkGrey),
            MoveTo(0, 3),
            Print("│  model:    "),
            SetForegroundColor(Color::Green),
            Print(truncate_to_width(
                &settings.model,
                width.saturating_sub(15) as usize
            )),
            ResetColor,
            SetForegroundColor(Color::DarkGrey),
            MoveTo(0, 4),
            Print(format!(
                "│  thinking: {:<3}  /help  /providers  /models  /context  /exit",
                if settings.thinking { "on" } else { "off" }
            )),
            MoveTo(0, 5),
            Print(format!("└{horizontal}┘")),
            ResetColor
        )?;
        Ok(())
    }

    fn draw_history(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        if rows < 10 {
            return Ok(());
        }
        let start_y = 7;
        let end_y = rows.saturating_sub(if self.working.is_some() { 7 } else { 5 });
        if end_y <= start_y {
            return Ok(());
        }
        let height = (end_y - start_y + 1) as usize;
        let wrapped = render_history_lines(&self.history, cols.saturating_sub(4) as usize);
        let max_offset = wrapped.len().saturating_sub(height);
        self.history_scroll_offset = self.history_scroll_offset.min(max_offset);
        let end = wrapped.len().saturating_sub(self.history_scroll_offset);
        let start = end.saturating_sub(height);
        for (idx, line) in wrapped[start..end].iter().enumerate() {
            let y = start_y + idx as u16;
            queue!(self.stdout, MoveTo(1, y))?;
            if let Some(bg) = line.bg {
                queue!(self.stdout, SetBackgroundColor(bg))?;
            }
            queue!(
                self.stdout,
                SetForegroundColor(line.fg),
                SetAttribute(if line.bold {
                    Attribute::Bold
                } else {
                    Attribute::Reset
                }),
                Print(pad_to_width(&line.text, cols.saturating_sub(2) as usize)),
                ResetColor
            )?;
        }
        Ok(())
    }

    fn draw_working(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        let Some(working) = &self.working else {
            return Ok(());
        };
        if rows < 8 || cols < 20 {
            return Ok(());
        }
        let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let elapsed = working.started.elapsed().as_secs().max(1);
        let text = format!(
            "{} Working ({}s · esc to interrupt)",
            frames[working.frame % frames.len()],
            elapsed
        );
        queue!(
            self.stdout,
            MoveTo(1, rows.saturating_sub(5)),
            SetForegroundColor(Color::DarkGrey),
            Print(pad_to_width(&text, cols.saturating_sub(2) as usize)),
            ResetColor
        )?;
        Ok(())
    }

    fn draw_input(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        if rows < 4 || cols < 8 {
            return Ok(());
        }
        let y = rows - 3;
        let width = cols.saturating_sub(2);
        let input_width = width.saturating_sub(5) as usize;
        let (before, after, ghost, _) =
            visible_input(&self.input, self.cursor, &self.suggestion, input_width);
        let horizontal = "─".repeat(width.saturating_sub(2) as usize);
        queue!(
            self.stdout,
            MoveTo(1, y),
            SetForegroundColor(Color::DarkGrey),
            Print(format!("┌{horizontal}┐")),
            MoveTo(1, y + 1),
            Print("│"),
            SetForegroundColor(Color::DarkGrey),
            Print(" > "),
            SetForegroundColor(Color::White),
            Print(before),
            SetForegroundColor(if self.cursor_visible {
                Color::White
            } else {
                Color::DarkGrey
            }),
            Print("│"),
            SetForegroundColor(Color::White),
            Print(after),
            SetForegroundColor(Color::DarkGrey),
            Print(ghost),
            ResetColor,
            SetForegroundColor(Color::DarkGrey),
            MoveTo(cols.saturating_sub(2), y + 1),
            Print("│"),
            MoveTo(1, y + 2),
            Print(format!("└{horizontal}┘")),
            ResetColor
        )?;
        Ok(())
    }

    fn place_cursor(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        if rows < 4 || cols < 8 {
            return Ok(());
        }
        let input_width = cols.saturating_sub(7) as usize;
        let (_, _, _, cursor_col) =
            visible_input(&self.input, self.cursor, &self.suggestion, input_width);
        let x = 5 + cursor_col as u16;
        let max_x = cols.saturating_sub(3);
        queue!(self.stdout, MoveTo(x.min(max_x), rows - 2))
    }

    fn handle_key(&mut self, key: KeyEvent) -> InputAction {
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                InputAction::Exit
            }
            KeyCode::Char(ch) => {
                self.input.insert(self.cursor, ch);
                self.cursor += ch.len_utf8();
                InputAction::Continue
            }
            KeyCode::Tab => {
                self.accept_suggestion();
                InputAction::Continue
            }
            KeyCode::Backspace => {
                if let Some(prev) = previous_char_boundary(&self.input, self.cursor) {
                    self.input.drain(prev..self.cursor);
                    self.cursor = prev;
                }
                InputAction::Continue
            }
            KeyCode::Delete => {
                if let Some(next) = next_char_boundary(&self.input, self.cursor) {
                    self.input.drain(self.cursor..next);
                }
                InputAction::Continue
            }
            KeyCode::Left => {
                if let Some(prev) = previous_char_boundary(&self.input, self.cursor) {
                    self.cursor = prev;
                }
                InputAction::Continue
            }
            KeyCode::Right => {
                if self.cursor == self.input.len() && !self.suggestion.is_empty() {
                    self.accept_suggestion();
                } else if let Some(next) = next_char_boundary(&self.input, self.cursor) {
                    self.cursor = next;
                }
                InputAction::Continue
            }
            KeyCode::Home => {
                self.cursor = 0;
                InputAction::Continue
            }
            KeyCode::End => {
                self.cursor = self.input.len();
                InputAction::Continue
            }
            KeyCode::PageUp => {
                self.scroll_history(8);
                InputAction::Continue
            }
            KeyCode::PageDown => {
                self.scroll_history(-8);
                InputAction::Continue
            }
            KeyCode::Enter => InputAction::Submit,
            KeyCode::Esc => InputAction::Exit,
            _ => InputAction::Continue,
        }
    }

    fn scroll_history(&mut self, delta: isize) {
        if delta > 0 {
            self.history_scroll_offset = self.history_scroll_offset.saturating_add(delta as usize);
        } else {
            self.history_scroll_offset = self
                .history_scroll_offset
                .saturating_sub(delta.unsigned_abs());
        }
    }

    fn move_cursor_from_mouse(&mut self, column: u16, row: u16) -> io::Result<()> {
        let (_, rows) = terminal::size()?;
        if row != rows.saturating_sub(2) {
            return Ok(());
        }
        let input_start = 5;
        if column < input_start {
            self.cursor = 0;
            return Ok(());
        }
        self.cursor = byte_index_after_column(&self.input, (column - input_start) as usize);
        Ok(())
    }

    fn accept_suggestion(&mut self) {
        if self.cursor == self.input.len() && !self.suggestion.is_empty() {
            self.input.push_str(&self.suggestion);
            self.cursor = self.input.len();
            self.suggestion.clear();
        }
    }

    fn refresh_suggestion(
        &mut self,
        settings: &Settings,
        suggest: &mut dyn FnMut(&str, &Settings) -> Option<String>,
    ) -> bool {
        let next = if self.cursor == self.input.len() {
            suggest(&self.input, settings).unwrap_or_default()
        } else {
            String::new()
        };
        if next == self.suggestion {
            return false;
        }
        self.suggestion = next;
        true
    }

    fn tick_cursor(&mut self, settings: &Settings) -> io::Result<()> {
        if self.last_blink.elapsed() >= Duration::from_millis(550) {
            self.cursor_visible = !self.cursor_visible;
            self.last_blink = Instant::now();
            self.draw(settings)?;
        }
        Ok(())
    }
}

impl Drop for TerminalUi {
    fn drop(&mut self) {
        if self.active {
            let _ = execute!(self.stdout, Show, DisableMouseCapture, LeaveAlternateScreen);
            let _ = disable_raw_mode();
            self.active = false;
        }
    }
}

enum InputAction {
    Continue,
    Submit,
    Exit,
}

fn previous_char_boundary(input: &str, index: usize) -> Option<usize> {
    input[..index].char_indices().last().map(|(idx, _)| idx)
}

fn next_char_boundary(input: &str, index: usize) -> Option<usize> {
    input[index..]
        .char_indices()
        .nth(1)
        .map(|(idx, _)| index + idx)
        .or_else(|| (index < input.len()).then_some(input.len()))
}

fn byte_index_after_column(input: &str, column: usize) -> usize {
    let mut width = 0;
    for (idx, ch) in input.char_indices() {
        let ch_width = ch.width().unwrap_or(1).max(1);
        if column < width + ch_width {
            return idx + ch.len_utf8();
        }
        width += ch_width;
    }
    input.len()
}

fn visible_input(
    input: &str,
    cursor: usize,
    suggestion: &str,
    max_width: usize,
) -> (String, String, String, usize) {
    let max_width = max_width.max(1);
    let before = tail_to_width(&input[..cursor], max_width.saturating_sub(1));
    let before_width = before.width();

    let after_width = max_width.saturating_sub(before_width + 1);
    let after = truncate_to_width(&input[cursor..], after_width);
    let mut ghost = String::new();
    if cursor == input.len() && !suggestion.is_empty() {
        let used = after.width();
        if used < after_width {
            ghost = ghost_text(suggestion, after_width - used);
        }
    }

    (before, after, ghost, before_width)
}

fn ghost_text(input: &str, max_width: usize) -> String {
    truncate_to_width(input, max_width)
}

fn tail_to_width(input: &str, max_width: usize) -> String {
    let mut out = String::new();
    let mut width = 0;
    for ch in input.chars().rev() {
        let ch_width = ch.width().unwrap_or(1);
        if width + ch_width > max_width {
            break;
        }
        width += ch_width;
        out.insert(0, ch);
    }
    out
}

fn truncate_to_width(input: &str, max_width: usize) -> String {
    let mut out = String::new();
    let mut width = 0;
    for ch in input.chars() {
        let ch_width = ch.width().unwrap_or(1);
        if width + ch_width > max_width {
            break;
        }
        width += ch_width;
        out.push(ch);
    }
    out
}

struct RenderLine {
    text: String,
    fg: Color,
    bg: Option<Color>,
    bold: bool,
}

fn render_history_lines(items: &[HistoryItem], max_width: usize) -> Vec<RenderLine> {
    let max_width = max_width.max(1);
    let mut out = Vec::new();
    for item in items {
        match item {
            HistoryItem::Status(text) => {
                for line in wrap_text(text, max_width) {
                    out.push(RenderLine {
                        text: line,
                        fg: Color::DarkGrey,
                        bg: None,
                        bold: false,
                    });
                }
            }
            HistoryItem::User(text) => {
                for (idx, line) in wrap_text(text, max_width.saturating_sub(4))
                    .into_iter()
                    .enumerate()
                {
                    out.push(RenderLine {
                        text: if idx == 0 {
                            format!("› {line}")
                        } else {
                            format!("  {line}")
                        },
                        fg: Color::White,
                        bg: Some(Color::DarkGrey),
                        bold: true,
                    });
                }
                out.push(RenderLine {
                    text: String::new(),
                    fg: Color::Reset,
                    bg: None,
                    bold: false,
                });
            }
            HistoryItem::Assistant(text) => {
                for line in wrap_text(text, max_width.saturating_sub(4)) {
                    out.push(RenderLine {
                        text: format!("  {line}"),
                        fg: Color::Grey,
                        bg: None,
                        bold: false,
                    });
                }
                out.push(RenderLine {
                    text: String::new(),
                    fg: Color::Reset,
                    bg: None,
                    bold: false,
                });
            }
        }
    }
    out
}

fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
    let max_width = max_width.max(1);
    let mut out = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut current = String::new();
        let mut width = 0;
        for ch in line.chars() {
            let ch_width = ch.width().unwrap_or(1);
            if width + ch_width > max_width && !current.is_empty() {
                out.push(current);
                current = String::new();
                width = 0;
            }
            current.push(ch);
            width += ch_width;
        }
        if !current.is_empty() {
            out.push(current);
        }
    }
    out
}

fn pad_to_width(input: &str, max_width: usize) -> String {
    let mut text = truncate_to_width(input, max_width);
    let width = text.width();
    if width < max_width {
        text.push_str(&" ".repeat(max_width - width));
    }
    text
}
