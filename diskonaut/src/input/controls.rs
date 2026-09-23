use ::ratatui::backend::Backend;
use ratatui::crossterm::event::Event;
use ratatui::crossterm::event::read;
use ratatui::crossterm::event::{KeyEvent, KeyEventKind, MouseEvent, MouseEventKind};

use crate::App;
use crate::config::Keybinds;
use libdiskonaut::FileToDelete;

#[derive(Clone)]
pub struct TerminalEvents;

impl Iterator for TerminalEvents {
    type Item = Event;
    fn next(&mut self) -> Option<Event> {
        loop {
            let event = read().unwrap();
            if !is_key_release(&event) && !is_mouse_noise(&event) {
                return Some(event);
            }
        }
    }
}

/// Whether `event` is a key being let go of.
///
/// Windows consoles report a release for every press, where Unix terminals report presses alone.
/// Every handler here acts on a press, so a release passed on would act twice: `q` would open the
/// quit prompt on the way down and answer it on the way up.
pub fn is_key_release(event: &Event) -> bool {
    matches!(
        event,
        Event::Key(KeyEvent {
            kind: KeyEventKind::Release,
            ..
        })
    )
}

/// Whether `event` is a mouse event no handler acts on: movement, drags, releases, scrolling.
///
/// Mouse capture reports every pointer movement, and some modals close on any event at all, so
/// passing these on would close a dialog when the mouse merely crossed the window — and flood the
/// rendering thread with events besides. Only presses get through.
pub fn is_mouse_noise(event: &Event) -> bool {
    matches!(event, Event::Mouse(mouse) if !matches!(mouse.kind, MouseEventKind::Down(_)))
}

/// Act on a mouse press in a mode that shows the board. Returns whether `evt` was one, so the
/// caller can stop there.
fn handle_mouse<B: Backend>(evt: &Event, app: &mut App<B>) -> bool {
    let Event::Mouse(MouseEvent {
        kind, column, row, ..
    }) = *evt
    else {
        return false;
    };
    if let MouseEventKind::Down(button) = kind {
        app.click(button, column, row);
    }
    true
}

pub fn handle_keypress_loading_mode<B: Backend>(evt: Event, app: &mut App<B>) {
    if handle_mouse(&evt, app) {
        return;
    }
    let kb = &app.keybinds;
    if kb.is_quit(&evt) {
        app.prompt_exit();
    } else if kb.delete.matches_event(&evt) {
        app.show_warning_modal();
    } else if kb.is_move_right(&evt) {
        app.move_selected_right();
    } else if kb.is_move_left(&evt) {
        app.move_selected_left();
    } else if kb.is_move_down(&evt) {
        app.move_selected_down();
    } else if kb.is_move_up(&evt) {
        app.move_selected_up();
    } else if kb.is_zoom_in(&evt) {
        app.zoom_in();
    } else if kb.zoom_out.matches_event(&evt) {
        app.zoom_out();
    } else if kb.reset_zoom.matches_event(&evt) {
        app.reset_zoom();
    } else if kb.is_enter(&evt) {
        app.handle_enter();
    } else if kb.parent.matches_event(&evt) {
        app.go_up();
    }
}

pub fn handle_keypress_normal_mode<B: Backend>(evt: Event, app: &mut App<B>) {
    if handle_mouse(&evt, app) {
        return;
    }
    let kb = &app.keybinds;
    if kb.is_quit(&evt) {
        app.prompt_exit();
    } else if kb.delete.matches_event(&evt) {
        app.prompt_file_deletion();
    } else if kb.is_move_right(&evt) {
        app.move_selected_right();
    } else if kb.is_move_left(&evt) {
        app.move_selected_left();
    } else if kb.is_move_down(&evt) {
        app.move_selected_down();
    } else if kb.is_move_up(&evt) {
        app.move_selected_up();
    } else if kb.is_zoom_in(&evt) {
        app.zoom_in();
    } else if kb.zoom_out.matches_event(&evt) {
        app.zoom_out();
    } else if kb.reset_zoom.matches_event(&evt) {
        app.reset_zoom();
    } else if kb.is_enter(&evt) {
        app.handle_enter();
    } else if kb.parent.matches_event(&evt) {
        app.go_up();
    }
}

pub fn handle_keypress_delete_file_mode<B: Backend>(
    evt: Event,
    app: &mut App<B>,
    file_to_delete: FileToDelete,
) {
    let kb = &app.keybinds;
    if kb.is_quit(&evt) || kb.is_cancel(&evt) {
        app.normal_mode();
    } else if kb.is_confirm(&evt) {
        app.delete_file(&file_to_delete);
    }
}

pub fn handle_keypress_error_message<B: Backend>(evt: Event, app: &mut App<B>) {
    let kb = &app.keybinds;
    if kb.is_quit(&evt) || kb.parent.matches_event(&evt) {
        app.normal_mode();
    }
}

pub fn handle_keypress_screen_too_small<B: Backend>(evt: Event, app: &mut App<B>) {
    if app.keybinds.is_quit(&evt) {
        app.exit();
    }
}

pub fn handle_keypress_exiting_mode<B: Backend>(evt: Event, app: &mut App<B>) {
    let kb = &app.keybinds;
    if kb.is_quit(&evt) || kb.is_cancel(&evt) {
        app.reset_ui_mode();
        // we have to manually call render here to make sure ui gets updated
        // because reset_ui_mode does not call it itself
        app.render();
    } else if kb.is_confirm(&evt) {
        app.exit();
    }
}

pub fn handle_keypress_warning_message<B: Backend>(_evt: Event, app: &mut App<B>) {
    app.reset_ui_mode();
}

/// Returns true when the stdin thread should pause briefly after handling (quit / confirm keys).
pub fn needs_quit_delay(evt: &Event, keybinds: &Keybinds) -> bool {
    keybinds.is_quit(evt) || keybinds.is_confirm(evt)
}
