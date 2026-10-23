use super::*;

fn parse(fixture: &[u8]) -> Vec<ControlEvent> {
    let mut parser = ControlParser::new();
    let body = fixture.strip_suffix(b"\n").unwrap();
    body.split(|&b| b == b'\n')
        .filter_map(|line| parser.push_line(line))
        .collect()
}

fn ok(time: u64, number: u64, from_client: bool, body: &[&[u8]]) -> ControlEvent {
    ControlEvent::Reply(CommandReply {
        time,
        number,
        from_client,
        success: true,
        body: body.iter().map(|line| line.to_vec()).collect(),
    })
}

#[test]
fn session_events_transcript() {
    use Notification::*;
    let n = ControlEvent::Notification;
    assert_eq!(
        parse(include_bytes!("testdata/session_events.txt")),
        vec![
            ok(1792572895, 315, false, &[]),
            n(SessionChanged {
                session: SessionId(0),
                name: "demo".into()
            }),
            ok(1792572895, 317, true, &[]),
            n(LayoutChange {
                window: WindowId(0),
                layout: "285d,40x5,0,0[40x2,0,0,0,40x2,0,3,1]".into()
            }),
            n(Output {
                pane: PaneId(0),
                data: b"h\xc3\xa9\\\r\n".to_vec()
            }),
            n(WindowRenamed {
                window: WindowId(0),
                name: "zsh".into()
            }),
            ok(1792572896, 318, true, &[]),
            n(WindowPaneChanged {
                window: WindowId(0),
                pane: PaneId(1)
            }),
            ok(1792572896, 319, true, &[]),
            n(UnlinkedWindowAdd(WindowId(1))),
            n(SessionsChanged),
            n(UnlinkedWindowRenamed {
                window: WindowId(1),
                name: "tmux".into()
            }),
            n(WindowRenamed {
                window: WindowId(0),
                name: "sleep".into()
            }),
            ok(1792572896, 320, true, &[]),
            n(SessionRenamed {
                session: SessionId(0),
                name: "demo2".into()
            }),
            ControlEvent::Reply(CommandReply {
                time: 1792572896,
                number: 321,
                from_client: true,
                success: false,
                body: vec![b"parse error: unknown command: bogus-command".to_vec()]
            }),
            n(UnlinkedWindowRenamed {
                window: WindowId(1),
                name: "zsh".into()
            }),
            ok(1792572897, 322, true, &[]),
            n(Exit { reason: None }),
        ]
    );
}

#[test]
fn extended_output_transcript() {
    use Notification::*;
    let n = ControlEvent::Notification;
    assert_eq!(
        parse(include_bytes!("testdata/extended_output.txt")),
        vec![
            ok(1792572885, 315, false, &[]),
            n(SessionChanged { session: SessionId(0), name: "demo".into() }),
            ok(1792572886, 317, true, &[]),
            ok(1792572886, 318, true, &[]),
            n(WindowAdd(WindowId(1))),
            n(Output {
                pane: PaneId(0),
                data: b"h\xc3\xa9\\\r\n\x1bPtmux;\x1b\x1b]777;notify;warp://cli-agent;{\"x\":1}\x07\x1b\\"
                    .to_vec()
            }),
            n(WindowRenamed { window: WindowId(0), name: "zsh".into() }),
            n(WindowClose(WindowId(1))),
            n(Exit { reason: None }),
        ]
    );
}

#[test]
fn capture_reply_transcript() {
    use Notification::*;
    let n = ControlEvent::Notification;
    // Reply bodies are raw and keep empty lines.
    assert_eq!(
        parse(include_bytes!("testdata/capture_reply.txt")),
        vec![
            ok(1792572957, 315, false, &[]),
            n(SessionChanged {
                session: SessionId(0),
                name: "demo".into()
            }),
            ok(1792572957, 317, true, &[&b"ab"[..], b"cd", b""]),
            ok(1792572957, 318, true, &[&b""[..]]),
            ok(1792572957, 319, true, &[&b"2 1 0"[..]]),
            n(WindowRenamed {
                window: WindowId(0),
                name: "sleep".into()
            }),
            ok(1792572958, 320, true, &[]),
            n(LayoutChange {
                window: WindowId(0),
                layout: "e9dd,30x4,0,0,0".into()
            }),
            ok(1792572958, 321, true, &[&b"30 4"[..]]),
            n(Exit { reason: None }),
        ]
    );
}

#[test]
fn body_lines_that_look_like_notifications_stay_body() {
    let mut parser = ControlParser::new();
    for line in [&b"%begin 1 5 1"[..], b"%1", b"%output %1 x", b"%end 1 4 1"] {
        assert_eq!(parser.push_line(line), None);
    }
    assert_eq!(
        parser.push_line(b"%end 1 5 1"),
        Some(ControlEvent::Reply(CommandReply {
            time: 1,
            number: 5,
            from_client: true,
            success: true,
            body: vec![
                b"%1".to_vec(),
                b"%output %1 x".to_vec(),
                b"%end 1 4 1".to_vec()
            ],
        }))
    );
}

#[test]
fn small_notifications() {
    let mut p = ControlParser::new();
    let n = |e| Some(ControlEvent::Notification(e));
    assert_eq!(
        p.push_line(b"%exit detached"),
        n(Notification::Exit {
            reason: Some("detached".into())
        })
    );
    assert_eq!(p.push_line(b"%pause %3"), n(Notification::Pause(PaneId(3))));
    assert_eq!(
        p.push_line(b"%continue %3"),
        n(Notification::Continue(PaneId(3)))
    );
    assert_eq!(
        p.push_line(b"%window-renamed @2 two words"),
        n(Notification::WindowRenamed {
            window: WindowId(2),
            name: "two words".into()
        })
    );
    assert_eq!(
        p.push_line(b"%window-add oops"),
        n(Notification::Other("%window-add oops".into()))
    );
    assert_eq!(
        p.push_line(b"%client-detached /dev/x"),
        n(Notification::Other("%client-detached /dev/x".into()))
    );
}
