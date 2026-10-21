use std::time::Duration;

use tmux_control::commands::{self, WindowInfo};
use tmux_control::connection::{Connection, ConnectionConfig, ConnectionEvent};
use tmux_control::testing::FakeTmux;
use tmux_control::{PaneId, SessionId, WindowId as TmuxWindowId};
use warpui::elements::Empty;
use warpui::platform::WindowStyle;
use warpui::{App, AppContext, Element, Entity, ModelHandle, TypedActionView, View, WindowId};

use super::*;

const KEY: TmuxSessionKey = TmuxSessionKey {
    session: SessionId(0),
};
const W: Duration = Duration::from_millis(200);

fn info(window: u32, pane: u32, name: &str) -> WindowInfo {
    WindowInfo {
        window: TmuxWindowId(window),
        pane: PaneId(pane),
        name: name.to_owned(),
    }
}

struct OwnerView;

impl Entity for OwnerView {
    type Event = ();
}

impl View for OwnerView {
    fn ui_name() -> &'static str {
        "OwnerView"
    }

    fn render(&self, _: &AppContext) -> Box<dyn Element> {
        Empty::new().finish()
    }
}

impl TypedActionView for OwnerView {
    type Action = ();
}

struct Fixture {
    sessions: ModelHandle<TmuxControlSessions>,
    events: async_channel::Receiver<TmuxControlEvent>,
    owner: WindowId,
}

impl Fixture {
    fn new(app: &mut App) -> Self {
        let sessions = app.add_singleton_model(|_| TmuxControlSessions::new());
        let (sender, events) = async_channel::unbounded();
        app.update(|ctx| {
            ctx.subscribe_to_model(&sessions, move |_, event: &TmuxControlEvent, _| {
                let _ = sender.try_send(event.clone());
            });
        });
        let (owner, _) = app.add_window(WindowStyle::NotStealFocus, |_| OwnerView);
        Self {
            sessions,
            events,
            owner,
        }
    }

    fn connect(&self, app: &mut App) -> (FakeTmux, Connection) {
        let (tmux, connection, events) = FakeTmux::start(ConnectionConfig::default());
        self.attach(app, &connection, events);
        (tmux, connection)
    }

    fn attach(
        &self,
        app: &mut App,
        connection: &Connection,
        events: async_channel::Receiver<ConnectionEvent>,
    ) {
        let (connection, owner) = (connection.clone(), self.owner);
        self.sessions.update(app, |sessions, ctx| {
            sessions.attach_with_connection(connection, events, owner, ctx)
        });
    }

    async fn next_event(&self) -> TmuxControlEvent {
        for _ in 0..1000 {
            if let Ok(event) = self.events.try_recv() {
                return event;
            }
            warpui::r#async::Timer::after(Duration::from_millis(5)).await;
        }
        panic!("timed out waiting for a TmuxControlEvent");
    }

    /// Attaches a session with windows @0 and @1.
    async fn attached(&self, app: &mut App) -> (FakeTmux, Connection) {
        let (mut tmux, connection) = self.connect(app);
        tmux.handshake("demo", "3.8");
        read_command(&mut tmux).await;
        tmux.reply(&["@0 %0 zsh", "@1 %1 claude"]);
        assert!(matches!(
            self.next_event().await,
            TmuxControlEvent::Attached { .. }
        ));
        (tmux, connection)
    }
}

/// Reads without blocking the app's executor, which must run for the command to be written.
async fn read_command(tmux: &mut FakeTmux) -> String {
    for _ in 0..1000 {
        if let Some(line) = tmux.try_read_command(Duration::from_millis(1)) {
            return line;
        }
        warpui::r#async::Timer::after(Duration::from_millis(5)).await;
    }
    panic!("timed out waiting for a tmux command");
}

#[test]
fn attach_lists_windows_then_follows_add_rename_close_and_exit() {
    App::test((), |mut app| async move {
        let fixture = Fixture::new(&mut app);
        let (mut tmux, _connection) = fixture.connect(&mut app);
        tmux.handshake("demo", "3.8");
        assert_eq!(read_command(&mut tmux).await, commands::list_windows());
        tmux.reply(&["@0 %0 zsh", "@1 %1 claude"]);
        assert!(matches!(
            fixture.next_event().await,
            TmuxControlEvent::Attached { key: KEY, session_name, windows, owner_window, .. }
                if session_name == "demo"
                    && windows == vec![info(0, 0, "zsh"), info(1, 1, "claude")]
                    && owner_window == fixture.owner
        ));
        tmux.send("%window-add @2");
        assert_eq!(
            read_command(&mut tmux).await,
            commands::describe_window(TmuxWindowId(2))
        );
        tmux.reply(&["@2 %2 build"]);
        assert!(matches!(
            fixture.next_event().await,
            TmuxControlEvent::WindowAdded { window, .. } if window == info(2, 2, "build")
        ));
        tmux.send("%window-renamed @1 review");
        assert!(matches!(
            fixture.next_event().await,
            TmuxControlEvent::WindowRenamed { window: TmuxWindowId(1), name, .. }
                if name == "review"
        ));
        tmux.send("%session-renamed $0 work");
        assert!(matches!(
            fixture.next_event().await,
            TmuxControlEvent::SessionRenamed { name, .. } if name == "work"
        ));
        tmux.send("%window-close @0");
        assert!(matches!(
            fixture.next_event().await,
            TmuxControlEvent::WindowClosed {
                window: TmuxWindowId(0),
                ..
            }
        ));
        tmux.send("%exit");
        tmux.close();
        assert!(matches!(
            fixture.next_event().await,
            TmuxControlEvent::Detached {
                key: KEY,
                reason: None
            }
        ));
    });
}

#[test]
fn version_refusal_becomes_attach_failed() {
    App::test((), |mut app| async move {
        let fixture = Fixture::new(&mut app);
        let (mut tmux, _connection) = fixture.connect(&mut app);
        tmux.handshake("demo", "3.3");
        assert!(matches!(
            fixture.next_event().await,
            TmuxControlEvent::AttachFailed { message, .. }
                if message == "tmux 3.3 is too old; Tarp needs tmux 3.4 or newer"
        ));
    });
}

#[test]
fn closing_before_ready_reports_attach_failed() {
    App::test((), |mut app| async move {
        let fixture = Fixture::new(&mut app);
        let (mut tmux, _connection) = fixture.connect(&mut app);
        tmux.close();
        assert!(matches!(
            fixture.next_event().await,
            TmuxControlEvent::AttachFailed { message, .. } if message == "tmux exited"
        ));
    });
}

#[test]
fn releasing_the_last_window_detaches() {
    App::test((), |mut app| async move {
        let fixture = Fixture::new(&mut app);
        let (mut tmux, _connection) = fixture.attached(&mut app).await;
        let release = |app: &mut App, window| {
            fixture.sessions.update(app, |s, ctx| {
                s.release_window(KEY, TmuxWindowId(window), ctx)
            })
        };
        release(&mut app, 0);
        assert_eq!(tmux.try_read_command(W), None);
        release(&mut app, 1);
        assert_eq!(read_command(&mut tmux).await, "detach-client");
        release(&mut app, 9);
        assert_eq!(tmux.try_read_command(W), None);
    });
}

#[test]
fn attaching_an_attached_session_relists_on_the_existing_connection() {
    App::test((), |mut app| async move {
        let fixture = Fixture::new(&mut app);
        let (mut first, _first_connection) = fixture.attached(&mut app).await;
        let (mut second, _second_connection) = fixture.connect(&mut app);
        second.handshake("demo", "3.8");
        assert_eq!(read_command(&mut second).await, "detach-client");
        assert_eq!(read_command(&mut first).await, commands::list_windows());
        first.reply(&["@0 %0 zsh"]);
        assert!(matches!(
            fixture.next_event().await,
            TmuxControlEvent::Attached { windows, .. } if windows == vec![info(0, 0, "zsh")]
        ));
        // The superseded connection closing must not detach the session.
        second.close();
        warpui::r#async::Timer::after(W).await;
        assert!(fixture.events.is_empty());
    });
}

#[test]
fn tmux_closing_the_last_open_window_detaches() {
    App::test((), |mut app| async move {
        let fixture = Fixture::new(&mut app);
        let (mut tmux, _connection) = fixture.attached(&mut app).await;
        fixture.sessions.update(&mut app, |s, ctx| {
            s.release_window(KEY, TmuxWindowId(0), ctx)
        });
        tmux.send("%window-close @1");
        assert!(matches!(
            fixture.next_event().await,
            TmuxControlEvent::WindowClosed {
                window: TmuxWindowId(1),
                ..
            }
        ));
        assert_eq!(read_command(&mut tmux).await, "detach-client");
        assert!(matches!(
            fixture.next_event().await,
            TmuxControlEvent::Detached {
                key: KEY,
                reason: None
            }
        ));
    });
}

#[test]
fn attaching_from_another_window_is_refused() {
    App::test((), |mut app| async move {
        let mut fixture = Fixture::new(&mut app);
        let (mut first, _first_connection) = fixture.attached(&mut app).await;
        let first_owner = fixture.owner;
        let other = WindowId::new();
        fixture.owner = other;
        let (mut second, _second_connection) = fixture.connect(&mut app);
        second.handshake("demo", "3.8");
        assert_eq!(read_command(&mut second).await, "detach-client");
        assert!(matches!(
            fixture.next_event().await,
            TmuxControlEvent::AttachFailed { owner_window, message }
                if owner_window == other && message == "already open in another Tarp window"
        ));
        assert_eq!(first.try_read_command(W), None);
        // The first window still owns the session.
        first.send("%window-add @2");
        assert_eq!(
            read_command(&mut first).await,
            commands::describe_window(TmuxWindowId(2))
        );
        first.reply(&["@2 %2 build"]);
        assert!(matches!(
            fixture.next_event().await,
            TmuxControlEvent::WindowAdded { owner_window, .. } if owner_window == first_owner
        ));
    });
}

#[test]
fn attaching_after_the_owner_window_closed_takes_the_session_over() {
    App::test((), |mut app| async move {
        let mut fixture = Fixture::new(&mut app);
        let (mut first, _first_connection) = fixture.attached(&mut app).await;
        app.update(|ctx| ctx.simulate_window_closed(fixture.owner));
        let other = WindowId::new();
        fixture.owner = other;
        let (mut second, _second_connection) = fixture.connect(&mut app);
        second.handshake("demo", "3.8");
        assert_eq!(read_command(&mut second).await, "detach-client");
        assert_eq!(read_command(&mut first).await, commands::list_windows());
        first.reply(&["@0 %0 zsh"]);
        assert!(matches!(
            fixture.next_event().await,
            TmuxControlEvent::Attached { owner_window, .. } if owner_window == other
        ));
    });
}
