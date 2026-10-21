//! Decoding of the bytes tmux delivers in `%output` notifications.

/// Decodes tmux's `%output` escaping: `\NNN` (three octal digits) becomes one byte, and any
/// other byte, including a lone `\`, is kept as is.
pub fn unescape_output(data: &[u8]) -> Vec<u8> {
    let is_octal = |byte: u8| (b'0'..=b'7').contains(&byte);
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        match data[i..] {
            [b'\\', a, b, c, ..] if is_octal(a) && is_octal(b) && is_octal(c) => {
                // Three octal digits can exceed 0xFF; tmux never emits those, so truncate.
                out.push(((a - b'0') << 6) | ((b - b'0') << 3) | (c - b'0'));
                i += 4;
            }
            _ => {
                out.push(data[i]);
                i += 1;
            }
        }
    }
    out
}

const PREFIX: &[u8] = b"\x1bPtmux;";
const ESC: u8 = 0x1b;

#[derive(Debug, Default)]
enum State {
    #[default]
    Normal,
    /// The first `n` bytes of `PREFIX` have been seen and withheld.
    Prefix(usize),
    Inside,
    InsideEsc,
}

/// Strips tmux's DCS passthrough wrapper (`ESC P tmux; … ESC \`), un-doubling the `ESC`s inside,
/// so the wrapped escape sequences reach the terminal model as if printed directly. Stateful so
/// a wrapper can span chunks.
#[derive(Debug, Default)]
pub struct PassthroughUnwrapper {
    state: State,
}

impl PassthroughUnwrapper {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, input: &[u8], out: &mut Vec<u8>) {
        for &byte in input {
            self.push_byte(byte, out);
        }
    }

    fn push_byte(&mut self, byte: u8, out: &mut Vec<u8>) {
        match self.state {
            State::Normal => self.push_normal(byte, out),
            State::Prefix(held) if PREFIX[held] == byte => {
                self.state = if held + 1 == PREFIX.len() {
                    State::Inside
                } else {
                    State::Prefix(held + 1)
                };
            }
            State::Prefix(held) => {
                out.extend_from_slice(&PREFIX[..held]);
                self.push_normal(byte, out);
            }
            State::Inside if byte == ESC => self.state = State::InsideEsc,
            State::Inside => out.push(byte),
            State::InsideEsc => match byte {
                ESC => {
                    out.push(ESC);
                    self.state = State::Inside;
                }
                b'\\' => self.state = State::Normal,
                _ => {
                    out.extend_from_slice(&[ESC, byte]);
                    self.state = State::Inside;
                }
            },
        }
    }

    fn push_normal(&mut self, byte: u8, out: &mut Vec<u8>) {
        if byte == ESC {
            self.state = State::Prefix(1);
        } else {
            self.state = State::Normal;
            out.push(byte);
        }
    }
}

#[cfg(test)]
#[path = "output_tests.rs"]
mod tests;
