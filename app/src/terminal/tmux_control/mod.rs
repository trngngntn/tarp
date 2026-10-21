mod event_loop;
mod sessions;
mod terminal_manager;

use settings::Setting;
use warpui::{AppContext, SingletonEntity};

pub use sessions::{TmuxControlSessions, TmuxSessionKey};
pub use terminal_manager::{TmuxPaneBinding, TmuxTerminalManager};

use crate::features::FeatureFlag;
use crate::workspace::tab_settings::TabSettings;

pub fn tmux_control_mode_enabled(ctx: &AppContext) -> bool {
    FeatureFlag::TmuxControlMode.is_enabled() && *TabSettings::as_ref(ctx).tmux_control_mode.value()
}
