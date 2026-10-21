use std::io;
use std::sync::Arc;

use async_channel::Receiver;
use parking_lot::FairMutex;
use tmux_control::connection::PaneEvent;
use tmux_control::output::PassthroughUnwrapper;
use warpui::{Entity, ModelContext};

use super::TmuxPaneBinding;
use crate::terminal::event_listener::ChannelEventListener;
use crate::terminal::model::ansi::Processor;
use crate::terminal::writeable_pty::Message;
use crate::terminal::{SizeInfo, TerminalModel};

/// Moves one tmux pane's output into a terminal model and the model's input and resizes back to
/// tmux.
pub(super) struct TmuxEventLoop;

impl TmuxEventLoop {
    pub(super) fn new(
        model: Arc<FairMutex<TerminalModel>>,
        listener: ChannelEventListener,
        messages: Receiver<Message>,
        size_info: SizeInfo,
        binding: TmuxPaneBinding,
        ctx: &mut ModelContext<Self>,
    ) -> Self {
        let events = binding.connection.subscribe_pane(binding.pane);
        // A restore must match the grid's size, which a background tab doesn't know until shown.
        if binding.visible {
            binding
                .connection
                .resize_window(binding.window, size_info.columns, size_info.rows);
            binding.connection.restore_pane(binding.pane);
        }
        let executor = ctx.background_executor();
        executor
            .spawn(run_output(model, listener, binding.clone(), events))
            .detach();
        executor.spawn(run_input(binding, messages)).detach();
        Self
    }
}

impl Entity for TmuxEventLoop {
    type Event = ();
}

pub(super) async fn run_output(
    model: Arc<FairMutex<TerminalModel>>,
    listener: ChannelEventListener,
    binding: TmuxPaneBinding,
    events: Receiver<PaneEvent>,
) {
    let mut processor = Processor::default();
    let mut unwrapper = PassthroughUnwrapper::new();
    let mut bytes = Vec::new();
    while let Ok(event) = events.recv().await {
        let data = match event {
            PaneEvent::Output(data) => Some(data),
            PaneEvent::Restored(data) => {
                // A sequence cut off by a pause would otherwise swallow the redraw.
                processor = Processor::default();
                unwrapper = PassthroughUnwrapper::new();
                Some(data)
            }
            PaneEvent::Paused => None,
        };
        if let Some(data) = data {
            bytes.clear();
            unwrapper.push(&data, &mut bytes);
            // tmux is the pane's terminal and has already answered its queries.
            processor.parse_bytes(&mut *model.lock(), &bytes, &mut io::sink());
            listener.send_wakeup_event();
        }
        if events.is_empty() {
            binding.connection.resume_if_paused(binding.pane);
        }
    }
}

pub(super) async fn run_input(binding: TmuxPaneBinding, messages: Receiver<Message>) {
    let TmuxPaneBinding {
        connection,
        window,
        pane,
        visible,
        ..
    } = binding;
    let mut restored = visible;
    while let Ok(message) = messages.recv().await {
        match message {
            Message::Input(bytes) => connection.send_input(pane, &bytes),
            Message::Resize(size_info) => {
                connection.resize_window(window, size_info.columns, size_info.rows);
                if !restored {
                    connection.restore_pane(pane);
                    restored = true;
                }
            }
            Message::Shutdown => {
                connection.unsubscribe_pane(pane);
                return;
            }
            Message::ChildExited => {}
        }
    }
}

#[cfg(test)]
#[path = "event_loop_tests.rs"]
mod tests;
