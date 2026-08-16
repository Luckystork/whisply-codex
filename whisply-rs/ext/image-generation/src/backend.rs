use http::HeaderMap;
use http::HeaderValue;
use whisply_api::ImageEditRequest;
use whisply_api::ImageGenerationRequest;
use whisply_api::ImageResponse;
use whisply_api::ImagesClient;
use whisply_api::ReqwestTransport;
use whisply_login::default_client::add_originator_header;
use whisply_login::default_client::create_client;
use whisply_model_provider::SharedModelProvider;

const X_CODEX_IMAGE_TURN_ID_HEADER: &str = "x-codex-image-turn-id";

#[derive(Clone)]
pub(crate) struct CodexImagesBackend {
    provider: SharedModelProvider,
    originator: Option<String>,
}

impl CodexImagesBackend {
    /// Creates a backend that sends image requests through the active model provider.
    pub(crate) fn new(provider: SharedModelProvider, originator: Option<String>) -> Self {
        Self {
            provider,
            originator,
        }
    }

    /// Resolves the provider and auth required for the current image API request.
    async fn client(&self) -> Result<ImagesClient<ReqwestTransport>, String> {
        let provider = self
            .provider
            .api_provider()
            .await
            .map_err(|err| err.to_string())?;
        let auth = self
            .provider
            .api_auth()
            .await
            .map_err(|err| err.to_string())?;
        Ok(ImagesClient::new(
            ReqwestTransport::from_http_client(create_client()),
            provider,
            auth,
        ))
    }

    /// Sends a standalone image generation request through the configured Images client.
    pub(crate) async fn generate(
        &self,
        request: ImageGenerationRequest,
        turn_id: &str,
    ) -> Result<ImageResponse, String> {
        self.client()
            .await?
            .generate(
                &request,
                image_request_headers(self.originator.as_deref(), turn_id),
            )
            .await
            .map_err(|err| err.to_string())
    }

    /// Sends a standalone image edit request through the configured Images client.
    pub(crate) async fn edit(
        &self,
        request: ImageEditRequest,
        turn_id: &str,
    ) -> Result<ImageResponse, String> {
        self.client()
            .await?
            .edit(
                &request,
                image_request_headers(self.originator.as_deref(), turn_id),
            )
            .await
            .map_err(|err| err.to_string())
    }
}

fn image_request_headers(originator: Option<&str>, turn_id: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Ok(turn_id) = HeaderValue::from_str(turn_id) {
        headers.insert(X_CODEX_IMAGE_TURN_ID_HEADER, turn_id);
    }
    if let Some(originator) = originator {
        add_originator_header(&mut headers, originator);
    }
    headers
}
