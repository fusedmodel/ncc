// NCC Terminal · TUI 命令台（P1：ratatui + crossterm）
//
// 全屏界面：标题栏 / 输出区（自动裁到可见行）/ 输入行 / 快捷键提示。
// 支持 Tab 补全、↑↓ 历史、Esc 清空、Ctrl+C / Ctrl+D / exit 退出；
// 非 TTY 自动回退到 REPL（见 terminal::run）。

use crate::config::CliConfig;
use crate::terminal;
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Terminal;
use std::io;
use std::time::Duration;

struct Deck {
    base: String,
    posix: String,
    output: Vec<String>,
    input: Vec<char>,
    cursor: usize,
    history: Vec<String>,
    hist: Option<usize>,
    quit: bool,
}

const MAX_OUT: usize = 2000;

impl Deck {
    fn new(cfg: &CliConfig) -> Self {
        Deck {
            base: cfg.base_url().clone(),
            posix: terminal::runtime_summary(),
            output: vec![
                "NCC Terminal — 能力命令台".to_string(),
                format!("  输入 help 查看命令；其它内容交给 POSIX shell；Ctrl+C / exit 退出。"),
            ],
            input: Vec::new(),
            cursor: 0,
            history: Vec::new(),
            hist: None,
            quit: false,
        }
    }

    fn add<I: IntoIterator<Item = String>>(&mut self, lines: I) {
        for l in lines {
            if self.output.len() >= MAX_OUT {
                self.output.remove(0);
            }
            self.output.push(l);
        }
    }

    fn exec(&mut self, cfg: &CliConfig, raw: &str) {
        self.history.push(raw.to_string());
        self.hist = None;
        let line = raw.trim();
        if line.is_empty() {
            self.add(std::iter::once(String::new()));
            return;
        }
        let mut parts = line.split_whitespace();
        let first = parts.next().unwrap_or("");
        match first {
            "exit" | "quit" | "q" => {
                self.add(vec!["bye 👋".to_string()]);
                self.quit = true;
            }
            "help" | "?" => self.add(terminal::HELP.lines().map(|s| s.to_string())),
            "runtime" => match parts.next().unwrap_or("status") {
                "status" => self.add(terminal::status(cfg).lines().map(|s| s.to_string())),
                "setup" => self.add(terminal::setup_text(cfg).lines().map(|s| s.to_string())),
                _ => self.add(vec!["用法: runtime status | runtime setup".to_string()]),
            },
            "update" => self.add(terminal::update_check().lines().map(|s| s.to_string())),
            "ncc" => {
                let args: Vec<String> = parts.map(|s| s.to_string()).collect();
                if args.is_empty() {
                    self.add(vec!["用法: ncc <子命令>，如 ncc install @org/pkg".to_string()]);
                } else {
                    match terminal::run_ncc_capture(&args) {
                        Ok(o) => {
                            let o = o.trim();
                            if o.is_empty() {
                                self.add(vec!["(无输出)".to_string()]);
                            } else {
                                self.add(o.lines().map(|s| s.to_string()));
                            }
                        }
                        Err(e) => self.add(vec![format!("✗ {e:#}")]),
                    }
                }
            }
            _ => match terminal::shell_capture(line) {
                Ok(o) => {
                    let o = o.trim();
                    if o.is_empty() {
                        self.add(vec![format!("$ {line}")]);
                    } else {
                        self.add(o.lines().map(|s| s.to_string()));
                    }
                }
                Err(e) => self.add(vec![format!("✗ {e:#}")]),
            },
        }
    }

    fn complete(&mut self) {
        let start = self
            .input
            .iter()
            .position(|c| !c.is_whitespace())
            .unwrap_or(self.input.len());
        let input: String = self.input[start..].iter().collect();
        if input.is_empty() {
            return;
        }
        let mut cands: Vec<String> = Vec::new();
        for b in terminal::BUILTINS {
            if b.starts_with(&input) {
                cands.push((*b).to_string());
            }
        }
        for s in terminal::NCC_SUBS {
            let full = format!("ncc {s}");
            if full.starts_with(&input) {
                cands.push(full);
            }
        }
        if cands.is_empty() {
            return;
        }
        cands.sort();
        cands.dedup();
        self.input = cands[0].clone().chars().collect();
        self.cursor = self.input.len();
    }
}

/// 主入口（在真实 TTY 中运行全屏 TUI）。
pub fn run(cfg: &CliConfig) -> Result<()> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut term = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    let mut deck = Deck::new(cfg);
    let res: Result<()> = (|| -> Result<()> {
        loop {
            term.draw(|f| {
                let area = f.size();
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(1),
                        Constraint::Min(3),
                        Constraint::Length(3),
                        Constraint::Length(1),
                    ])
                    .split(area);

                // 标题栏
                let title = format!(
                    " NCC Terminal — 能力命令台  (@ncc/terminal · base: {} · posix: {}) ",
                    deck.base, deck.posix
                );
                let t = Paragraph::new(Span::styled(
                    title,
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ));
                f.render_widget(t, chunks[0]);

                // 输出区：取最后可见行（避免与 scroll 状态纠缠）
                let out_rect = chunks[1];
                let vis = out_rect.height.saturating_sub(2) as usize;
                let start = deck.output.len().saturating_sub(vis);
                let lines: Vec<Line> = deck
                    .output
                    .iter()
                    .skip(start)
                    .map(|l| Line::from(l.as_str()))
                    .collect();
                let out = Paragraph::new(lines)
                    .block(Block::default().borders(Borders::ALL).title(" output "))
                    .wrap(Wrap { trim: false });
                f.render_widget(out, out_rect);

                // 输入行
                let prompt = Span::styled(
                    "$ ",
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                );
                let mut spans = vec![prompt];
                let left: String = deck.input[..deck.cursor].iter().collect();
                let right: String = deck.input.iter().skip(deck.cursor + 1).collect();
                spans.push(Span::raw(left));
                if let Some(c) = deck.input.get(deck.cursor) {
                    spans.push(Span::styled(
                        c.to_string(),
                        Style::default().add_modifier(Modifier::REVERSED),
                    ));
                }
                if !right.is_empty() {
                    spans.push(Span::raw(right));
                }
                let inp = Paragraph::new(Line::from(spans))
                    .block(Block::default().borders(Borders::ALL).title(" input "));
                f.render_widget(inp, chunks[2]);

                // 提示行
                let hint = Paragraph::new(Span::styled(
                    " Tab 补全 · ↑↓ 历史 · Esc 清空 · Enter 执行 · Ctrl+C / Ctrl+D / exit 退出 ",
                    Style::default().fg(Color::DarkGray),
                ));
                f.render_widget(hint, chunks[3]);
            })?;

            if deck.quit {
                break;
            }
            if !event::poll(Duration::from_millis(200))? {
                continue;
            }
            match event::read()? {
                Event::Key(k) => {
                    if k.kind != KeyEventKind::Press {
                        continue;
                    }
                    match k.code {
                        KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                            deck.quit = true;
                        }
                        KeyCode::Char('d') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                            if deck.input.is_empty() {
                                deck.quit = true;
                            }
                        }
                        KeyCode::Char(ch) => {
                            deck.input.insert(deck.cursor, ch);
                            deck.cursor += 1;
                        }
                        KeyCode::Backspace => {
                            if deck.cursor > 0 {
                                deck.input.remove(deck.cursor - 1);
                                deck.cursor -= 1;
                            }
                        }
                        KeyCode::Enter => {
                            let line: String = deck.input.iter().collect();
                            deck.input.clear();
                            deck.cursor = 0;
                            deck.exec(cfg, &line);
                        }
                        KeyCode::Tab => deck.complete(),
                        KeyCode::Esc => {
                            deck.input.clear();
                            deck.cursor = 0;
                        }
                        KeyCode::Up => {
                            if !deck.history.is_empty() {
                                let i = deck.hist.map(|h| h.saturating_sub(1)).unwrap_or(deck.history.len() - 1);
                                deck.hist = Some(i);
                                deck.input = deck.history[i].clone().chars().collect();
                                deck.cursor = deck.input.len();
                            }
                        }
                        KeyCode::Down => {
                            if let Some(h) = deck.hist {
                                if h + 1 < deck.history.len() {
                                    deck.hist = Some(h + 1);
                                    deck.input = deck.history[h + 1].clone().chars().collect();
                                } else {
                                    deck.hist = None;
                                    deck.input.clear();
                                }
                                deck.cursor = deck.input.len();
                            }
                        }
                        KeyCode::Left => {
                            deck.cursor = deck.cursor.saturating_sub(1);
                        }
                        KeyCode::Right => {
                            if deck.cursor < deck.input.len() {
                                deck.cursor += 1;
                            }
                        }
                        _ => {}
                    }
                }
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
        Ok(())
    })();

    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;
    res
}
