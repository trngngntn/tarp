use std::path::Path;

/// The app does not inherit a login shell's PATH, so look in the usual install locations.
pub fn tmux_binary() -> &'static str {
    ["/opt/homebrew/bin/tmux", "/usr/local/bin/tmux"]
        .into_iter()
        .find(|path| Path::new(path).exists())
        .unwrap_or("tmux")
}
