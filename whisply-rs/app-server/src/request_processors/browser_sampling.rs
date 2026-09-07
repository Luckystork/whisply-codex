use super::*;
use codex_app_server_protocol::{
    WhisplyBrowserCancelParams, WhisplyBrowserCancelResponse, WhisplyBrowserSampleParams,
    WhisplyBrowserSampleResponse,
};
use tokio_util::sync::CancellationToken;

pub(super) type BrowserSampleRegistry =
    Arc<std::sync::Mutex<HashMap<Uuid, (ConnectionId, CancellationToken)>>>;

struct BrowserSampleGuard {
    active: BrowserSampleRegistry,
    reference: Uuid,
}

impl Drop for BrowserSampleGuard {
    fn drop(&mut self) {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((_, cancellation)) = active.remove(&self.reference) {
            cancellation.cancel();
        }
    }
}

impl CatalogRequestProcessor {
    pub(crate) fn browser_connection_closed(&self, connection_id: ConnectionId) {
        let active = self
            .browser_samples
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (connection, cancellation) in active.values() {
            if *connection == connection_id {
                cancellation.cancel();
            }
        }
    }

    pub(crate) async fn whisply_browser_sample(
        &self,
        request_id: &ConnectionRequestId,
        params: WhisplyBrowserSampleParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        if !is_whisply_provider(&self.config.model_provider) {
            return Err(invalid_request(
                "Browser planning requires the authenticated Whisply runtime.",
            ));
        }
        let reference = Uuid::parse_str(&params.reference)
            .ok()
            .filter(|id| id.get_version_num() == 4 && id.to_string() == params.reference)
            .ok_or_else(|| invalid_request("This Browser step reference is invalid."))?;
        codex_whisply::validate_browser_sampling_payload(&params.payload)
            .map_err(|_| invalid_request("This Browser request payload is invalid."))?;
        let cancellation = CancellationToken::new();
        {
            let mut active = self
                .browser_samples
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if active.contains_key(&reference) || active.len() >= 32 {
                return Err(invalid_request(
                    "This Browser step is already running or cannot start now.",
                ));
            }
            active.insert(
                reference,
                (request_id.connection_id.clone(), cancellation.clone()),
            );
        }
        let _guard = BrowserSampleGuard {
            active: Arc::clone(&self.browser_samples),
            reference,
        };
        let result = self
            .run_browser_sample(reference, params, cancellation)
            .await;
        result.map(|response| Some(response.into()))
    }

    async fn run_browser_sample(
        &self,
        reference: Uuid,
        params: WhisplyBrowserSampleParams,
        cancellation: CancellationToken,
    ) -> Result<WhisplyBrowserSampleResponse, JSONRPCErrorError> {
        let gateway = match self.thread_manager.managed_gateway_client() {
            Some(gateway) => gateway,
            None => codex_whisply::managed_gateway_client_from_environment()
                .map_err(|_| internal_error("The authenticated Browser runtime is unavailable."))?
                .ok_or_else(|| {
                    internal_error("The authenticated Browser runtime is unavailable.")
                })?,
        };
        let proof_gateway = Arc::clone(&gateway);
        let proof_task =
            tokio::task::spawn_blocking(move || proof_gateway.browser_step_proof(reference));
        let proof = tokio::select! {
            _ = cancellation.cancelled() => return Err(invalid_request("The Browser step was stopped.")),
            proof = proof_task => proof.map_err(|_| internal_error("The Browser step could not be verified."))?
                .map_err(|_| invalid_request("This Browser step has expired, ended, or belongs to a different runtime."))?,
        };
        let request = Arc::new(
            codex_whisply::ValidatedBrowserSamplingRequest::new(params.payload, proof).map_err(
                |_| invalid_request("The Browser request no longer matches its admitted step."),
            )?,
        );
        let catalog = fetch_verified_whisply_catalog(
            Some(gateway),
            self.config.codex_home.to_path_buf(),
            self.config.http_client_factory(),
        )
        .await
        .map_err(|_| internal_error("The signed Browser model catalog is unavailable."))?;
        let model = catalog
            .models
            .iter()
            .find(|model| model.id.as_str() == request.payload().model_id)
            .filter(|model| {
                model.availability == codex_whisply::ModelAvailability::Available
                    && model.subscription_availability
                        == codex_whisply::SubscriptionAvailability::Available
                    && model.capabilities.supports_structured_output
                    && (request.payload().visual_jpeg_data_url.is_none()
                        || model.capabilities.supports_images)
            })
            .ok_or_else(|| {
                invalid_request("The selected model is not available for this Browser step.")
            })?;
        let output_limit = model.capabilities.output_limit;
        let result = whisply_core::browser_sampling::sample_browser_step(
            &self.thread_manager,
            &self.config,
            Arc::clone(&request),
            output_limit,
            cancellation,
        )
        .await
        .map_err(|error| {
            if matches!(
                error.details(),
                whisply_protocol::error::CodexErrorDetails::TurnAborted
            ) {
                invalid_request("The Browser step was stopped.")
            } else {
                internal_error(error.to_string())
            }
        })?;
        Ok(WhisplyBrowserSampleResponse {
            request_id: request.proof().request_id().to_string(),
            component_id: request.proof().component_id().to_string(),
            model_id: request.payload().model_id.clone(),
            output_json: result.output_json,
            receipt: codex_app_server_protocol::WhisplyBrowserSampleReceipt {
                status: codex_app_server_protocol::WhisplyBrowserReceiptStatus::Settled,
                receipt_id: result.receipt_id,
            },
        })
    }

    pub(crate) async fn whisply_browser_cancel(
        &self,
        request_id: &ConnectionRequestId,
        params: WhisplyBrowserCancelParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let reference = Uuid::parse_str(&params.reference)
            .ok()
            .filter(|id| id.get_version_num() == 4 && id.to_string() == params.reference)
            .ok_or_else(|| invalid_request("This Browser step reference is invalid."))?;
        let active = self
            .browser_samples
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let accepted = active
            .get(&reference)
            .is_some_and(|(connection, cancellation)| {
                if *connection != request_id.connection_id {
                    return false;
                }
                cancellation.cancel();
                true
            });
        Ok(Some(WhisplyBrowserCancelResponse { accepted }.into()))
    }
}
