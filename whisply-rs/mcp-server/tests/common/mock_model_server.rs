use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use anyhow::ensure;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::Respond;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

/// Create a mock server that will provide the responses, in order, for
/// requests to the `/v1/responses` endpoint.
pub async fn create_mock_responses_server(responses: Vec<String>) -> MockServer {
    let server = MockServer::start().await;

    let seq_responder = SeqResponder {
        num_calls: AtomicUsize::new(0),
        responses,
    };

    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(seq_responder)
        .mount(&server)
        .await;

    server
}

/// Verifies response traffic at the successful end of a test instead of during
/// `MockServer` drop. That preserves the original child-process error when a
/// managed gateway launch fails before it can issue the first request.
pub async fn assert_mock_responses_request_count(
    server: &MockServer,
    expected: usize,
) -> anyhow::Result<()> {
    let actual = server
        .received_requests()
        .await
        .ok_or_else(|| anyhow::anyhow!("mock server does not retain received requests"))?
        .into_iter()
        .filter(|request| request.url.path() == "/v1/responses")
        .count();
    ensure!(
        actual == expected,
        "expected {expected} managed /v1/responses request(s), received {actual}"
    );
    Ok(())
}

struct SeqResponder {
    num_calls: AtomicUsize,
    responses: Vec<String>,
}

impl Respond for SeqResponder {
    fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
        let call_num = self.num_calls.fetch_add(1, Ordering::SeqCst);
        let response = self
            .responses
            .get(call_num)
            .expect("mock model response should exist");
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_raw(response.clone(), "text/event-stream")
    }
}
