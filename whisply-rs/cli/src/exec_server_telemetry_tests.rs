use super::run_until_shutdown_with_signals;

#[tokio::test]
async fn shutdown_signal_stops_executor() {
    let result = run_until_shutdown_with_signals(
        std::future::pending::<Result<(), std::io::Error>>(),
        std::future::ready(Ok(())),
    )
    .await;

    assert!(result.is_ok());
}
