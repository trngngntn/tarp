use std::any::Any;
use std::cell::Cell;
use std::sync::Arc;
use std::sync::mpsc::SyncSender;

use parking_lot::FairMutex;
use pathfinder_geometry::vector::Vector2F;
use tmux_control::connection::Connection;
use tmux_control::{PaneId, WindowId as TmuxWindowId};
use warpui::{AppContext, ModelHandle, SingletonEntity, ViewHandle, WindowId};

use super::event_loop::TmuxEventLoop;
use super::sessions::{TmuxControlSessions, TmuxSessionKey};
use crate::pane_group::TerminalViewResources;
use crate::pane_group::pane::DetachType;
use crate::persistence::ModelEvent;
use crate::terminal::remote_tty::{PtyController, build_pty_backed_terminal};
use crate::terminal::{TerminalManager, TerminalModel, TerminalView};

/// The tmux pane a tab shows.
#[derive(Clone)]
pub struct TmuxPaneBinding {
    pub connection: Connection,
    pub key: TmuxSessionKey,
    pub window: TmuxWindowId,
    pub pane: PaneId,
    /// Whether the tab is activated when created.
    pub visible: bool,
}

/// Backs a tab with a tmux window's pane.
pub struct TmuxTerminalManager {
    model: Arc<FairMutex<TerminalModel>>,
    binding: TmuxPaneBinding,
    /// Set once the pane is released, so a later close can't release a window re-attached since.
    released: Cell<bool>,

    // Hold strong references so the UI framework doesn't deallocate these models.
    _pty_controller: ModelHandle<PtyController>,
    _event_loop: ModelHandle<TmuxEventLoop>,
    _view: ViewHandle<TerminalView>,
}

impl TmuxTerminalManager {
    pub fn create_model(
        binding: TmuxPaneBinding,
        resources: TerminalViewResources,
        initial_size: Vector2F,
        model_event_sender: Option<SyncSender<ModelEvent>>,
        window_id: WindowId,
        ctx: &mut AppContext,
    ) -> (
        ViewHandle<TerminalView>,
        ModelHandle<Box<dyn TerminalManager>>,
    ) {
        let event_loop_binding = binding.clone();
        let terminal = build_pty_backed_terminal(
            resources,
            initial_size,
            model_event_sender,
            window_id,
            None, /* initial_input_config */
            true, /* raw */
            ctx,
            move |model, listener, messages, size_info, ctx| {
                ctx.add_model(|ctx| {
                    TmuxEventLoop::new(
                        model,
                        listener,
                        messages,
                        size_info,
                        event_loop_binding,
                        ctx,
                    )
                })
            },
        );
        let view = terminal.view.clone();
        let manager = Self {
            model: terminal.model,
            binding,
            released: Cell::new(false),
            _pty_controller: terminal.pty_controller,
            _event_loop: terminal.event_loop,
            _view: terminal.view,
        };
        let manager = ctx.add_model(|_| Box::new(manager) as Box<dyn TerminalManager>);
        (view, manager)
    }
}

impl TerminalManager for TmuxTerminalManager {
    fn model(&self) -> Arc<FairMutex<TerminalModel>> {
        self.model.clone()
    }

    fn on_view_detached(&self, detach_type: DetachType, app: &mut AppContext) {
        match detach_type {
            DetachType::Closed => {
                if self.released.replace(true) {
                    return;
                }
                let TmuxPaneBinding {
                    connection,
                    key,
                    window,
                    pane,
                    ..
                } = &self.binding;
                connection.unsubscribe_pane(*pane);
                TmuxControlSessions::handle(app).update(app, |sessions, ctx| {
                    sessions.release_window(*key, *window, ctx)
                });
            }
            DetachType::HiddenForClose | DetachType::Moved => {}
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
