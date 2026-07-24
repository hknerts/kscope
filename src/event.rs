//! Terminal input plumbing.
//!
//! crossterm's reader is blocking, so it lives on a dedicated blocking thread
//! and forwards events into the async world through a channel. This keeps the
//! tokio worker threads free for network I/O.

use crossterm::event::{Event, KeyEventKind};
use tokio::sync::mpsc::{channel, Receiver};

/// Start the input thread and return the receiving end.
pub fn spawn() -> Receiver<Event> {
    let (tx, rx) = channel(256);
    std::thread::Builder::new()
        .name("kscope-input".into())
        .spawn(move || loop {
            match crossterm::event::read() {
                Ok(event) => {
                    // Windows terminals emit both Press and Release; ignore the
                    // latter so every key does not fire twice.
                    if let Event::Key(k) = &event {
                        if k.kind == KeyEventKind::Release {
                            continue;
                        }
                    }
                    if tx.blocking_send(event).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
        })
        .expect("spawning input thread");
    rx
}
