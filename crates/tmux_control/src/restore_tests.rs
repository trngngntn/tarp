use super::*;

const NORMAL_STATE: &[u8] = b"2 1 0 0 0 0 3 0 0 0 0 0 0 0 1 0 2 5";

/// Every restore starts from the normal screen with default modes, whatever the terminal showed.
const RESET: &[u8] = b"\x1b[?1049l\x1b[0m\x1b[r\x1b[?25h\x1b[?1000l\x1b[?1002l\x1b[?1003l\
    \x1b[?1006l\x1b[?2004l\x1b[?1l\x1b>\x1b[H\x1b[2J\x1b[3J";

fn after_reset(rest: &[u8]) -> Vec<u8> {
    [RESET, rest].concat()
}

#[test]
fn state_format_and_parse() {
    assert_eq!(
        STATE_FORMAT,
        "#{cursor_x} #{cursor_y} #{alternate_on} #{alternate_saved_x} \
        #{alternate_saved_y} #{history_size} #{pane_height} #{bracket_paste_flag} \
        #{keypad_cursor_flag} #{keypad_flag} #{mouse_standard_flag} #{mouse_button_flag} \
        #{mouse_any_flag} #{mouse_sgr_flag} #{cursor_flag} #{scroll_region_upper} \
        #{scroll_region_lower} #{pane_width}"
    );
    let state = PaneState::parse(NORMAL_STATE).unwrap();
    assert_eq!(
        (
            state.cursor_x,
            state.cursor_y,
            state.pane_height,
            state.cursor_visible
        ),
        (2, 1, 3, true)
    );
    assert_eq!(
        state.size(),
        PaneSize {
            columns: 5,
            rows: 3
        }
    );
    // An unknown format (older tmux) expands to empty and reads as 0/false.
    assert!(
        !PaneState::parse(b"2 1 0 0 0 0 3  0 0 0 0 0 0 1 0 2 5")
            .unwrap()
            .bracketed_paste
    );
    assert_eq!(PaneState::parse(b"2 1 0"), None);
}

#[test]
fn normal_screen_restore() {
    let capture = PaneCapture {
        state: PaneState::parse(NORMAL_STATE).unwrap(),
        history: vec![b"x".to_vec()], // ignored: history_size is 0
        visible: vec![b"ab".to_vec(), b"cd".to_vec(), b"".to_vec()],
        saved_normal: vec![],
    };
    assert_eq!(
        restore_bytes(&capture),
        after_reset(b"ab\r\ncd\r\n\x1b[2;3H")
    );
}

#[test]
fn alternate_screen_restore() {
    let capture = PaneCapture {
        state: PaneState::parse(b"3 0 1 2 2 1 3 0 0 0 0 0 0 0 1 0 2 5").unwrap(),
        history: vec![b"n1".to_vec()],
        visible: vec![b"ALT".to_vec(), b"".to_vec(), b"".to_vec()],
        saved_normal: vec![b"n2".to_vec(), b"n3".to_vec(), b"n4".to_vec()],
    };
    assert_eq!(
        restore_bytes(&capture),
        after_reset(b"n1\r\nn2\r\nn3\r\nn4\x1b[3;3H\x1b[?1049h\x1b[HALT\r\n\r\n\x1b[1;4H")
    );
}

#[test]
fn modes_and_scroll_region_are_restored() {
    let capture = PaneCapture {
        state: PaneState::parse(b"0 0 0 0 0 0 3 1 0 0 0 0 1 1 0 1 2 5").unwrap(),
        history: vec![],
        visible: vec![b"".to_vec(); 3],
        saved_normal: vec![],
    };
    assert_eq!(
        restore_bytes(&capture),
        after_reset(b"\r\n\r\n\x1b[?2004h\x1b[?1003h\x1b[?1006h\x1b[?25l\x1b[2;3r\x1b[1;1H")
    );
}
