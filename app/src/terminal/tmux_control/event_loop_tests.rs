use std::sync::Arc;
use std::time::Duration;

use futures::executor::block_on;
use parking_lot::FairMutex;
use tmux_control::connection::{Connection, ConnectionConfig, PaneEvent};
use tmux_control::testing::FakeTmux;
use tmux_control::{PaneId, SessionId, WindowId as TmuxWindowId, commands};

use super::*;
use crate::terminal::event_listener::ChannelEventListener;
use crate::terminal::tmux_control::TmuxSessionKey;
use crate::terminal::writeable_pty::Message;
use crate::terminal::{SizeInfo, TerminalModel};

const W: Duration = Duration::from_millis(200);

fn raw_model() -> Arc<FairMutex<TerminalModel>> {
    Arc::new(FairMutex::new(TerminalModel::new_raw_for_test()))
}

fn listener() -> ChannelEventListener {
    ChannelEventListener::new_for_test()
}

fn binding_for(connection: &Connection) -> TmuxPaneBinding {
    TmuxPaneBinding {
        connection: connection.clone(),
        key: TmuxSessionKey {
            session: SessionId(0),
        },
        window: TmuxWindowId(0),
        pane: PaneId(0),
        visible: true,
    }
}

fn fixture(
    visible: bool,
) -> (
    FakeTmux,
    Arc<FairMutex<TerminalModel>>,
    ChannelEventListener,
    TmuxPaneBinding,
    async_channel::Receiver<PaneEvent>,
) {
    let (mut tmux, connection, _events) = FakeTmux::start(ConnectionConfig::default());
    tmux.handshake("demo", "3.8");
    let pane_rx = connection.subscribe_pane(PaneId(0));
    let binding = TmuxPaneBinding {
        visible,
        ..binding_for(&connection)
    };
    (tmux, raw_model(), listener(), binding, pane_rx)
}

fn size_info(columns: usize, rows: usize) -> SizeInfo {
    SizeInfo::new_without_font_metrics(rows, columns)
}

#[test]
fn restored_and_live_output_reach_the_model_unwrapped() {
    let (_tmux, model, listener, binding, _pane_rx) = fixture(true);
    let (tx, rx) = async_channel::bounded(8);
    tx.try_send(PaneEvent::Restored(b"\x1b[H\x1b[2J\x1b[3Jab".to_vec()))
        .unwrap();
    tx.try_send(PaneEvent::Output(
        b"\x1bPtmux;\x1b\x1b]0;t\x07\x1b\\cd".to_vec(),
    ))
    .unwrap();
    tx.close();
    block_on(run_output(model.clone(), listener, binding, rx));
    let model = model.lock();
    assert_eq!(model.block_list().active_block().output_to_string(), "abcd");
    assert_eq!(model.terminal_title().as_deref(), Some("t"));
}

#[test]
fn a_restore_discards_a_wrapper_cut_off_by_a_pause() {
    let (_tmux, model, listener, binding, _pane_rx) = fixture(true);
    let (tx, rx) = async_channel::bounded(8);
    tx.try_send(PaneEvent::Output(b"\x1bPtmux;\x1b\x1b]0;x".to_vec()))
        .unwrap();
    tx.try_send(PaneEvent::Restored(b"ab".to_vec())).unwrap();
    tx.close();
    block_on(run_output(model.clone(), listener, binding, rx));
    assert_eq!(
        model.lock().block_list().active_block().output_to_string(),
        "ab"
    );
}

#[test]
fn output_queries_are_not_answered() {
    let (mut tmux, model, listener, binding, _pane_rx) = fixture(true);
    let (tx, rx) = async_channel::bounded(8);
    tx.try_send(PaneEvent::Output(b"\x1b[c\x1b[6n".to_vec()))
        .unwrap();
    tx.close();
    block_on(run_output(model, listener, binding, rx));
    // tmux is the pane's terminal and already answered these.
    assert_eq!(tmux.try_read_command(W), None);
}

#[test]
fn input_and_resize_become_tmux_commands() {
    let (mut tmux, _model, _listener, binding, pane_rx) = fixture(false);
    let (tx, rx) = async_channel::bounded(8);
    tx.try_send(Message::Input(b"hi".to_vec().into())).unwrap();
    tx.try_send(Message::Resize(size_info(100, 30))).unwrap();
    tx.try_send(Message::Resize(size_info(100, 31))).unwrap();
    tx.try_send(Message::Shutdown).unwrap();
    block_on(run_input(binding, rx));
    assert_eq!(tmux.read_command(), "send-keys -t %0 -H 68 69");
    assert_eq!(tmux.read_command(), "refresh-client -C @0:100x30");
    // A background tab restores on its first resize, once.
    assert_eq!(tmux.read_command(), commands::restore_pane(PaneId(0)));
    assert_eq!(tmux.read_command(), "refresh-client -C @0:100x31");
    assert!(pane_rx.recv_blocking().is_err());
}

#[test]
fn a_drained_paused_pane_is_resumed() {
    let (mut tmux, connection, _events) = FakeTmux::start(ConnectionConfig {
        pane_channel_capacity: 1,
    });
    tmux.handshake("demo", "3.8");
    let rx = connection.subscribe_pane(PaneId(0));
    tmux.send("%output %0 a");
    // Overflows the channel, so the pane is paused.
    tmux.send("%output %0 b");
    assert_eq!(tmux.read_command(), "refresh-client -A '%0:pause'");
    let (model, listener, binding) = (raw_model(), listener(), binding_for(&connection));
    let worker = std::thread::spawn(move || block_on(run_output(model, listener, binding, rx)));
    assert_eq!(tmux.read_command(), commands::restore_pane(PaneId(0)));
    // Closes the receiver so run_output returns.
    connection.unsubscribe_pane(PaneId(0));
    worker.join().unwrap();
}
