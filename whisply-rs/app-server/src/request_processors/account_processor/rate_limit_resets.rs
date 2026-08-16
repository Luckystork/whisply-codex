use super::*;

impl AccountRequestProcessor {
    pub(crate) async fn consume_account_rate_limit_reset_credit(
        &self,
        _params: ConsumeAccountRateLimitResetCreditParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        Err(invalid_request(
            WHISPLY_MANAGED_ACCOUNT_DATA_UNAVAILABLE_ERROR,
        ))
    }
}
