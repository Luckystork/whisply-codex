use std::collections::VecDeque;
use std::sync::Arc;

use super::TRAILING_OUTPUT_GRACE;
use super::spawn_exit_watcher;
use super::split_valid_utf8_prefix_with_max;
use super::start_streaming_output;
use crate::session::tests::make_session_and_context_with_rx;
use crate::unified_exec::UnifiedExecContext;
use crate::unified_exec::process::NoopSpawnLifecycle;
use crate::unified_exec::process::UnifiedExecProcess;
use whisply_protocol::items::CommandExecutionStatus;
use whisply_protocol::items::TurnItem;
use whisply_protocol::protocol::Event;
use whisply_protocol::protocol::EventMsg;
use whisply_sandboxing::SandboxType;

use pretty_assertions::assert_eq;
use tokio::time::Duration;
use tokio::time::Instant;

struct StreamingOutputHarness {
    process: Arc<UnifiedExecProcess>,
    stdout_tx: tokio::sync::broadcast::Sender<Vec<u8>>,
    exit_tx: tokio::sync::oneshot::Sender<i32>,
    context: UnifiedExecContext,
    rx_event: async_channel::Receiver<Event>,
}

async fn streaming_output_harness() -> anyhow::Result<StreamingOutputHarness> {
    let (writer_tx, _writer_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
    let (stdout_tx, stdout_rx) = tokio::sync::broadcast::channel::<Vec<u8>>(8);
    let (exit_tx, exit_rx) = tokio::sync::oneshot::channel::<i32>();
    let spawned = whisply_utils_pty::spawn_from_driver(whisply_utils_pty::ProcessDriver {
        writer_tx,
        stdout_rx,
        stderr_rx: None,
        exit_rx,
        terminator: None,
        writer_handle: None,
        resizer: None,
        #[cfg(windows)]
        tty: false,
    });
    let process = Arc::new(
        UnifiedExecProcess::from_spawned(spawned, SandboxType::None, Box::new(NoopSpawnLifecycle))
            .await?,
    );
    let (session, turn, rx_event) = make_session_and_context_with_rx().await;
    let context = UnifiedExecContext::new(session, turn, "streaming-output-test".to_string());
    start_streaming_output(&process, &context);

    Ok(StreamingOutputHarness {
        process,
        stdout_tx,
        exit_tx,
        context,
        rx_event,
    })
}

#[tokio::test]
async fn streaming_output_finishes_on_close_without_waiting_for_grace() -> anyhow::Result<()> {
    let StreamingOutputHarness {
        process,
        stdout_tx,
        exit_tx,
        ..
    } = streaming_output_harness().await?;
    let output_drained = process.output_drained_notify();
    let drained = output_drained.notified();
    tokio::pin!(drained);

    tokio::time::pause();
    let exited_at = Instant::now();
    exit_tx.send(0).expect("send exit");
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        stdout_tx
            .send(b"LATE-OUTPUT-MARKER".to_vec())
            .expect("send late output");
    });

    (&mut drained).await;
    let elapsed = Instant::now().saturating_duration_since(exited_at);
    tokio::time::resume();

    assert!(
        elapsed >= Duration::from_millis(50) && elapsed < TRAILING_OUTPUT_GRACE,
        "output close should finish before the grace fallback: {elapsed:?}"
    );
    assert_eq!(
        process
            .terminal_output_buffer()
            .lock()
            .await
            .to_bytes_with_omission_marker(),
        b"LATE-OUTPUT-MARKER"
    );

    Ok(())
}

#[tokio::test]
async fn streaming_output_keeps_grace_as_fallback_without_close() -> anyhow::Result<()> {
    let StreamingOutputHarness {
        process,
        stdout_tx: _stdout_tx,
        exit_tx,
        ..
    } = streaming_output_harness().await?;
    let output_drained = process.output_drained_notify();
    let drained = output_drained.notified();
    tokio::pin!(drained);

    tokio::time::pause();
    let exited_at = Instant::now();
    exit_tx.send(0).expect("send exit");
    (&mut drained).await;
    let elapsed = Instant::now().saturating_duration_since(exited_at);
    tokio::time::resume();

    assert!(
        elapsed >= TRAILING_OUTPUT_GRACE
            && elapsed <= TRAILING_OUTPUT_GRACE + Duration::from_millis(10),
        "missing output close should use the grace fallback: {elapsed:?}"
    );

    Ok(())
}

#[tokio::test]
async fn exit_watcher_waits_for_late_network_denial_before_classifying_end() -> anyhow::Result<()> {
    let StreamingOutputHarness {
        process,
        stdout_tx,
        exit_tx,
        context,
        rx_event,
    } = streaming_output_harness().await?;

    tokio::time::pause();
    let process_for_late_denial = Arc::clone(&process);
    let (late_denial_armed_tx, late_denial_armed_rx) = tokio::sync::oneshot::channel();
    let network_denial_monitor = tokio::spawn(async move {
        let sleep = tokio::time::sleep(Duration::from_millis(10));
        tokio::pin!(sleep);
        late_denial_armed_tx.send(()).expect("arm late denial");
        sleep.await;
        process_for_late_denial.fail_and_terminate("LATE_DENIAL".to_string());
    });
    late_denial_armed_rx.await.expect("late denial armed");

    #[allow(deprecated)]
    let cwd = context.turn.cwd.clone().into();
    spawn_exit_watcher(
        Arc::clone(&process),
        Arc::clone(&context.session),
        Arc::clone(&context.turn),
        context.call_id,
        vec!["proof".to_string()],
        cwd,
        /*process_id*/ 123,
        /*plugin_attribution*/ None,
        process.terminal_output_buffer(),
        Instant::now(),
        Some(network_denial_monitor),
    );

    let exited_at = Instant::now();
    exit_tx.send(0).expect("send exit");
    drop(stdout_tx);

    let event = rx_event.recv().await.expect("command end event");
    let elapsed = Instant::now().saturating_duration_since(exited_at);
    tokio::time::resume();
    let EventMsg::ItemCompleted(completed) = event.msg else {
        panic!("expected ItemCompleted");
    };
    let TurnItem::CommandExecution(item) = completed.item else {
        panic!("expected CommandExecution");
    };
    assert_eq!(
        (
            item.status,
            item.exit_code,
            item.aggregated_output.as_deref()
        ),
        (
            CommandExecutionStatus::Failed,
            Some(-1),
            Some("LATE_DENIAL")
        )
    );
    assert!(
        elapsed >= Duration::from_millis(10) && elapsed < TRAILING_OUTPUT_GRACE,
        "completion should wait for denial without falling back to the output grace: {elapsed:?}"
    );

    Ok(())
}

#[tokio::test]
async fn exit_watcher_uses_canonical_output_when_live_deltas_lag() -> anyhow::Result<()> {
    let StreamingOutputHarness {
        process,
        stdout_tx,
        exit_tx,
        context,
        rx_event,
    } = streaming_output_harness().await?;

    {
        let terminal_output = process.terminal_output_buffer();
        let mut output = terminal_output.lock().await;
        output.push_chunk(b"CANONICAL-HEAD\n".to_vec());
        output.push_chunk(vec![
            b'x';
            crate::unified_exec::UNIFIED_EXEC_OUTPUT_MAX_BYTES
        ]);
        output.push_chunk(b"CANONICAL-TAIL\n".to_vec());
    }

    // This channel has a fixed capacity of 64. Sending 65 chunks without an
    // await deterministically puts the live subscriber behind before it can
    // poll, while the canonical output remains producer-owned.
    let live_output = process.output_sender_for_test();
    for index in 0..65 {
        live_output
            .send(format!("LIVE-{index}\n").into_bytes())
            .expect("live output receiver should be subscribed");
    }

    #[allow(deprecated)]
    let cwd = context.turn.cwd.clone().into();
    spawn_exit_watcher(
        Arc::clone(&process),
        Arc::clone(&context.session),
        Arc::clone(&context.turn),
        context.call_id,
        vec!["proof".to_string()],
        cwd,
        /*process_id*/ 123,
        /*plugin_attribution*/ None,
        process.terminal_output_buffer(),
        Instant::now(),
        /*network_denial_monitor*/ None,
    );

    exit_tx.send(0).expect("send exit");
    drop(stdout_tx);

    let completed = loop {
        let event = rx_event.recv().await.expect("command event");
        if let EventMsg::ItemCompleted(completed) = event.msg {
            break completed;
        }
    };
    let TurnItem::CommandExecution(item) = completed.item else {
        panic!("expected CommandExecution");
    };
    let output = item.aggregated_output.expect("terminal output");
    assert!(output.contains("CANONICAL-HEAD\n"));
    assert!(output.contains("CANONICAL-TAIL\n"));
    assert!(output.contains("bytes omitted"));
    assert!(!output.contains("LIVE-"));

    Ok(())
}

#[test]
fn split_valid_utf8_prefix_respects_max_bytes_for_ascii() {
    let mut buf = VecDeque::from(b"hello word!".to_vec());

    let first =
        split_valid_utf8_prefix_with_max(&mut buf, /*max_bytes*/ 5).expect("expected prefix");
    assert_eq!(first, b"hello".to_vec());
    assert_eq!(buf, VecDeque::from(b" word!".to_vec()));

    let second =
        split_valid_utf8_prefix_with_max(&mut buf, /*max_bytes*/ 5).expect("expected prefix");
    assert_eq!(second, b" word".to_vec());
    assert_eq!(buf, VecDeque::from(b"!".to_vec()));
}

#[test]
fn split_valid_utf8_prefix_avoids_splitting_utf8_codepoints() {
    // "é" is 2 bytes in UTF-8. With a max of 3 bytes, we should only emit 1 char (2 bytes).
    let mut buf = VecDeque::from("ééé".as_bytes().to_vec());

    let first =
        split_valid_utf8_prefix_with_max(&mut buf, /*max_bytes*/ 3).expect("expected prefix");
    assert_eq!(std::str::from_utf8(&first).unwrap(), "é");
    assert_eq!(buf, VecDeque::from("éé".as_bytes().to_vec()));
}

#[test]
fn split_valid_utf8_prefix_makes_progress_on_invalid_utf8() {
    let mut buf = VecDeque::from(vec![0xff, b'a', b'b']);

    let first =
        split_valid_utf8_prefix_with_max(&mut buf, /*max_bytes*/ 2).expect("expected prefix");
    assert_eq!(first, vec![0xff]);
    assert_eq!(buf, VecDeque::from(b"ab".to_vec()));
}

#[test]
fn split_valid_utf8_prefix_consumes_all_valid_bytes_before_invalid_utf8() {
    let mut bytes = vec![b'a'; 4096];
    bytes.push(0xff);
    bytes.extend(vec![b'b'; 4096]);
    let mut buf = VecDeque::from(bytes);

    let first =
        split_valid_utf8_prefix_with_max(&mut buf, /*max_bytes*/ 8192).expect("expected prefix");
    assert_eq!(first, vec![b'a'; 4096]);

    let second =
        split_valid_utf8_prefix_with_max(&mut buf, /*max_bytes*/ 8192).expect("expected prefix");
    assert_eq!(second, vec![0xff]);

    let third =
        split_valid_utf8_prefix_with_max(&mut buf, /*max_bytes*/ 8192).expect("expected prefix");
    assert_eq!(third, vec![b'b'; 4096]);
    assert!(buf.is_empty());
}

#[test]
fn split_invalid_utf8_advances_without_shifting_remaining_bytes() {
    let mut buf = VecDeque::from(vec![0xff; 1024]);
    let initial = buf.as_slices().0.as_ptr();

    for offset in 0..1024 {
        assert_eq!(
            split_valid_utf8_prefix_with_max(&mut buf, /*max_bytes*/ 128),
            Some(vec![0xff])
        );
        if let Some(first) = buf.as_slices().0.first() {
            assert_eq!(first, &0xff);
            assert_eq!(buf.as_slices().0.as_ptr(), initial.wrapping_add(offset + 1));
        }
    }

    assert!(buf.is_empty());
}
