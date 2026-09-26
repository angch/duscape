use ::ratatui::buffer::Buffer;
use ::ratatui::layout::Rect;
use ::ratatui::style::{Color, Modifier, Style};
use ::ratatui::widgets::Widget;
use ::std::path::{Path, PathBuf};
use ::std::time::{Duration, Instant};

use libduscape::format::{DisplayCount, DisplaySize, truncate_middle};
use libduscape::tiles::{FileMetadata, FileType};

use crate::config::Keybinds;
use crate::state::UiEffects;

fn render_currently_selected(
    buf: &mut Buffer,
    currently_selected: &FileMetadata,
    max_len: u16,
    y: u16,
) {
    let file_name = currently_selected.name.to_string_lossy();
    let size = DisplaySize(currently_selected.size as f64);
    let descendants = currently_selected.descendants;
    let (style, lines) = match currently_selected.file_type {
        FileType::File => (
            Style::default().add_modifier(Modifier::BOLD),
            vec![
                format!("SELECTED: {} ({})", file_name, size),
                format!("SELECTED: {}", file_name),
                format!("{}", file_name),
            ],
        ),
        FileType::Folder => (
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
            vec![
                format!(
                    "SELECTED: {} ({}, {} files)",
                    file_name,
                    size,
                    DisplayCount(descendants.expect("a folder should have descendants"))
                ),
                format!("SELECTED: {} ({})", file_name, size),
                format!("SELECTED: {}", file_name),
                format!("{}", file_name),
            ],
        ),
    };
    for line in lines {
        if (line.chars().count() as u16) < max_len {
            buf.set_string(1, y, line, style);
            break;
        }
    }
}

fn render_last_read_path(buf: &mut Buffer, last_read_path: &Path, max_len: u16, y: u16) {
    let last_read_path = last_read_path.to_string_lossy();
    if (last_read_path.chars().count() as u16) < max_len {
        buf.set_string(1, y, last_read_path, Style::default());
    } else {
        buf.set_string(
            1,
            y,
            truncate_middle(&last_read_path, max_len),
            Style::default(),
        );
    }
}

/// The help line, spelled with the keys actually bound so that it cannot disagree with them.
/// `scanning` leaves out what cannot be done until the scan is done: delete and rescan.
fn controls_legend(kb: &Keybinds, scanning: bool, switch_panel: bool) -> String {
    let delete = if scanning {
        String::new()
    } else {
        format!("<{}> - delete, ", kb.delete)
    };
    let rescan = if scanning {
        String::new()
    } else {
        format!("<{}/{}> - rescan folder/all, ", kb.rescan, kb.rescan_all)
    };
    let switch = if switch_panel {
        format!("<{}> - list/map, ", kb.switch_panel)
    } else {
        String::new()
    };
    format!(
        "<arrows> - move around, {switch}<{enter}> - enter folder, <{parent}> - parent folder, \
         {delete}{rescan}<{zoom_in}/{zoom_out}/{reset_zoom}> - zoom in/out/reset, \
         <{toggle_size}> - disk/apparent size, <{quit}> - quit",
        toggle_size = kb.toggle_size,
        enter = kb.enter,
        parent = kb.parent,
        zoom_in = kb.zoom_in,
        zoom_out = kb.zoom_out,
        reset_zoom = kb.reset_zoom,
        quit = kb.quit,
    )
}

/// What the help line tells an idle reader between showings of the legend, with the keys bound.
fn tips(kb: &Keybinds, scanning: bool, switch_panel: bool) -> Vec<String> {
    let mut tips = vec![
        "Tip: Ctrl+click entries in the list or the treemap to mark several at once".to_string(),
        "Tip: right-click an entry to copy its path, relative to where duscape was started"
            .to_string(),
        "Tip: double right-click copies the absolute path instead".to_string(),
        "Tip: double-click a folder to go into it".to_string(),
    ];
    if switch_panel {
        tips.push(format!(
            "Tip: Shift+Up/Down in the list marks a range and copies its paths; <{}> moves between \
             the list and the treemap",
            kb.switch_panel
        ));
    }
    if !scanning {
        tips.push(format!(
            "Tip: <{}> deletes every marked entry at once, after asking",
            kb.delete
        ));
        tips.push(format!(
            "Tip: <{}> rescans the selected folder after it changes on disk; <{}> rescans everything",
            kb.rescan, kb.rescan_all
        ));
    }
    tips.push(format!(
        "Tip: <{}> zooms in to show more nested folders as tiles, <{}> back out",
        kb.zoom_in, kb.zoom_out
    ));
    tips.push(format!(
        "Tip: <{}> switches between space used on disk and apparent size (file lengths), \
         without scanning again",
        kb.toggle_size
    ));
    tips.push("Tip: every key can be rebound in ~/.config/duscape/config.toml".to_string());
    tips.push(
        "Tip: when files fail to read, `duscape --issues FOLDER` prints why, and where".to_string(),
    );
    tips
}

/// Between one segment of the help line and the next.
const SEPARATOR: &str = "   ·   ";

/// Where the help line is, and whether it is on the move.
///
/// The line is an endless strip — the legend, a tip, the legend, the next tip — that rests at
/// least [`REST`] at each stop and then slides to the next in [`SLIDE`]. A segment that fits
/// has one stop; a longer one is shown a page at a time. The position is kept within a
/// segment, not as an offset along the strip, so that when the legend's text changes (the scan
/// finishing adds delete and rescan to it) nothing already on screen jumps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ticker {
    /// Even segments are the legend; odd ones the tips, in turn.
    segment: usize,
    /// Columns of the segment off the left edge at the current stop.
    column: usize,
    slide: Option<Slide>,
    /// When the line reached this stop. `None` until it first moves: it has been there since
    /// the program started.
    arrived: Option<Instant>,
}

/// A move from one stop to the next, in columns of the current segment; `to` may run past its
/// end, into the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Slide {
    from: usize,
    to: usize,
    /// Headed for the next segment's start rather than a page within this one. Kept apart from
    /// `to` so that a segment whose text changes mid-slide — the legend, when the scan ends —
    /// still lands on the next one.
    to_next: bool,
    started: Instant,
    /// Where it was last drawn.
    shown: usize,
}

impl Ticker {
    /// Whether it is sliding, and so wants [`FRAME`] ticks rather than [`IDLE_TICK`] ones.
    pub fn sliding(&self) -> bool {
        self.slide.is_some()
    }
}

/// How long a slide takes. With a tick every [`FRAME`], the last frame lands within 100 ms.
const SLIDE: Duration = Duration::from_millis(80);

/// The shortest time the line stands still at a stop, counted from its arrival or the last key
/// press, whichever is later: a key press restarts the wait.
pub const REST: Duration = Duration::from_secs(5);

/// Ticks while sliding: about 60 a second.
pub const FRAME: Duration = Duration::from_millis(16);

/// Ticks while resting, which only have to notice that the rest is over.
pub const IDLE_TICK: Duration = Duration::from_millis(250);

/// Columns a page of a long segment repeats from the page before, so the reader keeps their place.
const PAGE_OVERLAP: usize = 6;

/// The segments of the strip and how they are styled, for the keys and the screen as they are.
pub struct Strip {
    legend: String,
    tips: Vec<String>,
}

impl Strip {
    pub fn new(kb: &Keybinds, scanning: bool, switch_panel: bool) -> Self {
        Self {
            legend: controls_legend(kb, scanning, switch_panel),
            tips: tips(kb, scanning, switch_panel),
        }
    }
    fn segment(&self, index: usize) -> &str {
        if index.is_multiple_of(2) || self.tips.is_empty() {
            &self.legend
        } else {
            &self.tips[(index / 2) % self.tips.len()]
        }
    }
    fn segments(&self) -> usize {
        (self.tips.len() * 2).max(1)
    }
    /// A segment's columns, with the separator after it.
    fn span(&self, index: usize) -> usize {
        self.segment(index).chars().count() + SEPARATOR.chars().count()
    }
    /// Where a segment rests, in `width` columns: its start, and for one too long to fit, a page
    /// further each time, the last with the segment's end at the right edge.
    fn stops(&self, index: usize, width: usize) -> Vec<usize> {
        let len = self.segment(index).chars().count();
        let last = len.saturating_sub(width);
        let step = width.saturating_sub(PAGE_OVERLAP).max(1);
        let mut stops: Vec<usize> = (0..last).step_by(step).collect();
        stops.push(last);
        stops
    }
    /// Where the line stands at rest: the stop it is at, or the segment's last stop if the
    /// segment has since grown shorter than that.
    fn resting(&self, ticker: &Ticker, width: usize) -> usize {
        let last = self
            .stops(ticker.segment, width)
            .last()
            .copied()
            .unwrap_or(0);
        ticker.column.min(last)
    }
    /// Move `ticker` on to `now`, `width` columns wide, with no key pressed since `quiet_since`.
    /// Returns whether what is on screen changed.
    pub fn advance(
        &self,
        ticker: &mut Ticker,
        width: u16,
        quiet_since: Instant,
        now: Instant,
    ) -> bool {
        let width = usize::from(width);
        ticker.segment %= self.segments();
        if let Some(mut slide) = ticker.slide {
            let progress =
                now.saturating_duration_since(slide.started).as_secs_f64() / SLIDE.as_secs_f64();
            if progress >= 1.0 {
                if slide.to_next {
                    ticker.segment = (ticker.segment + 1) % self.segments();
                    ticker.column = 0;
                } else {
                    ticker.column = slide.to;
                }
                ticker.slide = None;
                ticker.arrived = Some(now);
                return true;
            }
            // Eased out: fast away, gentle into place.
            let eased = 1.0 - (1.0 - progress).powi(3);
            let shown = slide.from + ((slide.to - slide.from) as f64 * eased).round() as usize;
            let moved = shown != slide.shown;
            slide.shown = shown;
            ticker.slide = Some(slide);
            return moved;
        }
        let since = ticker
            .arrived
            .map_or(quiet_since, |arrived| arrived.max(quiet_since));
        if now.saturating_duration_since(since) < REST {
            return false;
        }
        let from = self.resting(ticker, width);
        let page = self
            .stops(ticker.segment, width)
            .into_iter()
            .find(|&stop| stop > from);
        let to = page.unwrap_or_else(|| self.span(ticker.segment));
        ticker.column = from;
        ticker.slide = Some(Slide {
            from,
            to,
            to_next: page.is_none(),
            started: now,
            shown: from,
        });
        false
    }
    /// Draw `width` columns of the strip at `ticker`, starting at `x`.
    fn render(&self, buf: &mut Buffer, ticker: Ticker, x: u16, y: u16, width: u16) {
        let legend = Style::default().add_modifier(Modifier::BOLD);
        let tip = Style::default();
        let mut segment = ticker.segment % self.segments();
        let mut skip = match ticker.slide {
            Some(slide) => slide.shown,
            None => self.resting(&ticker, usize::from(width)),
        };
        let mut column = 0u16;
        while column < width {
            let style = if segment.is_multiple_of(2) {
                legend
            } else {
                tip
            };
            for c in self.segment(segment).chars().chain(SEPARATOR.chars()) {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                if column >= width {
                    return;
                }
                buf[(x + column, y)].set_char(c).set_style(style);
                column += 1;
            }
            segment = (segment + 1) % self.segments();
        }
    }
}

fn render_small_files_legend(buf: &mut Buffer, x: u16, y: u16, small_files_legend: &str) {
    buf.set_string(
        x,
        y,
        small_files_legend,
        Style::default()
            .fg(Color::Reset)
            .bg(Color::Reset)
            .remove_modifier(Modifier::all()),
    );
    let small_files_legend_character = &mut buf[(x + 1, y)];
    small_files_legend_character.set_style(Style::default().bg(Color::White).fg(Color::Black));
}

pub struct BottomLine<'a> {
    keybinds: &'a Keybinds,
    /// Leave out of the help what cannot be done until the scan is done.
    scanning: bool,
    hide_small_files_legend: bool,
    currently_selected: Option<&'a FileMetadata>,
    /// Name the key that moves between the list and the treemap, while there is a list.
    switch_panel_hint: bool,
    last_read_path: Option<&'a PathBuf>,
    /// What is being rescanned, shown in place of the entry in hand until it is done.
    rescanning: Option<&'a str>,
    ticker: Ticker,
}

impl<'a> BottomLine<'a> {
    pub fn new(keybinds: &'a Keybinds, ui_effects: &'a UiEffects) -> Self {
        Self {
            keybinds,
            scanning: false,
            hide_small_files_legend: false,
            currently_selected: None,
            switch_panel_hint: false,
            last_read_path: None,
            rescanning: ui_effects.rescanning.as_deref(),
            ticker: ui_effects.ticker,
        }
    }
    pub fn scanning(mut self) -> Self {
        self.scanning = true;
        self
    }
    pub fn hide_small_files_legend(mut self, should_hide_small_files_legend: bool) -> Self {
        self.hide_small_files_legend = should_hide_small_files_legend;
        self
    }
    pub fn currently_selected(mut self, currently_selected: Option<&'a FileMetadata>) -> Self {
        self.currently_selected = currently_selected;
        self
    }
    pub fn switch_panel_hint(mut self, show: bool) -> Self {
        self.switch_panel_hint = show;
        self
    }
    pub fn last_read_path(mut self, last_read_path: Option<&'a PathBuf>) -> Self {
        self.last_read_path = last_read_path;
        self
    }
}

impl<'a> Widget for BottomLine<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let small_files_legend = "(x = Small files)";
        let small_files_len = if self.hide_small_files_legend {
            0
        } else {
            small_files_legend.chars().count() as u16
        };
        let max_status_len = area.width - small_files_len - 1;
        let status_line_y = area.y + area.height - 2;
        let controls_line_y = status_line_y + 1;
        if let Some(rescanning) = self.rescanning {
            let line = format!("RESCANNING: {rescanning}");
            buf.set_string(
                1,
                status_line_y,
                truncate_middle(&line, max_status_len.saturating_sub(1)),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            );
        } else if let Some(currently_selected) = self.currently_selected {
            render_currently_selected(buf, currently_selected, max_status_len, status_line_y);
        } else if let Some(last_read_path) = self.last_read_path {
            render_last_read_path(buf, last_read_path, max_status_len, status_line_y);
        }

        if !self.hide_small_files_legend {
            render_small_files_legend(
                buf,
                area.width - small_files_len - 1,
                status_line_y,
                small_files_legend,
            );
        }

        Strip::new(self.keybinds, self.scanning, self.switch_panel_hint).render(
            buf,
            self.ticker,
            area.x + 1,
            controls_line_y,
            area.width.saturating_sub(2),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::KeyBinding;
    use ratatui::crossterm::event::KeyCode;

    #[test]
    fn legend_names_the_bound_delete_key() {
        let legend = controls_legend(&Keybinds::default(), false, false);
        assert!(legend.contains("<d> - delete"), "{legend}");
        assert!(!legend.contains("BACKSPACE"), "{legend}");
    }

    #[test]
    fn legend_follows_a_rebound_delete_key() {
        let kb = Keybinds {
            delete: KeyBinding::key(KeyCode::Backspace),
            ..Keybinds::default()
        };
        let legend = controls_legend(&kb, false, false);
        assert!(legend.contains("<BACKSPACE> - delete"), "{legend}");
    }

    #[test]
    fn legend_names_the_rescan_keys_once_scanned() {
        let legend = controls_legend(&Keybinds::default(), false, false);
        assert!(legend.contains("<r/R> - rescan folder/all"), "{legend}");
        let legend = controls_legend(&Keybinds::default(), true, false);
        assert!(!legend.contains("rescan"), "{legend}");
    }

    #[test]
    fn legend_omits_delete_while_scanning() {
        let legend = controls_legend(&Keybinds::default(), true, false);
        assert!(!legend.contains("delete"), "{legend}");
        assert!(
            tips(&Keybinds::default(), true, true)
                .iter()
                .all(|tip| !tip.contains("delete") && !tip.contains("rescan"))
        );
    }

    #[test]
    fn legend_names_the_switch_panel_key_while_there_is_a_list() {
        let legend = controls_legend(&Keybinds::default(), false, true);
        assert!(legend.contains("<TAB> - list/map"), "{legend}");
        let legend = controls_legend(&Keybinds::default(), false, false);
        assert!(!legend.contains("TAB"), "{legend}");
    }

    fn line(strip: &Strip, ticker: Ticker, width: u16) -> String {
        let mut buf = Buffer::empty(Rect::new(0, 0, width, 1));
        strip.render(&mut buf, ticker, 0, 0, width);
        (0..width)
            .map(|x| buf[(x, 0)].symbol().to_string())
            .collect()
    }

    /// Drive `ticker` a frame at a time from `start` until it next comes to rest, with no key
    /// pressed. Returns when it did.
    fn next_stop(strip: &Strip, ticker: &mut Ticker, width: u16, start: Instant) -> Instant {
        let mut now = start;
        let quiet_since = start - REST;
        while !ticker.sliding() {
            strip.advance(ticker, width, quiet_since, now);
            now += FRAME;
        }
        let started = now - FRAME;
        while ticker.sliding() {
            strip.advance(ticker, width, quiet_since, now);
            now += FRAME;
        }
        assert!(now - FRAME - started <= Duration::from_millis(100));
        now - FRAME
    }

    #[test]
    fn ticker_rests_five_seconds_then_slides_to_the_first_tip_within_100_ms() {
        let strip = Strip::new(&Keybinds::default(), false, true);
        let mut ticker = Ticker::default();
        let start = Instant::now();
        let width = 300;
        assert!(line(&strip, ticker, width).starts_with("<arrows> - move around"));
        // Still, however often it is ticked, until five quiet seconds have passed.
        for tenth in 0..50 {
            let now = start + Duration::from_millis(tenth * 100);
            assert!(!strip.advance(&mut ticker, width, start, now));
            assert!(!ticker.sliding());
        }
        let mut now = start + REST;
        strip.advance(&mut ticker, width, start, now);
        assert!(ticker.sliding());
        let mut frames = vec![line(&strip, ticker, width)];
        while ticker.sliding() {
            now += FRAME;
            if strip.advance(&mut ticker, width, start, now) {
                frames.push(line(&strip, ticker, width));
            }
        }
        assert!(now - (start + REST) <= Duration::from_millis(100));
        assert!(frames.len() >= 4, "a slide, not a jump: {}", frames.len());
        assert!(frames.last().unwrap().starts_with("Tip: Ctrl+click"));
    }

    #[test]
    fn a_key_press_restarts_the_rest() {
        let strip = Strip::new(&Keybinds::default(), false, true);
        let mut ticker = Ticker::default();
        let start = Instant::now();
        let pressed = start + Duration::from_secs(4);
        strip.advance(&mut ticker, 300, pressed, start + Duration::from_secs(6));
        assert!(!ticker.sliding());
        strip.advance(&mut ticker, 300, pressed, pressed + REST);
        assert!(ticker.sliding());
    }

    #[test]
    fn a_segment_too_wide_is_shown_a_page_at_a_time() {
        let strip = Strip::new(&Keybinds::default(), false, true);
        let mut ticker = Ticker::default();
        let width = 40;
        let legend = strip.legend.clone();
        let mut now = Instant::now();
        let mut pages = vec![line(&strip, ticker, width)];
        while ticker.segment == 0 {
            now = next_stop(&strip, &mut ticker, width, now);
            pages.push(line(&strip, ticker, width));
        }
        // Every page before the tip is a window on the legend, and the last ends where it does.
        let (tip, pages) = pages.split_last().unwrap();
        assert!(tip.starts_with("Tip: "), "{tip}");
        assert!(pages.len() > 2);
        for page in pages {
            assert!(legend.contains(page.as_str()), "{page}");
        }
        assert!(legend.ends_with(pages.last().unwrap().as_str()));
    }

    #[test]
    fn ticker_shows_every_tip_between_legends_and_comes_round() {
        let strip = Strip::new(&Keybinds::default(), false, true);
        let mut ticker = Ticker::default();
        let mut now = Instant::now();
        let mut starts = Vec::new();
        loop {
            now = next_stop(&strip, &mut ticker, 300, now);
            starts.push(line(&strip, ticker, 12));
            if ticker.segment == 0 {
                break;
            }
        }
        assert_eq!(starts.len(), strip.tips.len() * 2, "{starts:?}");
        for (index, start) in starts.iter().enumerate() {
            let expected = if index % 2 == 0 {
                "Tip: "
            } else {
                "<arrows> - "
            };
            assert!(start.starts_with(expected), "{index}: {start}");
        }
    }

    #[test]
    fn a_legend_that_changes_mid_slide_still_lands_on_the_next_tip() {
        let scanning = Strip::new(&Keybinds::default(), true, true);
        let done = Strip::new(&Keybinds::default(), false, true);
        let mut ticker = Ticker::default();
        let start = Instant::now();
        // The whole legend fits, so its one slide heads for the first tip.
        scanning.advance(&mut ticker, 1000, start - REST, start);
        assert!(ticker.sliding());
        // The scan finishes mid-slide, and the legend grows.
        let mut now = start;
        while ticker.sliding() {
            now += FRAME;
            done.advance(&mut ticker, 1000, start - REST, now);
        }
        assert!(
            line(&done, ticker, 40).starts_with("Tip: "),
            "{}",
            line(&done, ticker, 40)
        );
    }

    #[test]
    fn a_legend_that_shrinks_under_the_ticker_does_not_jump_back() {
        // Resting past where the shorter legend's last page starts: it shows that page, not
        // the start again, and not the next segment.
        let short = Strip::new(&Keybinds::default(), true, false);
        let ticker = Ticker {
            column: 10_000,
            ..Ticker::default()
        };
        let shown = line(&short, ticker, 20);
        assert!(short.legend.ends_with(shown.as_str()), "{shown}");
    }
}
