use std::fmt::Write as _;

use crate::ids::{PaneId, WindowId};
use crate::restore::STATE_FORMAT;

pub const MIN_VERSION: (u32, u32) = (3, 4);
pub const SEND_KEYS_CHUNK: usize = 1000;
pub const PAUSE_AFTER_SECS: u32 = 5;
/// Number of replies `restore_pane` produces, one per `;`-separated command.
pub const RESTORE_REPLIES: usize = 6;

const WINDOW_FORMAT: &str = "#{window_id} #{pane_id} #{window_name}";

/// Quotes `arg` for the tmux command parser.
pub fn quote(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', r"'\''"))
}

pub fn version_query() -> String {
    "display-message -p '#{version}'".to_owned()
}

/// Extracts `(major, minor)` from a `#{version}` string such as `3.4a` or `next-3.6`.
pub fn parse_version(version: &str) -> Option<(u32, u32)> {
    let version = version.strip_prefix("next-").unwrap_or(version);
    let (major, minor) = version.split_once('.')?;
    let minor_digits = minor.len() - minor.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    Some((major.parse().ok()?, minor[..minor_digits].parse().ok()?))
}

pub fn send_keys(pane: PaneId, bytes: &[u8]) -> Vec<String> {
    bytes
        .chunks(SEND_KEYS_CHUNK)
        .map(|chunk| {
            let mut line = format!("send-keys -t {pane} -H");
            line.reserve(chunk.len() * 3);
            for byte in chunk {
                let _ = write!(line, " {byte:02x}");
            }
            line
        })
        .collect()
}

pub fn resize_window(window: WindowId, columns: usize, rows: usize) -> String {
    format!("refresh-client -C {window}:{columns}x{rows}")
}

pub fn pause_after(seconds: u32) -> String {
    format!("refresh-client -f pause-after={seconds}")
}

pub fn pause_pane(pane: PaneId) -> String {
    format!("refresh-client -A '{pane}:pause'")
}

pub fn continue_pane(pane: PaneId) -> String {
    format!("refresh-client -A '{pane}:continue'")
}

pub fn detach_client() -> String {
    "detach-client".to_owned()
}

pub fn list_windows() -> String {
    format!("list-windows -F '{WINDOW_FORMAT}'")
}

pub fn describe_window(window: WindowId) -> String {
    format!("display-message -p -t {window} '{WINDOW_FORMAT}'")
}

#[derive(Clone, Debug, PartialEq)]
pub struct WindowInfo {
    pub window: WindowId,
    pub pane: PaneId,
    pub name: String,
}

pub fn parse_window_line(line: &[u8]) -> Option<WindowInfo> {
    let line = String::from_utf8_lossy(line);
    let mut fields = line.splitn(3, ' ');
    Some(WindowInfo {
        window: fields.next()?.parse().ok()?,
        pane: fields.next()?.parse().ok()?,
        name: fields.next()?.to_owned(),
    })
}

/// Pauses `pane`, captures its state, history, screen and saved normal screen, then resumes it.
/// Produces `RESTORE_REPLIES` replies.
pub fn restore_pane(pane: PaneId) -> String {
    format!(
        "{} ; display-message -p -t {pane} '{STATE_FORMAT}' ; capture-pane -peqJ -t {pane} -S - -E -1 \
         ; capture-pane -peq -t {pane} ; capture-pane -peq -a -t {pane} ; {}",
        pause_pane(pane),
        continue_pane(pane),
    )
}

#[cfg(test)]
#[path = "commands_tests.rs"]
mod tests;
