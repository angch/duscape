use ::std::thread::park_timeout;
use ::std::time;

use crate::messages::Instruction;

pub enum Event {
    PathError,
    FileDeleted,
    /// Something was copied and is flashing in the title; redraw once it is due to go.
    ClipboardFlash(time::Duration),
    AppExit,
}

use std::sync::mpsc::{Receiver, SyncSender};

pub fn handle_events(event_receiver: Receiver<Event>, instruction_sender: SyncSender<Instruction>) {
    loop {
        let event = event_receiver
            .recv()
            .expect("failed to receive event on channel");
        match event {
            Event::PathError => {
                let _ = instruction_sender.send(Instruction::SetPathToRed);
                let _ = instruction_sender.send(Instruction::Render);
                park_timeout(time::Duration::from_millis(250));
                let _ = instruction_sender.send(Instruction::ResetCurrentPathColor);
                let _ = instruction_sender.send(Instruction::Render);
            }
            Event::FileDeleted => {
                let _ = instruction_sender.send(Instruction::FlashSpaceFreed);
                let _ = instruction_sender.send(Instruction::Render);
                park_timeout(time::Duration::from_millis(250));
                let _ = instruction_sender.send(Instruction::UnflashSpaceFreed);
                let _ = instruction_sender.send(Instruction::Render);
            }
            Event::ClipboardFlash(duration) => {
                // A thread of its own, so that this one is not held for the length of the flash
                // and the next event is not kept waiting. The flash expires by itself (see
                // `UiEffects::clipboard_flash`); this only makes sure a frame is drawn after it.
                let instruction_sender = instruction_sender.clone();
                let _ = ::std::thread::Builder::new()
                    .name("clipboard_flash".to_string())
                    .spawn(move || {
                        ::std::thread::sleep(duration);
                        let _ = instruction_sender.send(Instruction::Render);
                    });
            }
            Event::AppExit => {
                break;
            }
        }
    }
}
