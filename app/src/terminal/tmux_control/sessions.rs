use tmux_control::{SessionId, WindowId as TmuxWindowId};
use warpui::{Entity, ModelContext, SingletonEntity};

/// Identifies one attached tmux session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TmuxSessionKey {
    pub session: SessionId,
}

/// The app's tmux control-mode connections.
#[derive(Default)]
pub struct TmuxControlSessions;

impl TmuxControlSessions {
    /// Records that no tab shows `window` anymore.
    pub fn release_window(
        &mut self,
        _key: TmuxSessionKey,
        _window: TmuxWindowId,
        _ctx: &mut ModelContext<Self>,
    ) {
    }
}

impl Entity for TmuxControlSessions {
    type Event = ();
}

impl SingletonEntity for TmuxControlSessions {}
