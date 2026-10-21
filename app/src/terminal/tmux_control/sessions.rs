use std::collections::{HashMap, HashSet};

use tmux_control::commands::{self, WindowInfo};
use tmux_control::connection::{
    CommandError, CommandResult, Connection, ConnectionConfig, ConnectionEvent, SpawnConfig,
};
use tmux_control::parser::Notification;
use tmux_control::{SessionId, WindowId as TmuxWindowId, tmux_binary};
use warpui::{Entity, ModelContext, SingletonEntity, WindowId};

/// Identifies one attached tmux session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TmuxSessionKey {
    pub session: SessionId,
}

/// Where to attach; `None` fields let tmux choose.
pub struct AttachTarget {
    pub socket_name: Option<String>,
    pub session: Option<String>,
}

/// `owner_window` is the Workspace window that asked for the attach.
#[derive(Clone)]
pub enum TmuxControlEvent {
    Attached {
        key: TmuxSessionKey,
        owner_window: WindowId,
        session_name: String,
        connection: Connection,
        windows: Vec<WindowInfo>,
    },
    WindowAdded {
        key: TmuxSessionKey,
        owner_window: WindowId,
        connection: Connection,
        window: WindowInfo,
    },
    WindowClosed {
        key: TmuxSessionKey,
        window: TmuxWindowId,
    },
    WindowRenamed {
        key: TmuxSessionKey,
        window: TmuxWindowId,
        name: String,
    },
    SessionRenamed {
        key: TmuxSessionKey,
        name: String,
    },
    Detached {
        key: TmuxSessionKey,
        reason: Option<String>,
    },
    AttachFailed {
        owner_window: WindowId,
        message: String,
    },
}

enum LinkState {
    /// Not yet attached; `failed` once an `AttachFailed` was emitted for it.
    Pending {
        failed: bool,
    },
    Attached(TmuxSessionKey),
    /// Its session was already attached through another connection, which it left to.
    Superseded,
}

/// One tmux connection, from spawn until it closes.
struct Link {
    owner_window: WindowId,
    state: LinkState,
}

struct AttachedSession {
    link: u64,
    connection: Connection,
    owner_window: WindowId,
    name: String,
    open_windows: HashSet<TmuxWindowId>,
}

/// The app's tmux control-mode connections: one per attached tmux session.
#[derive(Default)]
pub struct TmuxControlSessions {
    links: HashMap<u64, Link>,
    sessions: HashMap<TmuxSessionKey, AttachedSession>,
    next_link: u64,
}

impl TmuxControlSessions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn attach(
        &mut self,
        target: AttachTarget,
        owner_window: WindowId,
        ctx: &mut ModelContext<Self>,
    ) {
        let config = SpawnConfig {
            binary: tmux_binary().into(),
            socket_name: target.socket_name,
            target: target.session,
            connection: ConnectionConfig::default(),
        };
        match Connection::spawn(config) {
            Ok((connection, events)) => {
                self.attach_with_connection(connection, events, owner_window, ctx)
            }
            Err(err) => ctx.emit(TmuxControlEvent::AttachFailed {
                owner_window,
                message: err.to_string(),
            }),
        }
    }

    /// Records that no tab shows `window` anymore, and detaches once no tab shows the session.
    pub fn release_window(
        &mut self,
        key: TmuxSessionKey,
        window: TmuxWindowId,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(session) = self.sessions.get_mut(&key) else {
            return;
        };
        if session.open_windows.remove(&window) && session.open_windows.is_empty() {
            // Dropped now so a new attach doesn't reuse a connection that is going away.
            session.connection.detach();
            self.sessions.remove(&key);
            ctx.emit(TmuxControlEvent::Detached { key, reason: None });
        }
    }

    fn attach_with_connection(
        &mut self,
        connection: Connection,
        events: async_channel::Receiver<ConnectionEvent>,
        owner_window: WindowId,
        ctx: &mut ModelContext<Self>,
    ) {
        let id = self.next_link;
        self.next_link += 1;
        self.links.insert(
            id,
            Link {
                owner_window,
                state: LinkState::Pending { failed: false },
            },
        );
        ctx.spawn_stream_local(
            events,
            move |me, event, ctx| me.handle_event(id, &connection, event, ctx),
            move |me, ctx| me.handle_closed(id, None, ctx),
        );
    }

    fn handle_event(
        &mut self,
        id: u64,
        connection: &Connection,
        event: ConnectionEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        match event {
            ConnectionEvent::Ready {
                session,
                session_name,
                ..
            } => {
                let key = TmuxSessionKey { session };
                if self.sessions.contains_key(&key) {
                    self.supersede(id, connection, key, ctx);
                } else {
                    let connection = connection.clone();
                    let windows = list_windows(&connection);
                    ctx.spawn(windows, move |me, windows, ctx| {
                        me.finish_attach(id, key, session_name, connection, windows, ctx)
                    });
                }
            }
            ConnectionEvent::Notification(notification) => {
                self.handle_notification(id, notification, ctx)
            }
            ConnectionEvent::Failed(err) => self.fail(id, err.to_string(), ctx),
            ConnectionEvent::Closed { reason } => self.handle_closed(id, reason, ctx),
        }
    }

    /// Leaves `connection` and shows the session's windows again through the connection that
    /// already owns it.
    fn supersede(
        &mut self,
        id: u64,
        connection: &Connection,
        key: TmuxSessionKey,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(link) = self.links.get_mut(&id) else {
            return;
        };
        link.state = LinkState::Superseded;
        let owner_window = link.owner_window;
        connection.detach();
        let Some(session) = self.sessions.get(&key) else {
            return;
        };
        let windows = list_windows(&session.connection);
        ctx.spawn(windows, move |me, windows, ctx| {
            me.finish_relist(key, owner_window, windows, ctx)
        });
    }

    fn finish_attach(
        &mut self,
        id: u64,
        key: TmuxSessionKey,
        session_name: String,
        connection: Connection,
        windows: Result<Vec<WindowInfo>, String>,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(link) = self.links.get(&id) else {
            return;
        };
        if matches!(link.state, LinkState::Pending { failed: true }) {
            return;
        }
        if self.sessions.contains_key(&key) {
            self.supersede(id, &connection, key, ctx);
            return;
        }
        let owner_window = link.owner_window;
        let windows = match windows {
            Ok(windows) => windows,
            Err(message) => {
                connection.detach();
                self.fail(id, message, ctx);
                return;
            }
        };
        if let Some(link) = self.links.get_mut(&id) {
            link.state = LinkState::Attached(key);
        }
        self.sessions.insert(
            key,
            AttachedSession {
                link: id,
                connection: connection.clone(),
                owner_window,
                name: session_name.clone(),
                open_windows: open_windows(&windows),
            },
        );
        ctx.emit(TmuxControlEvent::Attached {
            key,
            owner_window,
            session_name,
            connection,
            windows,
        });
    }

    fn finish_relist(
        &mut self,
        key: TmuxSessionKey,
        owner_window: WindowId,
        windows: Result<Vec<WindowInfo>, String>,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(session) = self.sessions.get_mut(&key) else {
            return;
        };
        match windows {
            Ok(windows) => {
                session.owner_window = owner_window;
                session.open_windows = open_windows(&windows);
                ctx.emit(TmuxControlEvent::Attached {
                    key,
                    owner_window,
                    session_name: session.name.clone(),
                    connection: session.connection.clone(),
                    windows,
                });
            }
            Err(message) => ctx.emit(TmuxControlEvent::AttachFailed {
                owner_window,
                message,
            }),
        }
    }

    fn handle_notification(
        &mut self,
        id: u64,
        notification: Notification,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(LinkState::Attached(key)) = self.links.get(&id).map(|link| &link.state) else {
            return;
        };
        let key = *key;
        let Some(session) = self.sessions.get_mut(&key) else {
            return;
        };
        match notification {
            Notification::WindowAdd(window) => {
                let describe = describe_window(&session.connection, window);
                ctx.spawn(describe, move |me, windows, ctx| {
                    if let Ok(windows) = windows
                        && let Some(window) = windows.into_iter().next()
                    {
                        me.add_window(key, window, ctx);
                    }
                });
            }
            Notification::WindowClose(window) => {
                session.open_windows.remove(&window);
                ctx.emit(TmuxControlEvent::WindowClosed { key, window });
            }
            Notification::WindowRenamed { window, name } => {
                ctx.emit(TmuxControlEvent::WindowRenamed { key, window, name });
            }
            Notification::SessionRenamed { name, .. } => {
                session.name = name.clone();
                ctx.emit(TmuxControlEvent::SessionRenamed { key, name });
            }
            Notification::Output { .. }
            | Notification::UnlinkedWindowAdd(_)
            | Notification::UnlinkedWindowClose(_)
            | Notification::UnlinkedWindowRenamed { .. }
            | Notification::LayoutChange { .. }
            | Notification::WindowPaneChanged { .. }
            | Notification::SessionChanged { .. }
            | Notification::SessionsChanged
            | Notification::Pause(_)
            | Notification::Continue(_)
            | Notification::Exit { .. }
            | Notification::Other(_) => {}
        }
    }

    fn add_window(
        &mut self,
        key: TmuxSessionKey,
        window: WindowInfo,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(session) = self.sessions.get_mut(&key) else {
            return;
        };
        session.open_windows.insert(window.window);
        ctx.emit(TmuxControlEvent::WindowAdded {
            key,
            owner_window: session.owner_window,
            connection: session.connection.clone(),
            window,
        });
    }

    fn fail(&mut self, id: u64, message: String, ctx: &mut ModelContext<Self>) {
        let Some(link) = self.links.get_mut(&id) else {
            return;
        };
        if let LinkState::Pending { failed } = &mut link.state
            && !*failed
        {
            *failed = true;
            ctx.emit(TmuxControlEvent::AttachFailed {
                owner_window: link.owner_window,
                message,
            });
        }
    }

    fn handle_closed(&mut self, id: u64, reason: Option<String>, ctx: &mut ModelContext<Self>) {
        let Some(link) = self.links.remove(&id) else {
            return;
        };
        match link.state {
            LinkState::Pending { failed: false } => ctx.emit(TmuxControlEvent::AttachFailed {
                owner_window: link.owner_window,
                message: reason.unwrap_or_else(|| "tmux exited".to_owned()),
            }),
            LinkState::Pending { failed: true } | LinkState::Superseded => {}
            LinkState::Attached(key) => {
                if self
                    .sessions
                    .get(&key)
                    .is_some_and(|session| session.link == id)
                {
                    self.sessions.remove(&key);
                    ctx.emit(TmuxControlEvent::Detached { key, reason });
                }
            }
        }
    }
}

fn open_windows(windows: &[WindowInfo]) -> HashSet<TmuxWindowId> {
    windows.iter().map(|info| info.window).collect()
}

fn list_windows(
    connection: &Connection,
) -> impl Future<Output = Result<Vec<WindowInfo>, String>> + use<> {
    window_lines(connection.command(commands::list_windows()))
}

fn describe_window(
    connection: &Connection,
    window: TmuxWindowId,
) -> impl Future<Output = Result<Vec<WindowInfo>, String>> + use<> {
    window_lines(connection.command(commands::describe_window(window)))
}

async fn window_lines(
    reply: async_channel::Receiver<CommandResult>,
) -> Result<Vec<WindowInfo>, String> {
    let lines = reply
        .recv()
        .await
        .map_err(|_| CommandError::Disconnected.to_string())?
        .map_err(|err| err.to_string())?;
    Ok(lines
        .iter()
        .filter_map(|line| commands::parse_window_line(line))
        .collect())
}

impl Entity for TmuxControlSessions {
    type Event = TmuxControlEvent;
}

impl SingletonEntity for TmuxControlSessions {}

#[cfg(test)]
#[path = "sessions_tests.rs"]
mod tests;
