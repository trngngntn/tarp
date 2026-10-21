use crate::ids::{PaneId, SessionId, WindowId};
use crate::output::unescape_output;

#[derive(Clone, Debug, PartialEq)]
pub enum ControlEvent {
    Notification(Notification),
    Reply(CommandReply),
}

/// The `%begin` .. `%end`/`%error` block tmux sends for a command.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandReply {
    pub time: u64,
    pub number: u64,
    /// False for blocks tmux emitted on its own, which answer no command.
    pub from_client: bool,
    pub success: bool,
    /// Raw body lines, including empty ones.
    pub body: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Notification {
    /// Data is already unescaped.
    Output {
        pane: PaneId,
        data: Vec<u8>,
    },
    WindowAdd(WindowId),
    WindowClose(WindowId),
    WindowRenamed {
        window: WindowId,
        name: String,
    },
    UnlinkedWindowAdd(WindowId),
    UnlinkedWindowClose(WindowId),
    UnlinkedWindowRenamed {
        window: WindowId,
        name: String,
    },
    LayoutChange {
        window: WindowId,
        layout: String,
    },
    WindowPaneChanged {
        window: WindowId,
        pane: PaneId,
    },
    SessionChanged {
        session: SessionId,
        name: String,
    },
    SessionRenamed {
        session: SessionId,
        name: String,
    },
    SessionsChanged,
    Pause(PaneId),
    Continue(PaneId),
    Exit {
        reason: Option<String>,
    },
    /// Unknown or malformed notification, kept verbatim.
    Other(String),
}

struct Block {
    time: u64,
    number: u64,
    flags: u64,
    body: Vec<Vec<u8>>,
}

pub struct ControlParser {
    block: Option<Block>,
    ready: bool,
}

impl Default for ControlParser {
    fn default() -> Self {
        Self::new()
    }
}

impl ControlParser {
    pub fn new() -> Self {
        Self {
            block: None,
            ready: false,
        }
    }

    /// True once the reply tmux sends on attach has closed.
    pub fn is_ready(&self) -> bool {
        self.ready
    }

    /// Feeds one line without its trailing `\n`.
    pub fn push_line(&mut self, line: &[u8]) -> Option<ControlEvent> {
        let Some(block) = &mut self.block else {
            return self.start_line(line);
        };
        match parse_block_end(line) {
            Some((success, time, number, flags))
                if (time, number, flags) == (block.time, block.number, block.flags) =>
            {
                let block = self.block.take()?;
                self.ready = true;
                Some(ControlEvent::Reply(CommandReply {
                    time,
                    number,
                    from_client: flags & 1 != 0,
                    success,
                    body: block.body,
                }))
            }
            _ => {
                block.body.push(line.to_vec());
                None
            }
        }
    }

    fn start_line(&mut self, line: &[u8]) -> Option<ControlEvent> {
        if let Some(rest) = line.strip_prefix(b"%begin ")
            && let Some([time, number, flags]) = parse_numbers(rest)
        {
            self.block = Some(Block {
                time,
                number,
                flags,
                body: Vec::new(),
            });
            return None;
        }
        Some(ControlEvent::Notification(parse_notification(line)))
    }
}

fn parse_numbers<const N: usize>(rest: &[u8]) -> Option<[u64; N]> {
    let text = std::str::from_utf8(rest).ok()?;
    let mut fields = text.split(' ');
    let mut numbers = [0; N];
    for number in &mut numbers {
        *number = fields.next()?.parse().ok()?;
    }
    fields.next().is_none().then_some(numbers)
}

fn parse_block_end(line: &[u8]) -> Option<(bool, u64, u64, u64)> {
    let (success, rest) = match (line.strip_prefix(b"%end "), line.strip_prefix(b"%error ")) {
        (Some(rest), _) => (true, rest),
        (_, Some(rest)) => (false, rest),
        (None, None) => return None,
    };
    let [time, number, flags] = parse_numbers(rest)?;
    Some((success, time, number, flags))
}

fn parse_notification(line: &[u8]) -> Notification {
    parse_known(line).unwrap_or_else(|| Notification::Other(String::from_utf8_lossy(line).into()))
}

fn parse_known(line: &[u8]) -> Option<Notification> {
    let (name, rest) = match line.iter().position(|&b| b == b' ') {
        Some(at) => (&line[..at], Some(&line[at + 1..])),
        None => (line, None),
    };
    // Output data may be arbitrary bytes, so it is split before any UTF-8 decoding.
    match name {
        b"%output" => {
            let (pane, data) = split_once(rest?, b" ")?;
            return Some(Notification::Output {
                pane: lossy(pane).parse().ok()?,
                data: unescape_output(data),
            });
        }
        b"%extended-output" => {
            let (head, data) = split_once(rest?, b" : ")?;
            let pane = head.split(|&b| b == b' ').next()?;
            return Some(Notification::Output {
                pane: lossy(pane).parse().ok()?,
                data: unescape_output(data),
            });
        }
        _ => {}
    }

    let rest = rest.map(lossy);
    let args = rest.as_deref();
    let id_and_text = || {
        let (id, text) = args?.split_once(' ')?;
        Some((id, text.to_owned()))
    };
    Some(match name {
        b"%window-add" => Notification::WindowAdd(args?.parse().ok()?),
        b"%window-close" => Notification::WindowClose(args?.parse().ok()?),
        b"%window-renamed" => {
            let (window, name) = id_and_text()?;
            Notification::WindowRenamed {
                window: window.parse().ok()?,
                name,
            }
        }
        b"%unlinked-window-add" => Notification::UnlinkedWindowAdd(args?.parse().ok()?),
        b"%unlinked-window-close" => Notification::UnlinkedWindowClose(args?.parse().ok()?),
        b"%unlinked-window-renamed" => {
            let (window, name) = id_and_text()?;
            Notification::UnlinkedWindowRenamed {
                window: window.parse().ok()?,
                name,
            }
        }
        b"%layout-change" => {
            let mut fields = args?.split(' ');
            let window = fields.next()?.parse().ok()?;
            Notification::LayoutChange {
                window,
                layout: fields.next()?.to_owned(),
            }
        }
        b"%window-pane-changed" => {
            let (window, pane) = args?.split_once(' ')?;
            Notification::WindowPaneChanged {
                window: window.parse().ok()?,
                pane: pane.parse().ok()?,
            }
        }
        b"%session-changed" => {
            let (session, name) = id_and_text()?;
            Notification::SessionChanged {
                session: session.parse().ok()?,
                name,
            }
        }
        b"%session-renamed" => {
            let (session, name) = id_and_text()?;
            Notification::SessionRenamed {
                session: session.parse().ok()?,
                name,
            }
        }
        b"%sessions-changed" => Notification::SessionsChanged,
        b"%pause" => Notification::Pause(args?.parse().ok()?),
        b"%continue" => Notification::Continue(args?.parse().ok()?),
        b"%exit" => Notification::Exit {
            reason: args.map(str::to_owned),
        },
        _ => return None,
    })
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn split_once<'a>(haystack: &'a [u8], needle: &[u8]) -> Option<(&'a [u8], &'a [u8])> {
    let at = haystack
        .windows(needle.len())
        .position(|window| window == needle)?;
    Some((&haystack[..at], &haystack[at + needle.len()..]))
}

#[cfg(test)]
#[path = "parser_tests.rs"]
mod tests;
