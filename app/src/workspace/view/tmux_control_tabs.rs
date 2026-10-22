use std::collections::HashSet;
use std::mem;

use tmux_control::WindowId as TmuxWindowId;
use tmux_control::commands::WindowInfo;
use tmux_control::connection::Connection;
use warp_core::features::FeatureFlag;
use warpui::{EntityId, SingletonEntity as _, ViewContext, WindowId};

use super::Workspace;
use super::tab_grouping::tmux_agents;
use super::vertical_tabs::tab_terminal_shell_pids;
use crate::pane_group::PaneGroup;
use crate::tab::TabData;
use crate::terminal::tmux_control::{
    AttachTarget, TmuxControlEvent, TmuxControlSessions, TmuxPaneBinding, TmuxSessionKey,
    tmux_control_mode_enabled,
};
use crate::view_components::DismissibleToast;
use crate::workspace::tab_group::TabGroupId;

impl Workspace {
    /// Attaches the tmux session the active tab's shell is a client of, or else the session tmux
    /// picks (its most recently used one).
    pub(super) fn attach_tmux_session(&mut self, ctx: &mut ViewContext<Self>) {
        if !tmux_control_mode_enabled(ctx) {
            return;
        }
        let shell_pids: Vec<u32> =
            tab_terminal_shell_pids(self.active_tab_pane_group().as_ref(ctx), ctx)
                .into_iter()
                .map(|(_, pid)| pid)
                .collect();
        let owner_window = self.window_id;
        if shell_pids.is_empty() {
            Self::request_tmux_attach(None, owner_window, ctx);
            return;
        }
        ctx.spawn(
            async move {
                tokio::task::spawn_blocking(move || {
                    let mut sessions = tmux_agents::tmux_client_sessions();
                    shell_pids
                        .iter()
                        .find_map(|pid| sessions.remove(pid))
                        .map(|session| session.id)
                })
                .await
            },
            move |_, session, ctx| {
                Self::request_tmux_attach(session.ok().flatten(), owner_window, ctx)
            },
        );
    }

    fn request_tmux_attach(
        session: Option<String>,
        owner_window: WindowId,
        ctx: &mut ViewContext<Self>,
    ) {
        let target = AttachTarget {
            socket_name: None,
            session,
        };
        TmuxControlSessions::handle(ctx).update(ctx, |sessions, ctx| {
            sessions.attach(target, owner_window, ctx)
        });
    }

    pub(super) fn handle_tmux_control_event(
        &mut self,
        event: &TmuxControlEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        match event {
            TmuxControlEvent::Attached {
                key,
                owner_window,
                session_name,
                connection,
                windows,
            } => {
                if *owner_window != self.window_id {
                    return;
                }
                let mut activate = true;
                for window in windows {
                    if self.tmux_window_tab_index(*key, window.window).is_some() {
                        continue;
                    }
                    let group = self
                        .tab_groups
                        .values()
                        .find(|group| group.name.as_deref() == Some(session_name.as_str()))
                        .map(|group| group.id);
                    let index = self.insert_index_in_group(group);
                    let index =
                        self.open_tmux_window_tab(*key, window, connection, index, activate, ctx);
                    activate = false;
                    if FeatureFlag::GroupedTabs.is_enabled() {
                        self.put_tab_in_named_group(index, session_name);
                    }
                }
            }
            TmuxControlEvent::WindowAdded {
                key,
                owner_window,
                connection,
                window,
            } => {
                if *owner_window != self.window_id
                    || self.tmux_window_tab_index(*key, window.window).is_some()
                {
                    return;
                }
                let group = self.tmux_session_group(*key);
                let index = self.insert_index_in_group(group);
                let index = self.open_tmux_window_tab(*key, window, connection, index, false, ctx);
                if FeatureFlag::GroupedTabs.is_enabled()
                    && let Some(name) = group
                        .and_then(|group| self.tab_groups.get(&group))
                        .and_then(|group| group.name.clone())
                {
                    self.put_tab_in_named_group(index, &name);
                }
            }
            TmuxControlEvent::WindowClosed { key, window } => {
                if let Some(index) = self.tmux_window_tab_index(*key, *window) {
                    self.close_tab(index, true, false, ctx);
                }
                self.tmux_window_tabs.remove(&(*key, *window));
            }
            TmuxControlEvent::WindowRenamed { key, window, name } => {
                if let Some(index) = self.tmux_window_tab_index(*key, *window) {
                    self.tabs[index].pane_group.update(ctx, |pane_group, ctx| {
                        pane_group.set_custom_title(Some(name.clone()), ctx)
                    });
                }
            }
            TmuxControlEvent::SessionRenamed { key, name } => {
                if let Some(group) = self
                    .tmux_session_group(*key)
                    .and_then(|group| self.tab_groups.get_mut(&group))
                {
                    group.name = Some(name.clone());
                    ctx.notify();
                }
            }
            TmuxControlEvent::Detached { key, .. } => {
                let windows: Vec<TmuxWindowId> = self
                    .tmux_window_tabs
                    .keys()
                    .filter(|(tab_key, _)| tab_key == key)
                    .map(|(_, window)| *window)
                    .collect();
                for window in windows {
                    if let Some(index) = self.tmux_window_tab_index(*key, window) {
                        self.close_tab(index, true, false, ctx);
                    }
                    self.tmux_window_tabs.remove(&(*key, window));
                }
            }
            TmuxControlEvent::AttachFailed {
                owner_window,
                message,
            } => {
                if *owner_window != self.window_id {
                    return;
                }
                self.toast_stack.update(ctx, |toast_stack, ctx| {
                    toast_stack.add_ephemeral_toast(DismissibleToast::error(message.clone()), ctx);
                });
            }
        }
    }

    /// Releases every tmux window this workspace shows, as its window goes away. Undoing the
    /// close doesn't bring them back; re-attaching does.
    pub(super) fn release_tmux_tabs(&mut self, ctx: &mut ViewContext<Self>) {
        self.unshown_tmux_tabs.clear();
        let tmux_tabs: HashSet<EntityId> =
            self.tmux_window_tabs.drain().map(|(_, id)| id).collect();
        let pane_groups: Vec<_> = self
            .tabs
            .iter()
            .filter(|tab| tmux_tabs.contains(&tab.pane_group.id()))
            .map(|tab| tab.pane_group.clone())
            .collect();
        for pane_group in pane_groups {
            self.released_tmux_tabs.insert(pane_group.id());
            pane_group.update(ctx, |pane_group, ctx| pane_group.clean_up_panes(ctx));
        }
    }

    /// Removes the tabs `release_tmux_tabs` released, as the window's close is undone, keeping the
    /// active tab if it remains and opening a terminal tab if none does.
    pub(super) fn drop_released_tmux_tabs(&mut self, ctx: &mut ViewContext<Self>) {
        let released = mem::take(&mut self.released_tmux_tabs);
        if released.is_empty() {
            return;
        }
        let active = self
            .tabs
            .get(self.active_tab_index)
            .map(|tab| tab.pane_group.id());
        let mut groups = Vec::new();
        self.tabs.retain(|tab| {
            let keep = !released.contains(&tab.pane_group.id());
            if !keep {
                groups.extend(tab.group_id);
            }
            keep
        });
        self.tab_mru_order.retain(|id| !released.contains(id));
        for group in groups {
            self.prune_empty_tab_group(group, ctx);
        }
        if self.tabs.is_empty() {
            self.active_tab_index = 0;
            self.add_terminal_tab(false, ctx);
            return;
        }
        let index = self
            .tabs
            .iter()
            .position(|tab| Some(tab.pane_group.id()) == active)
            .unwrap_or_else(|| self.active_tab_index.min(self.tabs.len() - 1));
        self.activate_tab_internal(index, ctx);
        ctx.notify();
    }

    pub(super) fn is_tmux_window_tab(&self, pane_group_id: EntityId) -> bool {
        self.tmux_window_tabs
            .values()
            .any(|id| *id == pane_group_id)
    }

    /// Restores a tmux tab created in the background the first time it is shown. A resize to an
    /// unchanged size reaches no one, so this one is forced.
    pub(super) fn restore_tmux_tab_on_first_show(
        &mut self,
        pane_group_id: EntityId,
        ctx: &mut ViewContext<Self>,
    ) {
        if !self.unshown_tmux_tabs.remove(&pane_group_id) {
            return;
        }
        if let Some(tab) = self
            .tabs
            .iter()
            .find(|tab| tab.pane_group.id() == pane_group_id)
        {
            tab.pane_group.update(ctx, |pane_group, ctx| {
                pane_group.for_all_terminal_panes(
                    |terminal_view, ctx| terminal_view.refresh_size(ctx),
                    ctx,
                )
            });
        }
    }

    /// Builds the tab showing one tmux window, at `index` or at the end, and returns its index.
    fn open_tmux_window_tab(
        &mut self,
        key: TmuxSessionKey,
        window: &WindowInfo,
        connection: &Connection,
        index: Option<usize>,
        activate: bool,
        ctx: &mut ViewContext<Self>,
    ) -> usize {
        let binding = TmuxPaneBinding {
            connection: connection.clone(),
            key,
            window: window.window,
            pane: window.pane,
            visible: activate,
        };
        let pane_group = ctx.add_typed_action_view(|ctx| {
            PaneGroup::new_for_tmux_window(
                binding,
                self.tips_completed.clone(),
                self.user_default_shell_unsupported_banner_model_handle
                    .clone(),
                self.server_api.clone(),
                self.model_event_sender.clone(),
                ctx,
            )
        });
        ctx.subscribe_to_view(&pane_group, move |me, pane_group, event, ctx| {
            me.handle_file_tree_event(pane_group, event, ctx)
        });
        pane_group.update(ctx, |pane_group, ctx| {
            pane_group.set_custom_title(Some(window.name.clone()), ctx)
        });
        let pane_group_id = pane_group.id();
        let index = index.unwrap_or(self.tabs.len()).min(self.tabs.len());
        self.tabs.insert(index, TabData::new(pane_group));
        if index <= self.active_tab_index && self.tabs.len() > 1 {
            self.active_tab_index += 1;
        }
        self.tab_mru_order.push(pane_group_id);
        self.tmux_window_tabs
            .insert((key, window.window), pane_group_id);
        if activate {
            self.activate_tab_internal(index, ctx);
        } else {
            self.unshown_tmux_tabs.insert(pane_group_id);
        }
        ctx.notify();
        index
    }

    /// The index of the live tab showing `window`. Looked up by id, since a tab may have been
    /// closed or moved since it was recorded.
    fn tmux_window_tab_index(&self, key: TmuxSessionKey, window: TmuxWindowId) -> Option<usize> {
        let pane_group_id = self.tmux_window_tabs.get(&(key, window))?;
        self.tabs
            .iter()
            .position(|tab| tab.pane_group.id() == *pane_group_id)
    }

    /// The group holding the session's live tabs.
    fn tmux_session_group(&self, key: TmuxSessionKey) -> Option<TabGroupId> {
        self.tmux_window_tabs
            .iter()
            .filter(|((tab_key, _), _)| *tab_key == key)
            .filter_map(|(_, pane_group_id)| {
                self.tabs
                    .iter()
                    .find(|tab| tab.pane_group.id() == *pane_group_id)
            })
            .find_map(|tab| tab.group_id)
    }

    /// Where a new member of `group` goes to keep the group contiguous; `None` for the end.
    fn insert_index_in_group(&self, group: Option<TabGroupId>) -> Option<usize> {
        group.and_then(|group| self.index_after_group(group))
    }
}
