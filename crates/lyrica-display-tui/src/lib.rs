use std::io;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::prelude::*;
use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap,
};
use tokio::sync::{mpsc, watch};

use lyrica_core::display::{DisplayBackend, DisplayState, SchedulerCommand};
use lyrica_core::lyrics::Lyrics;
use lyrica_core::player::PlaybackStatus;
use lyrica_core::provider::SearchRequest;
use lyrica_provider::ProviderGroup;

/// TUI display backend using ratatui with search/select support.
pub struct TuiDisplay {
    cmd_tx: mpsc::Sender<SchedulerCommand>,
    provider: Arc<ProviderGroup>,
}

impl TuiDisplay {
    pub fn new(cmd_tx: mpsc::Sender<SchedulerCommand>, provider: ProviderGroup) -> Self {
        Self {
            cmd_tx,
            provider: Arc::new(provider),
        }
    }
}

#[async_trait::async_trait]
impl DisplayBackend for TuiDisplay {
    async fn run(&mut self, state_rx: watch::Receiver<DisplayState>) -> Result<()> {
        let cmd_tx = self.cmd_tx.clone();
        let provider = self.provider.clone();
        tokio::task::spawn_blocking(move || run_tui(state_rx, cmd_tx, provider))
            .await??;
        Ok(())
    }
}

/// Which view the TUI is showing.
#[derive(PartialEq)]
enum TuiMode {
    /// Normal lyrics display.
    Lyrics,
    /// Search input dialog.
    SearchInput,
    /// Search results selection.
    SearchResults,
}

struct TuiState {
    mode: TuiMode,
    /// Search input fields.
    search_title: String,
    search_artist: String,
    /// Which input field is active (0 = title, 1 = artist).
    search_focus: usize,
    /// Search result candidates.
    candidates: Vec<Lyrics>,
    /// List selection state.
    list_state: ListState,
    /// Status message shown briefly at the bottom.
    status_message: Option<(String, std::time::Instant)>,
    /// Is a search in progress?
    searching: bool,
}

impl TuiState {
    fn new() -> Self {
        Self {
            mode: TuiMode::Lyrics,
            search_title: String::new(),
            search_artist: String::new(),
            search_focus: 0,
            candidates: Vec::new(),
            list_state: ListState::default(),
            status_message: None,
            searching: false,
        }
    }

    fn set_status(&mut self, msg: &str) {
        self.status_message = Some((msg.to_string(), std::time::Instant::now()));
    }
}

fn run_tui(
    mut state_rx: watch::Receiver<DisplayState>,
    cmd_tx: mpsc::Sender<SchedulerCommand>,
    provider: Arc<ProviderGroup>,
) -> Result<()> {
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    let mut tui = TuiState::new();

    // Channel for receiving search results from async task.
    let (search_result_tx, search_result_rx) = std::sync::mpsc::channel::<Vec<Lyrics>>();

    let mut needs_redraw = true;

    loop {
        // Check for search results.
        if let Ok(results) = search_result_rx.try_recv() {
            tui.searching = false;
            if results.is_empty() {
                tui.set_status("No results found");
                tui.mode = TuiMode::Lyrics;
            } else {
                tui.set_status(&format!("{} candidates found", results.len()));
                tui.candidates = results;
                tui.list_state.select(Some(0));
                tui.mode = TuiMode::SearchResults;
            }
            needs_redraw = true;
        }

        // Clear expired status messages (after 3 seconds).
        if let Some((_, ts)) = &tui.status_message {
            if ts.elapsed() > Duration::from_secs(3) {
                tui.status_message = None;
                needs_redraw = true;
            }
        }

        // Only redraw when state changed or forced.
        if needs_redraw || state_rx.has_changed().unwrap_or(false) {
            state_rx.mark_unchanged();
            let display_state = state_rx.borrow().clone();
            terminal.draw(|frame| {
                render_main(frame, &display_state, &mut tui);
            })?;
            needs_redraw = false;
        }

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                let display_state = state_rx.borrow().clone();
                needs_redraw = true;
                match tui.mode {
                    TuiMode::Lyrics => match key.code {
                        KeyCode::Char('q') => break,
                        KeyCode::Char('s') | KeyCode::Char('/') => {
                            tui.search_title = display_state
                                .track.as_ref().map(|t| t.title.clone()).unwrap_or_default();
                            tui.search_artist = display_state
                                .track.as_ref().map(|t| t.artist.clone()).unwrap_or_default();
                            tui.search_focus = 0;
                            tui.mode = TuiMode::SearchInput;
                        }
                        KeyCode::Char('r') => {
                            let _ = cmd_tx.blocking_send(SchedulerCommand::ResearchCurrent);
                            tui.set_status("Re-searching...");
                        }
                        // Offset adjustment: +/- for 100ms, [/] for 500ms.
                        KeyCode::Char('+') | KeyCode::Char('=') => {
                            let _ = cmd_tx.blocking_send(SchedulerCommand::AdjustOffset { delta_ms: 100 });
                            tui.set_status(&format!("Offset: {}ms", display_state.offset_ms + 100));
                        }
                        KeyCode::Char('-') => {
                            let _ = cmd_tx.blocking_send(SchedulerCommand::AdjustOffset { delta_ms: -100 });
                            tui.set_status(&format!("Offset: {}ms", display_state.offset_ms - 100));
                        }
                        KeyCode::Char(']') => {
                            let _ = cmd_tx.blocking_send(SchedulerCommand::AdjustOffset { delta_ms: 500 });
                            tui.set_status(&format!("Offset: {}ms", display_state.offset_ms + 500));
                        }
                        KeyCode::Char('[') => {
                            let _ = cmd_tx.blocking_send(SchedulerCommand::AdjustOffset { delta_ms: -500 });
                            tui.set_status(&format!("Offset: {}ms", display_state.offset_ms - 500));
                        }
                        KeyCode::Char('0') => {
                            let _ = cmd_tx.blocking_send(SchedulerCommand::SetOffset { offset_ms: 0 });
                            tui.set_status("Offset reset to 0ms");
                        }
                        _ => {}
                    },
                    TuiMode::SearchInput => match key.code {
                        KeyCode::Esc => {
                            tui.mode = TuiMode::Lyrics;
                        }
                        KeyCode::Tab => {
                            tui.search_focus = (tui.search_focus + 1) % 2;
                        }
                        KeyCode::BackTab => {
                            tui.search_focus = if tui.search_focus == 0 { 1 } else { 0 };
                        }
                        KeyCode::Enter => {
                            if !tui.search_title.is_empty() && !tui.searching {
                                tui.searching = true;
                                tui.set_status("Searching...");
                                let title = tui.search_title.clone();
                                let artist = tui.search_artist.clone();
                                let p = provider.clone();
                                let tx = search_result_tx.clone();
                                std::thread::spawn(move || {
                                    let rt = tokio::runtime::Runtime::new().unwrap();
                                    let request = SearchRequest {
                                        title,
                                        artist,
                                        album: None,
                                        duration: None,
                                    };
                                    let results = rt.block_on(p.search_all(&request))
                                        .unwrap_or_default();
                                    let _ = tx.send(results);
                                });
                            }
                        }
                        KeyCode::Backspace => {
                            if tui.search_focus == 0 {
                                tui.search_title.pop();
                            } else {
                                tui.search_artist.pop();
                            }
                        }
                        KeyCode::Char(c) => {
                            // Ctrl+U to clear field.
                            if key.modifiers.contains(KeyModifiers::CONTROL) && c == 'u' {
                                if tui.search_focus == 0 {
                                    tui.search_title.clear();
                                } else {
                                    tui.search_artist.clear();
                                }
                            } else {
                                if tui.search_focus == 0 {
                                    tui.search_title.push(c);
                                } else {
                                    tui.search_artist.push(c);
                                }
                            }
                        }
                        _ => {}
                    },
                    TuiMode::SearchResults => match key.code {
                        KeyCode::Esc => {
                            tui.mode = TuiMode::Lyrics;
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            let i = tui.list_state.selected().unwrap_or(0);
                            if i > 0 {
                                tui.list_state.select(Some(i - 1));
                            }
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            let i = tui.list_state.selected().unwrap_or(0);
                            if i + 1 < tui.candidates.len() {
                                tui.list_state.select(Some(i + 1));
                            }
                        }
                        KeyCode::Enter => {
                            if let Some(idx) = tui.list_state.selected() {
                                if idx < tui.candidates.len() {
                                    let selected = tui.candidates[idx].clone();
                                    let source = selected.metadata.source.to_string();
                                    let _ = cmd_tx.blocking_send(SchedulerCommand::ApplyLyrics {
                                        lyrics: Arc::new(selected),
                                    });
                                    tui.set_status(&format!("Applied lyrics from {}", source));
                                    tui.mode = TuiMode::Lyrics;
                                }
                            }
                        }
                        KeyCode::Char('s') | KeyCode::Char('/') => {
                            // Go back to search input.
                            tui.mode = TuiMode::SearchInput;
                        }
                        _ => {}
                    },
                }
            }
        }
    }

    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}

// --- Rendering ---

fn render_main(frame: &mut Frame, state: &DisplayState, tui: &mut TuiState) {
    let area = frame.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Header.
            Constraint::Min(5),    // Lyrics.
            Constraint::Length(1), // Status bar.
        ])
        .split(area);

    // Header.
    let header_text = if let Some(ref track) = state.track {
        format!("{} - {}", track.title, track.artist)
    } else {
        "No track playing".to_string()
    };
    let header = Paragraph::new(header_text)
        .block(Block::default().borders(Borders::BOTTOM).title(" Lyrica "));
    frame.render_widget(header, chunks[0]);

    // Lyrics area.
    render_lyrics(frame, chunks[1], state);

    // Status bar.
    let status_icon = match state.status {
        PlaybackStatus::Playing => "▶",
        PlaybackStatus::Paused => "⏸",
        PlaybackStatus::Stopped => "⏹",
    };
    let position_secs = state.playback_position.as_secs();

    let offset_str = if state.offset_ms != 0 {
        format!(" [offset:{:+}ms]", state.offset_ms)
    } else {
        String::new()
    };

    let status_text = if let Some((ref msg, _)) = tui.status_message {
        format!(
            " {} {:02}:{:02}{} | {}",
            status_icon,
            position_secs / 60,
            position_secs % 60,
            offset_str,
            msg,
        )
    } else {
        format!(
            " {} {:02}:{:02}{} | s:search  r:re-search  +/-:offset  0:reset  q:quit",
            status_icon,
            position_secs / 60,
            position_secs % 60,
            offset_str,
        )
    };
    let status_bar = Paragraph::new(status_text).style(
        Style::default().fg(Color::Black).bg(Color::White),
    );
    frame.render_widget(status_bar, chunks[2]);

    // Overlay dialogs.
    match tui.mode {
        TuiMode::SearchInput => render_search_input(frame, area, tui),
        TuiMode::SearchResults => render_search_results(frame, area, tui),
        TuiMode::Lyrics => {}
    }
}

fn render_lyrics(frame: &mut Frame, area: Rect, state: &DisplayState) {
    let lyrics = match state.lyrics {
        Some(ref l) => l,
        None => {
            let msg = if state.track.is_some() {
                "No lyrics found. Press 's' to search manually."
            } else {
                "Waiting for lyrics..."
            };
            let p = Paragraph::new(msg)
                .alignment(Alignment::Center)
                .style(Style::default().fg(Color::DarkGray));
            frame.render_widget(p, area);
            return;
        }
    };

    let current_idx = state.current_line_index.unwrap_or(0);
    let visible_lines = area.height as usize;
    let context = visible_lines / 2;

    let start = current_idx.saturating_sub(context);
    let end = (start + visible_lines).min(lyrics.lines.len());

    let mut lines: Vec<Line> = Vec::new();
    for i in start..end {
        let lyric_line = &lyrics.lines[i];
        let is_current = Some(i) == state.current_line_index;

        let style = if is_current {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let mut spans = vec![Span::styled(&lyric_line.content, style)];

        if let Some(ref trans) = lyric_line.translation {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                trans,
                Style::default().fg(if is_current {
                    Color::Yellow
                } else {
                    Color::DarkGray
                }),
            ));
        }

        lines.push(Line::from(spans));
    }

    let paragraph = Paragraph::new(lines)
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_search_input(frame: &mut Frame, area: Rect, tui: &TuiState) {
    let popup = centered_rect(60, 9, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Search Lyrics (Enter to search, Esc to cancel) ")
        .style(Style::default().bg(Color::Black));
    frame.render_widget(block.clone(), popup);

    let inner = block.inner(popup);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Title label.
            Constraint::Length(1), // Title input.
            Constraint::Length(1), // Artist label.
            Constraint::Length(1), // Artist input.
            Constraint::Min(0),   // Padding.
        ])
        .split(inner);

    let title_label_style = if tui.search_focus == 0 {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let artist_label_style = if tui.search_focus == 1 {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    frame.render_widget(
        Paragraph::new("  Title:").style(title_label_style),
        chunks[0],
    );

    let title_display = if tui.search_focus == 0 {
        format!("  {}▎", tui.search_title)
    } else {
        format!("  {}", tui.search_title)
    };
    frame.render_widget(
        Paragraph::new(title_display).style(Style::default().fg(Color::White)),
        chunks[1],
    );

    frame.render_widget(
        Paragraph::new("  Artist:").style(artist_label_style),
        chunks[2],
    );

    let artist_display = if tui.search_focus == 1 {
        format!("  {}▎", tui.search_artist)
    } else {
        format!("  {}", tui.search_artist)
    };
    frame.render_widget(
        Paragraph::new(artist_display).style(Style::default().fg(Color::White)),
        chunks[3],
    );

    if tui.searching {
        frame.render_widget(
            Paragraph::new("  Searching...")
                .style(Style::default().fg(Color::Yellow)),
            chunks[4],
        );
    }
}

fn render_search_results(frame: &mut Frame, area: Rect, tui: &mut TuiState) {
    let height = (tui.candidates.len() as u16 + 4).min(area.height.saturating_sub(4));
    let popup = centered_rect(80, height, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(
            " {} results — ↑↓:navigate  Enter:select  Esc:cancel  s:new search ",
            tui.candidates.len()
        ))
        .style(Style::default().bg(Color::Black));

    let items: Vec<ListItem> = tui
        .candidates
        .iter()
        .enumerate()
        .map(|(i, lyrics)| {
            let source = &lyrics.metadata.source;
            let title = lyrics.metadata.title.as_deref().unwrap_or("?");
            let artist = lyrics.metadata.artist.as_deref().unwrap_or("?");
            let lines = lyrics.lines.len();
            let preview = lyrics
                .lines
                .iter()
                .find(|l| !l.content.is_empty())
                .map(|l| l.content.as_str())
                .unwrap_or("");

            let text = format!(
                " {:>2}. [{}] {} - {} ({} lines) │ {}",
                i + 1,
                source,
                title,
                artist,
                lines,
                preview,
            );
            ListItem::new(text)
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(list, popup, &mut tui.list_state);
}

/// Create a centered rectangle of given percentage width and absolute height.
fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(area.height.saturating_sub(height) / 2),
            Constraint::Length(height),
            Constraint::Min(0),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
