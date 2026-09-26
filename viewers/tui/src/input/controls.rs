use ::ratatui::backend::Backend;
use ratatui::crossterm::event::Event;
use ratatui::crossterm::event::read;
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use crate::App;
use crate::app::ListJump;
use crate::config::Keybinds;
use libduscape::FileToDelete;

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

/// The keys that move the list by more than a row. Fixed, like the arrow keys beside the
/// configurable movement keys; they do nothing while the treemap has the keyboard.
fn list_jump(evt: &Event) -> Option<ListJump> {
    let Event::Key(KeyEvent {
        code, modifiers, ..
    }) = evt
    else {
        return None;
    };
    if !modifiers.is_empty() {
        return None;
    }
    match code {
        KeyCode::PageUp => Some(ListJump::PageUp),
        KeyCode::PageDown => Some(ListJump::PageDown),
        KeyCode::Home => Some(ListJump::Home),
        KeyCode::End => Some(ListJump::End),
        _ => None,
    }
}

/// Shift+Down (1) or Shift+Up (-1), which extend a selection in the list.
fn shift_arrow(evt: &Event) -> Option<isize> {
    match evt {
        Event::Key(KeyEvent {
            code: KeyCode::Down,
            modifiers: KeyModifiers::SHIFT,
            ..
        }) => Some(1),
        Event::Key(KeyEvent {
            code: KeyCode::Up,
            modifiers: KeyModifiers::SHIFT,
            ..
        }) => Some(-1),
        _ => None,
    }
}

/// Act on a mouse press in a mode that shows the board. Returns whether `evt` was one, so the
/// caller can stop there.
fn handle_mouse<B: Backend>(evt: &Event, app: &mut App<B>) -> bool {
    let Event::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers,
    }) = *evt
    else {
        return false;
    };
    match kind {
        MouseEventKind::Down(MouseButton::Left) if modifiers.contains(KeyModifiers::CONTROL) => {
            app.ctrl_click(column, row);
        }
        MouseEventKind::Down(button) => app.click(button, column, row),
        _ => {}
    }
    true
}

pub fn handle_keypress_loading_mode<B: Backend>(evt: Event, app: &mut App<B>) {
    if handle_mouse(&evt, app) {
        return;
    }
    if let Some(delta) = shift_arrow(&evt) {
        app.extend_selection(delta);
        return;
    }
    if let Some(jump) = list_jump(&evt) {
        app.jump_list(jump);
        return;
    }
    let kb = &app.keybinds;
    if kb.is_quit(&evt) {
        app.prompt_exit();
    } else if kb.switch_panel.matches_event(&evt) {
        app.switch_focus();
    } else if kb.delete.matches_event(&evt) {
        app.show_warning_modal();
    } else if kb.toggle_size.matches_event(&evt) {
        app.toggle_size();
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
    if let Some(delta) = shift_arrow(&evt) {
        app.extend_selection(delta);
        return;
    }
    if let Some(jump) = list_jump(&evt) {
        app.jump_list(jump);
        return;
    }
    let kb = &app.keybinds;
    if kb.is_quit(&evt) {
        app.prompt_exit();
    } else if kb.switch_panel.matches_event(&evt) {
        app.switch_focus();
    } else if kb.delete.matches_event(&evt) {
        app.prompt_file_deletion();
    } else if kb.rescan.matches_event(&evt) {
        app.rescan_selected();
    } else if kb.rescan_all.matches_event(&evt) {
        app.rescan_all();
    } else if kb.toggle_size.matches_event(&evt) {
        app.toggle_size();
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
    files: Vec<FileToDelete>,
) {
    let kb = &app.keybinds;
    if kb.is_quit(&evt) || kb.is_cancel(&evt) {
        app.normal_mode();
    } else if kb.is_confirm(&evt) {
        app.delete_files(&files);
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
