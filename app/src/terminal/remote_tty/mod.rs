mod event_loop;
mod terminal_manager;

pub use terminal_manager::TerminalManager;
pub(in crate::terminal) use terminal_manager::{PtyController, build_pty_backed_terminal};
