use ::std::sync::mpsc;
use ::std::thread;
use ::std::time::Duration;

use super::{Event, Instruction, handle_events};

#[test]
fn path_error_flashes_title_then_resets() {
    let (event_tx, event_rx) = mpsc::sync_channel(1);
    let (instruction_tx, instruction_rx) = mpsc::sync_channel(16);

    let worker = thread::spawn(move || handle_events(event_rx, instruction_tx));

    event_tx.send(Event::PathError).expect("send path error");
    thread::sleep(Duration::from_millis(600));

    let instructions: Vec<_> = instruction_rx.try_iter().collect();
    assert!(
        instructions
            .iter()
            .any(|i| matches!(i, Instruction::SetPathToRed)),
        "expected SetPathToRed among {} instructions",
        instructions.len()
    );
    assert!(
        instructions
            .iter()
            .any(|i| matches!(i, Instruction::ResetCurrentPathColor)),
        "expected ResetCurrentPathColor among {} instructions",
        instructions.len()
    );

    event_tx.send(Event::AppExit).expect("send exit");
    worker.join().expect("join event handler");
}
