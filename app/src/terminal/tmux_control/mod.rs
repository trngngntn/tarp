use settings::Setting;
use warpui::{AppContext, SingletonEntity};

use crate::features::FeatureFlag;
use crate::workspace::tab_settings::TabSettings;

pub fn tmux_control_mode_enabled(ctx: &AppContext) -> bool {
    FeatureFlag::TmuxControlMode.is_enabled() && *TabSettings::as_ref(ctx).tmux_control_mode.value()
}
