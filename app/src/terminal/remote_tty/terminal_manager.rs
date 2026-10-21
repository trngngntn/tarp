use std::any::Any;
use std::sync::Arc;
use std::sync::mpsc::SyncSender;

use async_channel::{Receiver, Sender, TrySendError};
use parking_lot::FairMutex;
use pathfinder_geometry::vector::Vector2F;
use warpui::{AppContext, Entity, ModelHandle, ViewHandle, WindowId};

use crate::ai::blocklist::InputConfig;
use crate::context_chips::prompt_type::PromptType;
use crate::pane_group::TerminalViewResources;
use crate::persistence::ModelEvent;
use crate::terminal::event_listener::ChannelEventListener;
use crate::terminal::model::session::Sessions;
use crate::terminal::model_events::ModelEventDispatcher;
use crate::terminal::remote_tty::event_loop::EventLoop;
use crate::terminal::shell::{ShellName, ShellType};
use crate::terminal::terminal_manager::BlockSpacing;
use crate::terminal::writeable_pty::pty_controller::{EventLoopSendError, EventLoopSender};
use crate::terminal::writeable_pty::terminal_manager_util::{
    init_pty_controller_model, wire_up_pty_controller_with_surface,
};
use crate::terminal::writeable_pty::{self, Message};
use crate::terminal::{
    ShellLaunchState, SizeInfo, TerminalManager as TerminalManagerTrait, TerminalModel,
    TerminalView, terminal_manager,
};

pub(in crate::terminal) type PtyController = writeable_pty::PtyController<Sender<Message>>;

pub struct TerminalManager {
    model: Arc<FairMutex<TerminalModel>>,

    // Store a reference to the PTYController and EventLoop so the UI framework doesn't end up
    // deallocating them because there are no strong references to the models.
    _pty_controller: ModelHandle<PtyController>,

    _event_loop: ModelHandle<EventLoop>,

    _view: ViewHandle<TerminalView>,
}
#[cfg_attr(not(feature = "remote_tty"), allow(dead_code))]
pub struct TerminalManagerInit {
    pub(crate) manager: ModelHandle<Box<dyn TerminalManagerTrait>>,
    pub(crate) view: ViewHandle<TerminalView>,
}

impl TerminalManager {
    /// Creates a terminal manager model that feeds bytes to/from a remote PTY.
    pub fn create_model(
        resources: TerminalViewResources,
        initial_size: Vector2F,
        model_event_sender: Option<SyncSender<ModelEvent>>,
        window_id: WindowId,
        initial_input_config: Option<InputConfig>,
        ctx: &mut AppContext,
    ) -> TerminalManagerInit {
        let terminal = build_pty_backed_terminal(
            resources,
            initial_size,
            model_event_sender,
            window_id,
            initial_input_config,
            false, /* raw */
            ctx,
            Self::create_and_start_event_loop,
        );

        // Clone the view before moving it into the manager so the caller can
        // receive it alongside the boxed manager.
        let terminal_view = terminal.view.clone();

        // Create the terminal manager itself.
        let terminal_manager = Self {
            model: terminal.model,
            _view: terminal.view,
            _pty_controller: terminal.pty_controller,
            _event_loop: terminal.event_loop,
        };

        let manager_model = ctx.add_model(|_ctx| {
            let manager: Box<dyn TerminalManagerTrait> = Box::new(terminal_manager);
            manager
        });
        TerminalManagerInit {
            manager: manager_model,
            view: terminal_view,
        }
    }

    fn create_and_start_event_loop(
        terminal_model: Arc<FairMutex<TerminalModel>>,
        channel_event_listener: ChannelEventListener,
        message_receiver: Receiver<Message>,
        size_info: SizeInfo,
        ctx: &mut AppContext,
    ) -> ModelHandle<EventLoop> {
        ctx.add_model(|ctx| {
            EventLoop::start(
                terminal_model,
                message_receiver,
                channel_event_listener,
                size_info,
                ctx,
            )
        })
    }
}

/// A terminal model and view wired to a PTY controller whose bytes flow through an event loop of
/// type `E`.
pub(in crate::terminal) struct PtyBackedTerminal<E> {
    pub model: Arc<FairMutex<TerminalModel>>,
    pub view: ViewHandle<TerminalView>,
    pub pty_controller: ModelHandle<PtyController>,
    pub event_loop: ModelHandle<E>,
}

/// Builds a terminal whose PTY lives elsewhere. With `raw`, the model skips bootstrap and passes
/// input straight through.
#[allow(clippy::too_many_arguments)]
pub(in crate::terminal) fn build_pty_backed_terminal<E: Entity>(
    resources: TerminalViewResources,
    initial_size: Vector2F,
    model_event_sender: Option<SyncSender<ModelEvent>>,
    window_id: WindowId,
    initial_input_config: Option<InputConfig>,
    raw: bool,
    ctx: &mut AppContext,
    start_event_loop: impl FnOnce(
        Arc<FairMutex<TerminalModel>>,
        ChannelEventListener,
        Receiver<Message>,
        SizeInfo,
        &mut AppContext,
    ) -> ModelHandle<E>,
) -> PtyBackedTerminal<E> {
    // Create all the necessary channels we need for communication.
    let (wakeups_tx, wakeups_rx) = async_channel::unbounded();
    let (events_tx, events_rx) = async_channel::unbounded();
    let (executor_command_tx, executor_command_rx) = async_channel::unbounded();

    // Use an empty pty reads broadcaster since we don't need to broadcast any PTY bytes for the
    // network-backed PTY. We use 1 instead of 0 here because `async_broadcast` internally
    // asserts that the capacity is at least 1.
    let (pty_reads_tx, _pty_reads_rx) = async_broadcast::broadcast(1);

    let channel_event_proxy = ChannelEventListener::new(wakeups_tx, events_tx, pty_reads_tx);

    // Initialize the sessions model.
    let sessions: ModelHandle<Sessions> =
        ctx.add_model(|ctx| Sessions::new(executor_command_tx, ctx));

    let model_events =
        ctx.add_model(|ctx| ModelEventDispatcher::new(events_rx, sessions.clone(), ctx));

    // Create the terminal model.
    let mut model = terminal_manager::create_terminal_model(
        None, /* startup_directory */
        None, /* restored_blocks */
        initial_size,
        channel_event_proxy.clone(),
        // TODO(alokedesai): Add support for other shells within the network-backed pty.
        ShellLaunchState::ShellSpawned {
            available_shell: None,
            display_name: ShellName::blank(),
            shell_type: ShellType::Zsh,
        },
        BlockSpacing::for_gui(ctx),
        ctx,
    );
    if raw {
        model.enter_raw_mode();
    }

    let size_info = *model.block_list().size();
    let colors = model.colors();
    let model = Arc::new(FairMutex::new(model));

    let (event_loop_tx, event_loop_rx) = async_channel::unbounded();

    let event_loop = start_event_loop(
        model.clone(),
        channel_event_proxy.clone(),
        event_loop_rx,
        size_info,
        ctx,
    );

    // Initialize the PtyController.
    let pty_controller = init_pty_controller_model(
        event_loop_tx.clone(),
        executor_command_rx,
        model_events.clone(),
        sessions.clone(),
        model.clone(),
        ctx,
    );

    let cloned_model = model.clone();
    let prompt_type =
        ctx.add_model(|ctx| PromptType::new_dynamic_from_sessions(sessions.clone(), ctx));
    let view = ctx.add_typed_action_view(window_id, |ctx| {
        TerminalView::new(
            resources,
            wakeups_rx,
            model_events.clone(),
            cloned_model,
            sessions.clone(),
            size_info,
            colors,
            model_event_sender.clone(),
            prompt_type,
            initial_input_config,
            None, // conversation_restoration - not used for remote
            None, // inactive_pty_reads_rx
            false,
            ctx,
        )
    });

    wire_up_pty_controller_with_surface(
        &pty_controller,
        &view,
        model.clone(),
        sessions,
        model_event_sender,
        ctx,
    );

    PtyBackedTerminal {
        model,
        view,
        pty_controller,
        event_loop,
    }
}

impl super::super::TerminalManager for TerminalManager {
    fn model(&self) -> Arc<FairMutex<TerminalModel>> {
        self.model.clone()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl EventLoopSender for Sender<Message> {
    fn send(&self, message: Message) -> Result<(), EventLoopSendError> {
        self.try_send(message).map_err(|err| match err {
            TrySendError::Closed(_) => EventLoopSendError::Disconnected,
            TrySendError::Full(_) => EventLoopSendError::Other(err.into()),
        })
    }
}
