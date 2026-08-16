use super::bedrock_auth::set_user_model_provider_to_bedrock;
use super::*;
use crate::auth_mode::auth_mode_to_api;
use crate::external_auth::ExternalAuthBridge;
use codex_app_server_protocol::DesktopOnboardingEntrypoint;
use whisply_login::LoginOnboardingEntrypoint;
use whisply_model_provider::is_supported_amazon_bedrock_region;

mod rate_limit_resets;

// Duration before a browser ChatGPT login attempt is abandoned.
const LOGIN_CHATGPT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const WHISPLY_MANAGED_ACCOUNT_LOGIN_ERROR: &str =
    "Whisply account login is managed by the installed app. Sign in through Whisply.";
const WHISPLY_MANAGED_ACCOUNT_LOGOUT_ERROR: &str =
    "Whisply account logout is managed by the installed app. Sign out through Whisply.";
/// The native broker does not yet expose a safe account-data snapshot bridge.
/// Keep all legacy BackendClient routes unavailable rather than loading a
/// persisted direct credential or reaching a first-party provider.
const WHISPLY_MANAGED_ACCOUNT_DATA_UNAVAILABLE_ERROR: &str =
    "Whisply account data is managed by the installed app and is unavailable in this runtime.";
// Login overrides are intentionally available only in debug builds.
#[cfg(debug_assertions)]
const LOGIN_ISSUER_OVERRIDE_ENV_VAR: &str = "CODEX_APP_SERVER_LOGIN_ISSUER";
#[cfg(debug_assertions)]
const LOGIN_OPEN_APP_URL_OVERRIDE_ENV_VAR: &str = "CODEX_APP_SERVER_DEV_OPEN_APP_URL";

enum ActiveLogin {
    Browser {
        shutdown_handle: ShutdownHandle,
        login_id: Uuid,
    },
    DeviceCode {
        cancel: CancellationToken,
        login_id: Uuid,
    },
}

impl ActiveLogin {
    fn login_id(&self) -> Uuid {
        match self {
            ActiveLogin::Browser { login_id, .. } | ActiveLogin::DeviceCode { login_id, .. } => {
                *login_id
            }
        }
    }

    fn cancel(&self) {
        match self {
            ActiveLogin::Browser {
                shutdown_handle, ..
            } => shutdown_handle.shutdown(),
            ActiveLogin::DeviceCode { cancel, .. } => cancel.cancel(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum CancelLoginError {
    NotFound,
}

/// `account/login/start` has no broker-bound Whisply account variant. Every
/// currently deserializable variant carries a generic provider credential or
/// starts a generic provider OAuth flow, so reject it before auth or config
/// state can be read or written.
///
/// Keep this match exhaustive: adding a protocol variant must explicitly
/// decide whether it is broker-managed rather than silently becoming a login
/// route.
fn reject_non_whisply_account_login(params: &LoginAccountParams) -> Result<(), JSONRPCErrorError> {
    match params {
        LoginAccountParams::ApiKey { .. }
        | LoginAccountParams::Chatgpt { .. }
        | LoginAccountParams::ChatgptDeviceCode
        | LoginAccountParams::ChatgptAuthTokens { .. }
        | LoginAccountParams::AmazonBedrock { .. } => {
            Err(invalid_request(WHISPLY_MANAGED_ACCOUNT_LOGIN_ERROR))
        }
    }
}

impl Drop for ActiveLogin {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[derive(Clone)]
pub(crate) struct AccountRequestProcessor {
    auth_manager: Arc<AuthManager>,
    thread_manager: Arc<ThreadManager>,
    outgoing: Arc<OutgoingMessageSender>,
    config: Arc<Config>,
    config_manager: ConfigManager,
    active_login: Arc<Mutex<Option<ActiveLogin>>>,
}

impl AccountRequestProcessor {
    pub(crate) fn new(
        auth_manager: Arc<AuthManager>,
        thread_manager: Arc<ThreadManager>,
        outgoing: Arc<OutgoingMessageSender>,
        config: Arc<Config>,
        config_manager: ConfigManager,
    ) -> Self {
        Self {
            auth_manager,
            thread_manager,
            outgoing,
            config,
            config_manager,
            active_login: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) async fn login_account(
        &self,
        request_id: ConnectionRequestId,
        params: LoginAccountParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        reject_non_whisply_account_login(&params)?;
        self.login_v2(request_id, params).await.map(|()| None)
    }

    pub(crate) async fn logout_account(
        &self,
        _request_id: ConnectionRequestId,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        Err(invalid_request(WHISPLY_MANAGED_ACCOUNT_LOGOUT_ERROR))
    }

    pub(crate) async fn cancel_login_account(
        &self,
        params: CancelLoginAccountParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.cancel_login_response(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn get_account(
        &self,
        params: GetAccountParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.get_account_response(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn get_auth_status(
        &self,
        params: GetAuthStatusParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.get_auth_status_response(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn get_account_rate_limits(
        &self,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.get_account_rate_limits_response()
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn get_account_token_usage(
        &self,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.get_account_token_usage_response()
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn get_workspace_messages(
        &self,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.get_workspace_messages_response()
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn send_add_credits_nudge_email(
        &self,
        params: SendAddCreditsNudgeEmailParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.send_add_credits_nudge_email_response(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn cancel_active_login(&self) {
        let mut guard = self.active_login.lock().await;
        if let Some(active_login) = guard.take() {
            drop(active_login);
        }
    }

    pub(crate) fn clear_external_auth(&self) {
        self.auth_manager.clear_external_auth();
        self.thread_manager.plugins_manager().set_auth_mode(None);
    }

    fn current_account_updated_notification(&self) -> AccountUpdatedNotification {
        let auth = self.auth_manager.auth_cached();
        AccountUpdatedNotification {
            auth_mode: auth
                .as_ref()
                .map(CodexAuth::api_auth_mode)
                .map(auth_mode_to_api),
            plan_type: auth.as_ref().and_then(CodexAuth::account_plan_type),
        }
    }

    async fn load_latest_config(&self) -> Config {
        match self
            .config_manager
            .load_latest_config(/*fallback_cwd*/ None)
            .await
        {
            Ok(config) => config,
            Err(err) => {
                tracing::warn!("failed to reload config, using startup config: {err}");
                self.config.as_ref().clone()
            }
        }
    }

    async fn maybe_refresh_plugin_caches_for_current_config(
        _config_manager: &ConfigManager,
        thread_manager: &Arc<ThreadManager>,
        _auth: Option<CodexAuth>,
    ) {
        thread_manager.plugins_manager().set_auth_mode(None);
        thread_manager
            .plugins_manager()
            .clear_recommended_plugins_cache();
        // Account changes cannot authorize any direct plugin work in BrokerOnly.
        // Clear only local projections; do not start the legacy curated Git/HTTP
        // sync or remote installed/catalog refreshes.
        thread_manager.plugins_manager().clear_cache();
        thread_manager.skills_service().clear_cache();
        Self::spawn_effective_plugins_changed_task(
            Arc::clone(thread_manager),
            _config_manager.clone(),
        );
    }

    fn spawn_effective_plugins_changed_task(
        thread_manager: Arc<ThreadManager>,
        config_manager: ConfigManager,
    ) {
        tokio::spawn(async move {
            thread_manager.plugins_manager().clear_cache();
            thread_manager.skills_service().clear_cache();
            crate::mcp_refresh::reload_mcp_config_best_effort(&thread_manager, &config_manager)
                .await;
            thread_manager.invalidate_mcp_runtimes().await;
        });
    }

    async fn login_v2(
        &self,
        request_id: ConnectionRequestId,
        params: LoginAccountParams,
    ) -> Result<(), JSONRPCErrorError> {
        match params {
            LoginAccountParams::ApiKey { api_key } => {
                self.login_api_key_v2(request_id, LoginApiKeyParams { api_key })
                    .await;
            }
            LoginAccountParams::Chatgpt {
                app_brand,
                codex_streamlined_login,
                use_hosted_login_success_page,
            } => {
                let login_success_page = if use_hosted_login_success_page {
                    let app_brand = match app_brand.unwrap_or_default() {
                        LoginAppBrand::Codex => LoginSuccessPageBrand::Codex,
                        LoginAppBrand::Chatgpt => LoginSuccessPageBrand::Chatgpt,
                    };
                    LoginSuccessPage::Hosted {
                        url: CODEX_OPEN_APP_URL.parse().map_err(|err| {
                            internal_error(format!("invalid Whisply open app URL: {err}"))
                        })?,
                        app_brand,
                    }
                } else {
                    LoginSuccessPage::default()
                };
                self.login_chatgpt_v2(request_id, codex_streamlined_login, login_success_page)
                    .await;
            }
            LoginAccountParams::ChatgptDeviceCode => {
                self.login_chatgpt_device_code_v2(request_id).await;
            }
            LoginAccountParams::ChatgptAuthTokens {
                access_token,
                chatgpt_account_id,
                chatgpt_plan_type,
            } => {
                self.login_chatgpt_auth_tokens(
                    request_id,
                    access_token,
                    chatgpt_account_id,
                    chatgpt_plan_type,
                )
                .await;
            }
            LoginAccountParams::AmazonBedrock { api_key, region } => {
                self.login_amazon_bedrock_v2(request_id, api_key, region)
                    .await;
            }
        }
        Ok(())
    }

    fn external_auth_active_error(&self) -> JSONRPCErrorError {
        invalid_request(
            "External auth is active. Use account/login/start (chatgptAuthTokens) to update it or account/logout to clear it.",
        )
    }

    async fn login_api_key_common(
        &self,
        params: &LoginApiKeyParams,
    ) -> std::result::Result<(), JSONRPCErrorError> {
        if self.auth_manager.is_external_chatgpt_auth_active() {
            return Err(self.external_auth_active_error());
        }

        if !self
            .auth_manager
            .is_login_method_allowed(ForcedLoginMethod::Api)
        {
            return Err(invalid_request(
                "API key login is disabled. Use ChatGPT login instead.",
            ));
        }

        // Cancel any active login attempt.
        {
            let mut guard = self.active_login.lock().await;
            if let Some(active) = guard.take() {
                drop(active);
            }
        }

        match login_with_api_key(
            &self.config.codex_home,
            &params.api_key,
            self.config.cli_auth_credentials_store_mode,
            self.config.auth_keyring_backend_kind(),
        ) {
            Ok(()) => {
                self.auth_manager.reload().await;
                Ok(())
            }
            Err(err) => Err(internal_error(format!("failed to save api key: {err}"))),
        }
    }

    async fn login_api_key_v2(&self, request_id: ConnectionRequestId, params: LoginApiKeyParams) {
        let result = self
            .login_api_key_common(&params)
            .await
            .map(|()| LoginAccountResponse::ApiKey {});
        let logged_in = result.is_ok();
        self.outgoing.send_result(request_id, result).await;

        if logged_in {
            self.send_login_success_notifications(/*login_id*/ None)
                .await;
        }
    }

    async fn login_amazon_bedrock_v2(
        &self,
        request_id: ConnectionRequestId,
        api_key: String,
        region: String,
    ) {
        let result = async {
            if self.auth_manager.is_external_chatgpt_auth_active() {
                return Err(self.external_auth_active_error());
            }
            if !self
                .auth_manager
                .is_login_method_allowed(ForcedLoginMethod::Api)
            {
                return Err(invalid_request(
                    "Amazon Bedrock login is disabled. Use ChatGPT login instead.",
                ));
            }

            let api_key = api_key.trim();
            if api_key.is_empty() {
                return Err(invalid_request("Amazon Bedrock API key must not be empty."));
            }
            let region = region.trim();
            if !is_supported_amazon_bedrock_region(region) {
                return Err(invalid_request(format!(
                    "Amazon Bedrock Mantle does not support region `{region}`"
                )));
            }

            {
                let mut guard = self.active_login.lock().await;
                if let Some(active) = guard.take() {
                    drop(active);
                }
            }

            set_user_model_provider_to_bedrock(&self.config_manager).await?;
            login_with_bedrock_api_key(
                &self.config.codex_home,
                api_key,
                region,
                self.config.cli_auth_credentials_store_mode,
                self.config.auth_keyring_backend_kind(),
            )
            .map_err(|err| internal_error(format!("failed to save Amazon Bedrock auth: {err}")))?;
            self.auth_manager.reload().await;
            Ok(LoginAccountResponse::AmazonBedrock {})
        }
        .await;
        let logged_in = result.is_ok();
        self.outgoing.send_result(request_id, result).await;

        if logged_in {
            self.send_login_success_notifications(/*login_id*/ None)
                .await;
        }
    }

    // Build options for a ChatGPT login attempt; performs validation.
    async fn login_chatgpt_common(
        &self,
        codex_streamlined_login: bool,
        login_success_page: LoginSuccessPage,
    ) -> std::result::Result<LoginServerOptions, JSONRPCErrorError> {
        let config = self.config.as_ref();

        if self.auth_manager.is_external_chatgpt_auth_active() {
            return Err(self.external_auth_active_error());
        }

        if !self
            .auth_manager
            .is_login_method_allowed(ForcedLoginMethod::Chatgpt)
        {
            return Err(invalid_request(
                "ChatGPT login is disabled. Use API key login instead.",
            ));
        }

        let opts = LoginServerOptions {
            open_browser: false,
            codex_streamlined_login,
            login_success_page,
            ..LoginServerOptions::new(
                config.codex_home.to_path_buf(),
                oauth_client_id(),
                self.auth_manager.effective_chatgpt_workspaces(),
                config.cli_auth_credentials_store_mode,
                config.auth_keyring_backend_kind(),
                config.auth_route_config(),
            )
        };
        #[cfg(debug_assertions)]
        let opts = {
            let mut opts = opts;
            if let Ok(issuer) = std::env::var(LOGIN_ISSUER_OVERRIDE_ENV_VAR)
                && !issuer.trim().is_empty()
            {
                opts.issuer = issuer;
            }
            if let LoginSuccessPage::Hosted { url, .. } = &mut opts.login_success_page
                && let Ok(open_app_url) = std::env::var(LOGIN_OPEN_APP_URL_OVERRIDE_ENV_VAR)
                && !open_app_url.trim().is_empty()
            {
                *url = open_app_url.parse().map_err(|err| {
                    internal_error(format!("invalid Whisply open app URL: {err}"))
                })?;
            }
            opts
        };

        Ok(opts)
    }

    fn login_chatgpt_device_code_start_error(err: IoError) -> JSONRPCErrorError {
        let is_not_found = err.kind() == std::io::ErrorKind::NotFound;
        if is_not_found {
            invalid_request(err.to_string())
        } else {
            internal_error(format!("failed to request device code: {err}"))
        }
    }

    async fn login_chatgpt_v2(
        &self,
        request_id: ConnectionRequestId,
        codex_streamlined_login: bool,
        login_success_page: LoginSuccessPage,
    ) {
        let result = self
            .login_chatgpt_response(codex_streamlined_login, login_success_page)
            .await;
        self.outgoing.send_result(request_id, result).await;
    }

    async fn login_chatgpt_response(
        &self,
        codex_streamlined_login: bool,
        login_success_page: LoginSuccessPage,
    ) -> Result<LoginAccountResponse, JSONRPCErrorError> {
        let opts = self
            .login_chatgpt_common(codex_streamlined_login, login_success_page)
            .await?;
        let server = run_login_server(opts)
            .map_err(|err| internal_error(format!("failed to start login server: {err}")))?;
        let login_id = Uuid::new_v4();
        let shutdown_handle = server.cancel_handle();

        // Replace active login if present.
        {
            let mut guard = self.active_login.lock().await;
            if let Some(existing) = guard.take() {
                drop(existing);
            }
            *guard = Some(ActiveLogin::Browser {
                shutdown_handle: shutdown_handle.clone(),
                login_id,
            });
        }

        let outgoing_clone = self.outgoing.clone();
        let config_manager = self.config_manager.clone();
        let thread_manager = Arc::clone(&self.thread_manager);
        let active_login = self.active_login.clone();
        let auth_url = server.auth_url.clone();
        tokio::spawn(async move {
            let (success, error_msg, onboarding_entrypoint) = match tokio::time::timeout(
                LOGIN_CHATGPT_TIMEOUT,
                server.block_until_done_with_callback_result(),
            )
            .await
            {
                Ok(Ok(result)) => (
                    true,
                    None,
                    result
                        .onboarding_entrypoint
                        .map(|LoginOnboardingEntrypoint::LifeSciences| {
                            DesktopOnboardingEntrypoint::LifeSciences
                        }),
                ),
                Ok(Err(err)) => (false, Some(format!("Login server error: {err}")), None),
                Err(_elapsed) => {
                    shutdown_handle.shutdown();
                    (false, Some("Login timed out".to_string()), None)
                }
            };

            Self::send_chatgpt_login_completion_notifications(
                &outgoing_clone,
                config_manager,
                thread_manager,
                AccountLoginCompletedNotification {
                    login_id: Some(login_id.to_string()),
                    success,
                    error: error_msg,
                    onboarding_entrypoint,
                },
            )
            .await;

            // Clear the active login if it matches this attempt. It may have been replaced or cancelled.
            let mut guard = active_login.lock().await;
            if guard.as_ref().map(ActiveLogin::login_id) == Some(login_id) {
                *guard = None;
            }
        });

        Ok(LoginAccountResponse::Chatgpt {
            login_id: login_id.to_string(),
            auth_url,
        })
    }

    async fn login_chatgpt_device_code_v2(&self, request_id: ConnectionRequestId) {
        let result = self.login_chatgpt_device_code_response().await;
        self.outgoing.send_result(request_id, result).await;
    }

    async fn login_chatgpt_device_code_response(
        &self,
    ) -> Result<LoginAccountResponse, JSONRPCErrorError> {
        let opts = self
            .login_chatgpt_common(
                /*codex_streamlined_login*/ false,
                LoginSuccessPage::default(),
            )
            .await?;
        let device_code = request_device_code(&opts)
            .await
            .map_err(Self::login_chatgpt_device_code_start_error)?;
        let login_id = Uuid::new_v4();
        let cancel = CancellationToken::new();

        {
            let mut guard = self.active_login.lock().await;
            if let Some(existing) = guard.take() {
                drop(existing);
            }
            *guard = Some(ActiveLogin::DeviceCode {
                cancel: cancel.clone(),
                login_id,
            });
        }

        let verification_url = device_code.verification_url.clone();
        let user_code = device_code.user_code.clone();

        let outgoing_clone = self.outgoing.clone();
        let config_manager = self.config_manager.clone();
        let thread_manager = Arc::clone(&self.thread_manager);
        let active_login = self.active_login.clone();
        tokio::spawn(async move {
            let (success, error_msg) = tokio::select! {
                _ = cancel.cancelled() => {
                    (false, Some("Login was not completed".to_string()))
                }
                r = complete_device_code_login(opts, device_code) => {
                    match r {
                        Ok(()) => (true, None),
                        Err(err) => (false, Some(err.to_string())),
                    }
                }
            };

            Self::send_chatgpt_login_completion_notifications(
                &outgoing_clone,
                config_manager,
                thread_manager,
                AccountLoginCompletedNotification {
                    login_id: Some(login_id.to_string()),
                    success,
                    error: error_msg,
                    onboarding_entrypoint: None,
                },
            )
            .await;

            let mut guard = active_login.lock().await;
            if guard.as_ref().map(ActiveLogin::login_id) == Some(login_id) {
                *guard = None;
            }
        });

        Ok(LoginAccountResponse::ChatgptDeviceCode {
            login_id: login_id.to_string(),
            verification_url,
            user_code,
        })
    }

    async fn cancel_login_chatgpt_common(
        &self,
        login_id: Uuid,
    ) -> std::result::Result<(), CancelLoginError> {
        let mut guard = self.active_login.lock().await;
        if guard.as_ref().map(ActiveLogin::login_id) == Some(login_id) {
            if let Some(active) = guard.take() {
                drop(active);
            }
            Ok(())
        } else {
            Err(CancelLoginError::NotFound)
        }
    }

    async fn cancel_login_response(
        &self,
        params: CancelLoginAccountParams,
    ) -> Result<CancelLoginAccountResponse, JSONRPCErrorError> {
        let login_id = params.login_id;
        let uuid = Uuid::parse_str(&login_id)
            .map_err(|_| invalid_request(format!("invalid login id: {login_id}")))?;
        let status = match self.cancel_login_chatgpt_common(uuid).await {
            Ok(()) => CancelLoginAccountStatus::Canceled,
            Err(CancelLoginError::NotFound) => CancelLoginAccountStatus::NotFound,
        };
        Ok(CancelLoginAccountResponse { status })
    }

    async fn login_chatgpt_auth_tokens(
        &self,
        request_id: ConnectionRequestId,
        access_token: String,
        chatgpt_account_id: String,
        chatgpt_plan_type: Option<String>,
    ) {
        let result = self
            .login_chatgpt_auth_tokens_response(access_token, chatgpt_account_id, chatgpt_plan_type)
            .await;
        let logged_in = result.is_ok();
        self.outgoing.send_result(request_id, result).await;

        if logged_in {
            self.send_login_success_notifications(/*login_id*/ None)
                .await;
        }
    }

    async fn login_chatgpt_auth_tokens_response(
        &self,
        access_token: String,
        chatgpt_account_id: String,
        chatgpt_plan_type: Option<String>,
    ) -> Result<LoginAccountResponse, JSONRPCErrorError> {
        if !self
            .auth_manager
            .is_login_method_allowed(ForcedLoginMethod::Chatgpt)
        {
            return Err(invalid_request(
                "External ChatGPT auth is disabled. Use API key login instead.",
            ));
        }

        // Cancel any active login attempt to avoid persisting managed auth state.
        {
            let mut guard = self.active_login.lock().await;
            if let Some(active) = guard.take() {
                drop(active);
            }
        }

        if let Some(expected_workspaces) = self.auth_manager.effective_chatgpt_workspaces()
            && !expected_workspaces.contains(&chatgpt_account_id)
        {
            return Err(invalid_request(format!(
                "External auth must use one of workspace(s) {expected_workspaces:?}, but received {chatgpt_account_id:?}.",
            )));
        }

        let auth = CodexAuth::from_external_chatgpt_tokens(
            &access_token,
            &chatgpt_account_id,
            chatgpt_plan_type.as_deref(),
        )
        .map_err(|err| internal_error(format!("failed to set external auth: {err}")))?;
        self.auth_manager
            .set_external_auth(Arc::new(ExternalAuthBridge::new(
                Arc::clone(&self.outgoing),
                auth,
            )))
            .await
            .map_err(|err| internal_error(format!("failed to set external auth: {err}")))?;
        self.config_manager
            .sync_default_client_residency_requirement()
            .await;

        Ok(LoginAccountResponse::ChatgptAuthTokens {})
    }

    async fn send_login_success_notifications(&self, login_id: Option<Uuid>) {
        Self::maybe_refresh_plugin_caches_for_current_config(
            &self.config_manager,
            &self.thread_manager,
            self.auth_manager.auth_cached(),
        )
        .await;

        let payload_login_completed = AccountLoginCompletedNotification {
            login_id: login_id.map(|id| id.to_string()),
            success: true,
            error: None,
            onboarding_entrypoint: None,
        };
        self.outgoing
            .send_server_notification(ServerNotification::AccountLoginCompleted(
                payload_login_completed,
            ))
            .await;

        self.outgoing
            .send_server_notification(ServerNotification::AccountUpdated(
                self.current_account_updated_notification(),
            ))
            .await;
    }

    async fn send_chatgpt_login_completion_notifications(
        outgoing: &OutgoingMessageSender,
        config_manager: ConfigManager,
        thread_manager: Arc<ThreadManager>,
        payload_v2: AccountLoginCompletedNotification,
    ) {
        let success = payload_v2.success;
        outgoing
            .send_server_notification(ServerNotification::AccountLoginCompleted(payload_v2))
            .await;

        if success {
            let auth_manager = thread_manager.auth_manager();
            auth_manager.reload().await;
            config_manager
                .sync_default_client_residency_requirement()
                .await;

            let auth = auth_manager.auth_cached();
            Self::maybe_refresh_plugin_caches_for_current_config(
                &config_manager,
                &thread_manager,
                auth.clone(),
            )
            .await;
            let payload_v2 = AccountUpdatedNotification {
                auth_mode: auth
                    .as_ref()
                    .map(CodexAuth::api_auth_mode)
                    .map(auth_mode_to_api),
                plan_type: auth.as_ref().and_then(CodexAuth::account_plan_type),
            };
            outgoing
                .send_server_notification(ServerNotification::AccountUpdated(payload_v2))
                .await;
        }
    }

    async fn get_auth_status_response(
        &self,
        params: GetAuthStatusParams,
    ) -> Result<GetAuthStatusResponse, JSONRPCErrorError> {
        let include_token = params.include_token.unwrap_or(false);

        // Determine whether auth is required based on the active model provider.
        // If a custom provider is configured with `requires_openai_auth == false`,
        // then no auth step is required; otherwise, default to requiring auth.
        let config = self.load_latest_config().await;
        let requires_openai_auth = config.model_provider.requires_openai_auth;

        let response = if !requires_openai_auth {
            GetAuthStatusResponse {
                auth_method: None,
                auth_token: None,
                requires_openai_auth: Some(false),
            }
        } else {
            let auth = self.auth_manager.auth().await;
            match auth {
                Some(auth) => {
                    let permanent_refresh_failure =
                        self.auth_manager.refresh_failure_for_auth(&auth).is_some();
                    let auth_mode = auth_mode_to_api(auth.api_auth_mode());
                    let (reported_auth_method, token_opt) = if matches!(
                        auth,
                        CodexAuth::Headers(_)
                            | CodexAuth::AgentIdentity(_)
                            | CodexAuth::PersonalAccessToken(_)
                    ) || include_token
                        && permanent_refresh_failure
                    {
                        // This response cannot represent the metadata needed to reuse these
                        // credentials.
                        (Some(auth_mode), None)
                    } else {
                        match auth.get_token() {
                            Ok(token) if !token.is_empty() => {
                                let tok = if include_token { Some(token) } else { None };
                                (Some(auth_mode), tok)
                            }
                            Ok(_) => (None, None),
                            Err(err) => {
                                tracing::warn!("failed to get token for auth status: {err}");
                                (None, None)
                            }
                        }
                    };
                    GetAuthStatusResponse {
                        auth_method: reported_auth_method,
                        auth_token: token_opt,
                        requires_openai_auth: Some(true),
                    }
                }
                None => GetAuthStatusResponse {
                    auth_method: None,
                    auth_token: None,
                    requires_openai_auth: Some(true),
                },
            }
        };

        Ok(response)
    }

    async fn get_account_response(
        &self,
        _params: GetAccountParams,
    ) -> Result<GetAccountResponse, JSONRPCErrorError> {
        let config = self.load_latest_config().await;
        let provider =
            create_model_provider(config.model_provider, Some(self.auth_manager.clone()));
        let account_state = match provider.account_state() {
            Ok(account_state) => account_state,
            Err(err) => return Err(invalid_request(err.to_string())),
        };
        let account = account_state.account.map(Account::from);

        Ok(GetAccountResponse {
            account,
            requires_openai_auth: account_state.requires_openai_auth,
        })
    }

    async fn get_account_rate_limits_response(
        &self,
    ) -> Result<GetAccountRateLimitsResponse, JSONRPCErrorError> {
        Err(invalid_request(
            WHISPLY_MANAGED_ACCOUNT_DATA_UNAVAILABLE_ERROR,
        ))
    }

    async fn get_account_token_usage_response(
        &self,
    ) -> Result<GetAccountTokenUsageResponse, JSONRPCErrorError> {
        Err(invalid_request(
            WHISPLY_MANAGED_ACCOUNT_DATA_UNAVAILABLE_ERROR,
        ))
    }

    async fn get_workspace_messages_response(
        &self,
    ) -> Result<GetWorkspaceMessagesResponse, JSONRPCErrorError> {
        Err(invalid_request(
            WHISPLY_MANAGED_ACCOUNT_DATA_UNAVAILABLE_ERROR,
        ))
    }

    async fn send_add_credits_nudge_email_response(
        &self,
        _params: SendAddCreditsNudgeEmailParams,
    ) -> Result<SendAddCreditsNudgeEmailResponse, JSONRPCErrorError> {
        Err(invalid_request(
            WHISPLY_MANAGED_ACCOUNT_DATA_UNAVAILABLE_ERROR,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error_code::INVALID_REQUEST_ERROR_CODE;
    use pretty_assertions::assert_eq;

    #[test]
    fn account_login_rejects_each_non_whisply_variant_at_the_boundary() {
        let params = [
            LoginAccountParams::ApiKey {
                api_key: "user-api-key".to_string(),
            },
            LoginAccountParams::Chatgpt {
                codex_streamlined_login: false,
                use_hosted_login_success_page: false,
                app_brand: None,
            },
            LoginAccountParams::ChatgptDeviceCode,
            LoginAccountParams::ChatgptAuthTokens {
                access_token: "external-access-token".to_string(),
                chatgpt_account_id: "external-account".to_string(),
                chatgpt_plan_type: None,
            },
            LoginAccountParams::AmazonBedrock {
                api_key: "bedrock-api-key".to_string(),
                region: "us-west-2".to_string(),
            },
        ];

        for param in params {
            let error = reject_non_whisply_account_login(&param)
                .expect_err("generic account login must be rejected before any state mutation");
            assert_eq!(error.code, INVALID_REQUEST_ERROR_CODE);
            assert_eq!(error.message, WHISPLY_MANAGED_ACCOUNT_LOGIN_ERROR);
            assert_eq!(error.data, None);
        }
    }
}
