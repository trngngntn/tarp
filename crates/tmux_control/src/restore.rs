pub const STATE_FORMAT: &str = "#{cursor_x} #{cursor_y} #{alternate_on} #{alternate_saved_x} \
    #{alternate_saved_y} #{history_size} #{pane_height} #{bracket_paste_flag} \
    #{keypad_cursor_flag} #{keypad_flag} #{mouse_standard_flag} #{mouse_button_flag} \
    #{mouse_any_flag} #{mouse_sgr_flag} #{cursor_flag} #{scroll_region_upper} \
    #{scroll_region_lower}";

const FIELD_COUNT: usize = 17;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PaneState {
    pub cursor_x: usize,
    pub cursor_y: usize,
    pub alternate_on: bool,
    pub alternate_saved_x: usize,
    pub alternate_saved_y: usize,
    pub history_size: usize,
    pub pane_height: usize,
    pub bracketed_paste: bool,
    pub keypad_cursor: bool,
    pub keypad: bool,
    pub mouse_standard: bool,
    pub mouse_button: bool,
    pub mouse_any: bool,
    pub mouse_sgr: bool,
    pub cursor_visible: bool,
    pub scroll_region_upper: usize,
    pub scroll_region_lower: usize,
}

impl PaneState {
    /// Parses a line expanded from `STATE_FORMAT`. Empty fields, which tmux emits for formats it
    /// doesn't know, read as 0/false.
    pub fn parse(line: &[u8]) -> Option<Self> {
        let line = std::str::from_utf8(line).ok()?;
        let fields = line
            .split(' ')
            .map(|field| {
                if field.is_empty() {
                    Some(0)
                } else {
                    field.parse().ok()
                }
            })
            .collect::<Option<Vec<usize>>>()?;
        let [
            cursor_x,
            cursor_y,
            alternate_on,
            alternate_saved_x,
            alternate_saved_y,
            history_size,
            pane_height,
            bracketed_paste,
            keypad_cursor,
            keypad,
            mouse_standard,
            mouse_button,
            mouse_any,
            mouse_sgr,
            cursor_visible,
            scroll_region_upper,
            scroll_region_lower,
        ] = <[usize; FIELD_COUNT]>::try_from(fields).ok()?;
        Some(Self {
            cursor_x,
            cursor_y,
            alternate_on: alternate_on != 0,
            alternate_saved_x,
            alternate_saved_y,
            history_size,
            pane_height,
            bracketed_paste: bracketed_paste != 0,
            keypad_cursor: keypad_cursor != 0,
            keypad: keypad != 0,
            mouse_standard: mouse_standard != 0,
            mouse_button: mouse_button != 0,
            mouse_any: mouse_any != 0,
            mouse_sgr: mouse_sgr != 0,
            cursor_visible: cursor_visible != 0,
            scroll_region_upper,
            scroll_region_lower,
        })
    }
}

pub struct PaneCapture {
    pub state: PaneState,
    pub history: Vec<Vec<u8>>,
    pub visible: Vec<Vec<u8>>,
    pub saved_normal: Vec<Vec<u8>>,
}

fn push_lines(out: &mut Vec<u8>, lines: &[&Vec<u8>]) {
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(line);
    }
}

/// Builds the bytes that, fed to a fresh terminal, reproduce the captured pane.
pub fn restore_bytes(capture: &PaneCapture) -> Vec<u8> {
    let state = &capture.state;
    let mut out = b"\x1b[H\x1b[2J\x1b[3J".to_vec();

    let history = if state.history_size > 0 {
        &capture.history[..]
    } else {
        &[]
    };
    let screen = if state.alternate_on {
        &capture.saved_normal
    } else {
        &capture.visible
    };
    push_lines(&mut out, &history.iter().chain(screen).collect::<Vec<_>>());

    if state.alternate_on {
        out.extend_from_slice(
            format!(
                "\x1b[{};{}H",
                state.alternate_saved_y + 1,
                state.alternate_saved_x + 1
            )
            .as_bytes(),
        );
        out.extend_from_slice(b"\x1b[?1049h\x1b[H");
        push_lines(&mut out, &capture.visible.iter().collect::<Vec<_>>());
    }

    let modes: [(bool, &[u8]); 7] = [
        (state.bracketed_paste, b"\x1b[?2004h"),
        (state.keypad_cursor, b"\x1b[?1h"),
        (state.keypad, b"\x1b="),
        (state.mouse_standard, b"\x1b[?1000h"),
        (state.mouse_button, b"\x1b[?1002h"),
        (state.mouse_any, b"\x1b[?1003h"),
        (state.mouse_sgr, b"\x1b[?1006h"),
    ];
    for (enabled, sequence) in modes {
        if enabled {
            out.extend_from_slice(sequence);
        }
    }
    if !state.cursor_visible {
        out.extend_from_slice(b"\x1b[?25l");
    }

    if state.scroll_region_upper != 0 || state.scroll_region_lower + 1 != state.pane_height {
        out.extend_from_slice(
            format!(
                "\x1b[{};{}r",
                state.scroll_region_upper + 1,
                state.scroll_region_lower + 1
            )
            .as_bytes(),
        );
    }
    out.extend_from_slice(
        format!("\x1b[{};{}H", state.cursor_y + 1, state.cursor_x + 1).as_bytes(),
    );
    out
}

#[cfg(test)]
#[path = "restore_tests.rs"]
mod tests;
