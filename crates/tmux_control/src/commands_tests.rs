use super::*;

#[test]
fn quoting() {
    assert_eq!(quote("a b"), "'a b'");
    assert_eq!(quote("it's"), r"'it'\''s'");
}

#[test]
fn versions() {
    assert_eq!(parse_version("3.8"), Some((3, 8)));
    assert_eq!(parse_version("3.4a"), Some((3, 4)));
    assert_eq!(parse_version("3.10"), Some((3, 10)));
    assert_eq!(parse_version("next-3.6"), Some((3, 6)));
    assert_eq!(parse_version("openbsd-7.5"), None);
    assert_eq!(parse_version("master"), None);
    assert_eq!(version_query(), "display-message -p '#{version}'");
}

#[test]
fn send_keys_hex_in_chunks_of_1000() {
    assert_eq!(
        send_keys(PaneId(3), b"ab\x1b"),
        vec!["send-keys -t %3 -H 61 62 1b"]
    );
    let lines = send_keys(PaneId(3), &[b'a'; 2500]);
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[2].matches(" 61").count(), 500);
    assert!(send_keys(PaneId(3), b"").is_empty());
}

#[test]
fn fixed_commands() {
    assert_eq!(
        resize_window(WindowId(2), 120, 40),
        "refresh-client -C @2:120x40"
    );
    assert_eq!(
        pause_after(PAUSE_AFTER_SECS),
        "refresh-client -f pause-after=5"
    );
    assert_eq!(pause_pane(PaneId(3)), "refresh-client -A '%3:pause'");
    assert_eq!(continue_pane(PaneId(3)), "refresh-client -A '%3:continue'");
    assert_eq!(detach_client(), "detach-client");
    assert_eq!(
        list_windows(),
        "list-windows -F '#{window_id} #{pane_id} #{window_name}'"
    );
    assert_eq!(
        describe_window(WindowId(4)),
        "display-message -p -t @4 '#{window_id} #{pane_id} #{window_name}'"
    );
    let restore = restore_pane(PaneId(3));
    assert_eq!(
        restore,
        format!(
            "refresh-client -A '%3:pause' ; display-message -p -t %3 '{STATE_FORMAT}' ; \
             capture-pane -peqJ -t %3 -S - -E -1 ; capture-pane -peq -t %3 ; \
             capture-pane -peq -a -t %3 ; refresh-client -A '%3:continue'"
        )
    );
    assert_eq!(restore.matches(" ; ").count() + 1, RESTORE_REPLIES);
}

#[test]
fn window_lines() {
    assert_eq!(
        parse_window_line(b"@0 %0 sleep 1"),
        Some(WindowInfo {
            window: WindowId(0),
            pane: PaneId(0),
            name: "sleep 1".into()
        })
    );
    assert_eq!(parse_window_line(b"@0"), None);
}
