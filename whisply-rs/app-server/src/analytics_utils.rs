use whisply_analytics::AnalyticsEventsClient;

/// App-server analytics previously sent legacy events using ambient provider
/// credentials. BrokerOnly intentionally has no direct analytics authority.
pub(crate) fn analytics_events_client_from_config(
    _auth_manager: std::sync::Arc<whisply_login::AuthManager>,
    _config: &whisply_core::config::Config,
) -> AnalyticsEventsClient {
    AnalyticsEventsClient::disabled()
}
