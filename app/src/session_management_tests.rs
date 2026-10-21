use super::*;
use crate::pane_group::TerminalPaneId;

fn navigation_data(is_raw: bool) -> SessionNavigationData {
    SessionNavigationData::new(
        String::new(),
        SessionNavigationPromptElements {
            ps1_prompt_grid: None,
            prompt_chip_snapshot: None,
        },
        CommandContext::RunningCommand {
            running_command: "tmux".to_owned(),
        },
        PaneViewLocator {
            pane_group_id: EntityId::new(),
            pane_id: TerminalPaneId::dummy_terminal_pane_id().into(),
        },
        None,
        false,
        is_raw,
        WindowId::new(),
        SharedSessionStatus::NotShared,
    )
}

#[test]
fn running_summary_skips_raw_sessions() {
    let sessions = [navigation_data(true), navigation_data(false)];
    assert_eq!(
        RunningSessionSummary::new(&sessions)
            .long_running_cmds
            .len(),
        1
    );
}
