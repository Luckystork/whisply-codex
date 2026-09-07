use super::*;
use futures::StreamExt;
use whisply_core::config::permission_profile_catalog;

#[derive(Clone)]
pub(crate) struct CatalogRequestProcessor {
    pub(super) outgoing: Arc<OutgoingMessageSender>,
    pub(super) skills_watcher: Arc<SkillsWatcher>,
    pub(super) thread_manager: Arc<ThreadManager>,
    pub(super) config: Arc<Config>,
    pub(super) config_manager: ConfigManager,
    pub(super) browser_samples: super::browser_sampling::BrowserSampleRegistry,
}

const SKILLS_LIST_CWD_CONCURRENCY: usize = 5;

fn skills_to_info(
    skills: &[whisply_core::skills::SkillMetadata],
    disabled_paths: &HashSet<AbsolutePathBuf>,
) -> Vec<codex_app_server_protocol::SkillMetadata> {
    skills
        .iter()
        .map(|skill| {
            let enabled = !disabled_paths.contains(&skill.path_to_skills_md);
            codex_app_server_protocol::SkillMetadata {
                name: skill.name.clone(),
                description: skill.description.clone(),
                short_description: skill.short_description.clone(),
                version: skill.version.clone(),
                interface: skill.interface.clone().map(|interface| {
                    codex_app_server_protocol::SkillInterface {
                        display_name: interface.display_name,
                        short_description: interface.short_description,
                        icon_small: interface.icon_small,
                        icon_large: interface.icon_large,
                        icon_small_url: None,
                        icon_large_url: None,
                        brand_color: interface.brand_color,
                        default_prompt: interface.default_prompt,
                    }
                }),
                dependencies: skill.dependencies.clone().map(|dependencies| {
                    codex_app_server_protocol::SkillDependencies {
                        tools: dependencies
                            .tools
                            .into_iter()
                            .map(|tool| codex_app_server_protocol::SkillToolDependency {
                                r#type: tool.r#type,
                                value: tool.value,
                                description: tool.description,
                                transport: tool.transport,
                                command: tool.command,
                                url: tool.url,
                            })
                            .collect(),
                    }
                }),
                path: skill.path_to_skills_md.clone(),
                scope: skill.scope.into(),
                enabled,
            }
        })
        .collect()
}

fn hooks_to_info(hooks: &[whisply_hooks::HookListEntry]) -> Vec<HookMetadata> {
    hooks
        .iter()
        .map(|hook| HookMetadata {
            key: hook.key.clone(),
            event_name: hook.event_name.into(),
            handler_type: hook.handler_type.into(),
            matcher: hook.matcher.clone(),
            command: hook.command.clone(),
            timeout_sec: hook.timeout_sec,
            status_message: hook.status_message.clone(),
            additional_context_limit: hook.additional_context_limit,
            source_path: hook.source_path.clone(),
            source: hook.source.into(),
            plugin_id: hook.plugin_id.clone(),
            display_order: hook.display_order,
            enabled: hook.enabled,
            is_managed: hook.is_managed,
            current_hash: hook.current_hash.clone(),
            trust_status: hook.trust_status.into(),
        })
        .collect()
}

fn errors_to_info(
    errors: &[whisply_core::skills::SkillError],
) -> Vec<codex_app_server_protocol::SkillErrorInfo> {
    errors
        .iter()
        .map(|err| codex_app_server_protocol::SkillErrorInfo {
            path: err.path.to_path_buf(),
            message: err.message.clone(),
        })
        .collect()
}

impl CatalogRequestProcessor {
    pub(crate) fn new(
        outgoing: Arc<OutgoingMessageSender>,
        skills_watcher: Arc<SkillsWatcher>,
        thread_manager: Arc<ThreadManager>,
        config: Arc<Config>,
        config_manager: ConfigManager,
    ) -> Self {
        Self {
            outgoing,
            skills_watcher,
            thread_manager,
            config,
            config_manager,
            browser_samples: Default::default(),
        }
    }

    pub(crate) async fn skills_list(
        &self,
        params: SkillsListParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.skills_list_response(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn hooks_list(
        &self,
        params: HooksListParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.hooks_list_response(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn skills_config_write(
        &self,
        params: SkillsConfigWriteParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.skills_config_write_response_inner(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn skills_extra_roots_set(
        &self,
        params: SkillsExtraRootsSetParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.skills_extra_roots_set_response(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn model_list(
        &self,
        params: ModelListParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        Self::list_models(
            self.thread_manager.clone(),
            self.config.http_client_factory(),
            params,
        )
        .await
        .map(|response| Some(response.into()))
    }

    pub(crate) async fn whisply_model_catalog_read(
        &self,
        params: WhisplyModelCatalogReadParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let WhisplyModelCatalogReadParams {} = params;
        if !is_whisply_provider(&self.config.model_provider) {
            return Err(invalid_request(
                "the Whisply model catalog is available only in the managed Whisply runtime",
            ));
        }

        let catalog = fetch_verified_whisply_catalog(
            self.thread_manager.managed_gateway_client(),
            self.config.codex_home.to_path_buf(),
            self.config.http_client_factory(),
        )
        .await
        .map_err(|_| internal_error("Whisply model catalog is unavailable"))?;

        Ok(Some(whisply_model_catalog_response(catalog).into()))
    }

    pub(crate) async fn experimental_feature_list(
        &self,
        params: ExperimentalFeatureListParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.experimental_feature_list_response(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn permission_profile_list(
        &self,
        params: PermissionProfileListParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.permission_profile_list_response(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn collaboration_mode_list(
        &self,
        params: CollaborationModeListParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        Self::list_collaboration_modes(self.thread_manager.clone(), params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn mock_experimental_method(
        &self,
        params: MockExperimentalMethodParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.mock_experimental_method_inner(params)
            .await
            .map(|response| Some(response.into()))
    }

    async fn resolve_cwd_config(
        &self,
        cwd: &Path,
    ) -> Result<(AbsolutePathBuf, ConfigLayerStack), String> {
        let cwd_abs =
            AbsolutePathBuf::relative_to_current_dir(cwd).map_err(|err| err.to_string())?;
        let config_layer_stack = self
            .config_manager
            .load_config_layers_for_cwd(cwd_abs.clone())
            .await
            .map_err(|err| err.to_string())?;

        Ok((cwd_abs, config_layer_stack))
    }

    async fn load_latest_config(
        &self,
        fallback_cwd: Option<PathBuf>,
    ) -> Result<Config, JSONRPCErrorError> {
        self.config_manager
            .load_latest_config(fallback_cwd)
            .await
            .map_err(|err| internal_error(format!("failed to reload config: {err}")))
    }

    async fn list_models(
        thread_manager: Arc<ThreadManager>,
        http_client_factory: whisply_http_client::HttpClientFactory,
        params: ModelListParams,
    ) -> Result<ModelListResponse, JSONRPCErrorError> {
        let ModelListParams {
            limit,
            cursor,
            include_hidden,
        } = params;
        let models = supported_models(
            thread_manager,
            include_hidden.unwrap_or(false),
            http_client_factory,
        )
        .await;
        let total = models.len();

        if total == 0 {
            return Ok(ModelListResponse {
                data: Vec::new(),
                next_cursor: None,
            });
        }

        let effective_limit = limit.unwrap_or(total as u32).max(1) as usize;
        let effective_limit = effective_limit.min(total);
        let start = match cursor {
            Some(cursor) => cursor
                .parse::<usize>()
                .map_err(|_| invalid_request(format!("invalid cursor: {cursor}")))?,
            None => 0,
        };

        if start > total {
            return Err(invalid_request(format!(
                "cursor {start} exceeds total models {total}"
            )));
        }

        let end = start.saturating_add(effective_limit).min(total);
        let items = models[start..end].to_vec();
        let next_cursor = if end < total {
            Some(end.to_string())
        } else {
            None
        };
        Ok(ModelListResponse {
            data: items,
            next_cursor,
        })
    }

    async fn list_collaboration_modes(
        thread_manager: Arc<ThreadManager>,
        params: CollaborationModeListParams,
    ) -> Result<CollaborationModeListResponse, JSONRPCErrorError> {
        let CollaborationModeListParams {} = params;
        let items = thread_manager
            .list_collaboration_modes()
            .into_iter()
            .map(Into::into)
            .collect();
        let response = CollaborationModeListResponse { data: items };
        Ok(response)
    }

    async fn experimental_feature_list_response(
        &self,
        params: ExperimentalFeatureListParams,
    ) -> Result<ExperimentalFeatureListResponse, JSONRPCErrorError> {
        let ExperimentalFeatureListParams {
            cursor,
            limit,
            thread_id,
        } = params;
        let config = match thread_id.as_deref() {
            Some(thread_id) => {
                let thread_id = ThreadId::from_string(thread_id)
                    .map_err(|err| invalid_request(format!("invalid thread id: {err}")))?;
                let thread = self
                    .thread_manager
                    .get_thread(thread_id)
                    .await
                    .map_err(|_| invalid_request(format!("thread not found: {thread_id}")))?;
                let thread_config = thread.config().await;
                self.config_manager
                    .load_latest_config_for_thread(thread_config.as_ref())
                    .await
                    .map_err(|err| internal_error(format!("failed to reload config: {err}")))?
            }
            None => self.load_latest_config(/*fallback_cwd*/ None).await?,
        };
        let data = FEATURES
            .iter()
            .map(|spec| {
                let (stage, display_name, description, announcement) = match spec.stage {
                    Stage::Experimental {
                        name,
                        menu_description,
                        announcement,
                    } => (
                        ApiExperimentalFeatureStage::Beta,
                        Some(name.to_string()),
                        Some(menu_description.to_string()),
                        Some(announcement.to_string()),
                    ),
                    Stage::UnderDevelopment => (
                        ApiExperimentalFeatureStage::UnderDevelopment,
                        None,
                        None,
                        None,
                    ),
                    Stage::Stable => (ApiExperimentalFeatureStage::Stable, None, None, None),
                    Stage::Deprecated => {
                        (ApiExperimentalFeatureStage::Deprecated, None, None, None)
                    }
                    Stage::Removed => (ApiExperimentalFeatureStage::Removed, None, None, None),
                };

                ApiExperimentalFeature {
                    name: spec.key.to_string(),
                    stage,
                    display_name,
                    description,
                    announcement,
                    // BrokerOnly projects experimental feature state from the
                    // loaded local configuration without querying workspace settings.
                    enabled: config.features.enabled(spec.id),
                    default_enabled: spec.default_enabled,
                }
            })
            .collect::<Vec<_>>();

        let total = data.len();
        if total == 0 {
            return Ok(ExperimentalFeatureListResponse {
                data: Vec::new(),
                next_cursor: None,
            });
        }

        // Clamp to 1 so limit=0 cannot return a non-advancing page.
        let effective_limit = limit.unwrap_or(total as u32).max(1) as usize;
        let effective_limit = effective_limit.min(total);
        let start = match cursor {
            Some(cursor) => match cursor.parse::<usize>() {
                Ok(idx) => idx,
                Err(_) => return Err(invalid_request(format!("invalid cursor: {cursor}"))),
            },
            None => 0,
        };

        if start > total {
            return Err(invalid_request(format!(
                "cursor {start} exceeds total feature flags {total}"
            )));
        }

        let end = start.saturating_add(effective_limit).min(total);
        let data = data[start..end].to_vec();
        let next_cursor = if end < total {
            Some(end.to_string())
        } else {
            None
        };

        Ok(ExperimentalFeatureListResponse { data, next_cursor })
    }

    async fn permission_profile_list_response(
        &self,
        params: PermissionProfileListParams,
    ) -> Result<PermissionProfileListResponse, JSONRPCErrorError> {
        let PermissionProfileListParams { cursor, limit, cwd } = params;
        let config_layer_stack = match cwd {
            Some(cwd) => {
                let cwd = PathBuf::from(cwd);
                let (_, config_layer_stack) = self
                    .resolve_cwd_config(&cwd)
                    .await
                    .map_err(|err| internal_error(format!("failed to reload config: {err}")))?;
                config_layer_stack
            }
            None => self
                .config_manager
                .load_config_layers(/*cwd*/ None)
                .await
                .map_err(|err| internal_error(format!("failed to reload config: {err}")))?,
        };
        let profiles = permission_profile_catalog(&config_layer_stack)
            .map_err(|err| internal_error(format!("failed to resolve permission profiles: {err}")))?
            .into_iter()
            .map(|profile| PermissionProfileSummary {
                id: profile.id,
                description: profile.description,
                allowed: profile.allowed,
            })
            .collect::<Vec<_>>();
        let total = profiles.len();
        let effective_limit = limit.unwrap_or(total as u32).max(1) as usize;
        let effective_limit = effective_limit.min(total);
        let start = match cursor {
            Some(cursor) => cursor
                .parse::<usize>()
                .map_err(|_| invalid_request(format!("invalid cursor: {cursor}")))?,
            None => 0,
        };

        if start > total {
            return Err(invalid_request(format!(
                "cursor {start} exceeds total permission profiles {total}"
            )));
        }

        let end = start.saturating_add(effective_limit).min(total);
        let data = profiles[start..end].to_vec();
        let next_cursor = (end < total).then_some(end.to_string());

        Ok(PermissionProfileListResponse { data, next_cursor })
    }

    async fn mock_experimental_method_inner(
        &self,
        params: MockExperimentalMethodParams,
    ) -> Result<MockExperimentalMethodResponse, JSONRPCErrorError> {
        let MockExperimentalMethodParams { value } = params;
        let response = MockExperimentalMethodResponse { echoed: value };
        Ok(response)
    }

    async fn skills_list_response(
        &self,
        params: SkillsListParams,
    ) -> Result<SkillsListResponse, JSONRPCErrorError> {
        let SkillsListParams { cwds, force_reload } = params;
        let cwds = if cwds.is_empty() {
            vec![self.config.cwd.to_path_buf()]
        } else {
            cwds
        };

        let config = self.load_latest_config(/*fallback_cwd*/ None).await?;
        let skills_service = self.thread_manager.skills_service();
        let plugins_manager = self.thread_manager.plugins_manager();
        // BrokerOnly local skills must not inherit persisted ChatGPT auth.
        plugins_manager.set_auth_mode(None);
        let local_plugins_enabled = config.features.enabled(Feature::Plugins);
        if force_reload && local_plugins_enabled {
            plugins_manager.clear_cache();
            skills_service.clear_cache();
        }
        let fs = self
            .thread_manager
            .environment_manager()
            .default_environment()
            .map(|environment| environment.get_filesystem());
        let mut data = futures::stream::iter(cwds.into_iter().enumerate())
            .map(|(index, cwd)| {
                let config = &config;
                let fs = fs.clone();
                let plugins_manager = &plugins_manager;
                let skills_service = &skills_service;
                async move {
                    let (cwd_abs, config_layer_stack) = match self.resolve_cwd_config(&cwd).await {
                        Ok(resolved) => resolved,
                        Err(message) => {
                            let error_path = cwd.clone();
                            return (
                                index,
                                codex_app_server_protocol::SkillsListEntry {
                                    cwd,
                                    skills: Vec::new(),
                                    errors: vec![codex_app_server_protocol::SkillErrorInfo {
                                        path: error_path,
                                        message,
                                    }],
                                },
                            );
                        }
                    };
                    let (effective_skill_roots, plugin_skill_snapshots) = if local_plugins_enabled {
                        let plugins_input = config.plugins_config_input();
                        if config_layer_stack == plugins_input.config_layer_stack {
                            let plugins = plugins_manager.plugins_for_config(&plugins_input).await;
                            (
                                plugins.effective_plugin_skill_roots(),
                                plugins_manager.plugin_skill_snapshots_for_config(&plugins_input),
                            )
                        } else {
                            (
                                plugins_manager
                                    .effective_skill_roots_for_layer_stack(
                                        &config_layer_stack,
                                        &plugins_input,
                                    )
                                    .await,
                                None,
                            )
                        }
                    } else {
                        (Vec::new(), None)
                    };
                    let skills_input = whisply_core::skills::HostSkillsLoadInput::new(
                        cwd_abs.clone(),
                        effective_skill_roots,
                        config_layer_stack,
                        config.bundled_skills_enabled(),
                    )
                    .with_plugin_skill_snapshots(plugin_skill_snapshots);
                    let snapshot = skills_service
                        .snapshot_for_cwd(&skills_input, force_reload, fs)
                        .await;
                    let outcome = snapshot.outcome();
                    let errors = errors_to_info(&outcome.errors);
                    let skills = skills_to_info(&outcome.skills, &outcome.disabled_paths);
                    (
                        index,
                        codex_app_server_protocol::SkillsListEntry {
                            cwd,
                            skills,
                            errors,
                        },
                    )
                }
            })
            .buffer_unordered(SKILLS_LIST_CWD_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;
        data.sort_unstable_by_key(|(index, _)| *index);
        let data = data.into_iter().map(|(_, entry)| entry).collect();
        Ok(SkillsListResponse { data })
    }

    async fn skills_extra_roots_set_response(
        &self,
        params: SkillsExtraRootsSetParams,
    ) -> Result<SkillsExtraRootsSetResponse, JSONRPCErrorError> {
        let SkillsExtraRootsSetParams { extra_roots } = params;
        let extra_roots = self
            .thread_manager
            .skills_service()
            .set_account_extra_roots(extra_roots);
        self.skills_watcher
            .register_runtime_extra_roots(&extra_roots);
        self.outgoing
            .send_server_notification(ServerNotification::SkillsChanged(
                codex_app_server_protocol::SkillsChangedNotification {},
            ))
            .await;
        Ok(SkillsExtraRootsSetResponse {})
    }

    /// Handle `hooks/list` by resolving hooks for each requested cwd.
    async fn hooks_list_response(
        &self,
        params: HooksListParams,
    ) -> Result<HooksListResponse, JSONRPCErrorError> {
        let HooksListParams { cwds } = params;
        let cwds = if cwds.is_empty() {
            vec![self.config.cwd.to_path_buf()]
        } else {
            cwds
        };

        let plugins_manager = self.thread_manager.plugins_manager();
        // Hook discovery can run during startup; keep it local and never
        // hydrate persisted ChatGPT auth for plugin sources.
        plugins_manager.set_auth_mode(None);
        let mut data = Vec::new();
        for cwd in cwds {
            let config = match self
                .config_manager
                .load_for_cwd(
                    /*request_overrides*/ None,
                    ConfigOverrides::default(),
                    Some(cwd.clone()),
                )
                .await
            {
                Ok(config) => config,
                Err(err) => {
                    let error_path = cwd.clone();
                    data.push(codex_app_server_protocol::HooksListEntry {
                        cwd,
                        hooks: Vec::new(),
                        warnings: Vec::new(),
                        errors: vec![codex_app_server_protocol::HookErrorInfo {
                            path: error_path,
                            message: err.to_string(),
                        }],
                    });
                    continue;
                }
            };
            let plugins_enabled = config.features.enabled(Feature::Plugins);
            let plugin_hooks = if plugins_enabled {
                let plugins_input = config.plugins_config_input();
                let plugin_outcome = plugins_manager.plugins_for_config(&plugins_input).await;
                whisply_core_plugins::PluginHookLoadOutcome {
                    hook_sources: plugin_outcome.effective_plugin_hook_sources(),
                    hook_load_warnings: plugin_outcome.effective_plugin_hook_warnings(),
                }
            } else {
                whisply_core_plugins::PluginHookLoadOutcome::default()
            };
            let hooks = whisply_hooks::list_hooks(whisply_hooks::HooksConfig {
                feature_enabled: config.features.enabled(Feature::CodexHooks),
                bypass_hook_trust: config.bypass_hook_trust,
                config_layer_stack: Some(config.config_layer_stack),
                plugin_hook_sources: plugin_hooks.hook_sources,
                plugin_hook_load_warnings: plugin_hooks.hook_load_warnings,
                ..Default::default()
            });
            data.push(codex_app_server_protocol::HooksListEntry {
                cwd,
                hooks: hooks_to_info(&hooks.hooks),
                warnings: hooks.warnings,
                errors: Vec::new(),
            });
        }
        Ok(HooksListResponse { data })
    }

    async fn skills_config_write_response_inner(
        &self,
        params: SkillsConfigWriteParams,
    ) -> Result<SkillsConfigWriteResponse, JSONRPCErrorError> {
        let SkillsConfigWriteParams {
            path,
            name,
            enabled,
        } = params;
        let edit = match (path, name) {
            (Some(path), None) => ConfigEdit::SetSkillConfig {
                path: path.into_path_buf(),
                enabled,
            },
            (None, Some(name)) if !name.trim().is_empty() => {
                ConfigEdit::SetSkillConfigByName { name, enabled }
            }
            _ => {
                return Err(invalid_params(
                    "skills/config/write requires exactly one of path or name",
                ));
            }
        };
        let edits = vec![edit];
        ConfigEditsBuilder::new(&self.config.codex_home)
            .with_edits(edits)
            .apply()
            .await
            .map(|()| {
                self.thread_manager.plugins_manager().clear_cache();
                self.thread_manager.skills_service().clear_cache();
                SkillsConfigWriteResponse {
                    effective_enabled: enabled,
                }
            })
            .map_err(|err| internal_error(format!("failed to update skill settings: {err}")))
    }
}

fn whisply_model_catalog_response(
    catalog: codex_whisply::ModelCatalog,
) -> WhisplyModelCatalogReadResponse {
    WhisplyModelCatalogReadResponse {
        schema_version: catalog.schema_version,
        catalog_revision: catalog.catalog_revision,
        generated_at_unix_seconds: catalog.generated_at_unix_seconds,
        expires_at_unix_seconds: catalog.expires_at_unix_seconds,
        models: catalog
            .models
            .into_iter()
            .map(|model| WhisplyModelCatalogModel {
                id: model.id.as_str().to_string(),
                display_name: model.display_name,
                short_name: model.short_name,
                description: model.description,
                provider_family: model.provider_family,
                route_revision: model.route_revision,
                capabilities: WhisplyModelCatalogCapabilities {
                    input_modalities: model.capabilities.input_modalities,
                    output_modalities: model.capabilities.output_modalities,
                    supports_tool_calls: model.capabilities.supports_tool_calls,
                    supports_images: model.capabilities.supports_images,
                    supports_structured_output: model.capabilities.supports_structured_output,
                    supports_safe_reasoning_summary: model
                        .capabilities
                        .supports_safe_reasoning_summary,
                    supports_computer_use: model.capabilities.supports_computer_use,
                    supports_native_visible_progress: model
                        .capabilities
                        .supports_native_visible_progress,
                    context_limit: model.capabilities.context_limit,
                    output_limit: model.capabilities.output_limit,
                },
                allowed_profiles: model.allowed_profiles,
                allowed_reasoning_efforts: model.allowed_reasoning_efforts,
                availability: match model.availability {
                    codex_whisply::ModelAvailability::Available => {
                        WhisplyModelCatalogAvailability::Available
                    }
                    codex_whisply::ModelAvailability::TemporarilyUnavailable => {
                        WhisplyModelCatalogAvailability::TemporarilyUnavailable
                    }
                    codex_whisply::ModelAvailability::Deprecated => {
                        WhisplyModelCatalogAvailability::Deprecated
                    }
                },
                subscription_availability: match model.subscription_availability {
                    codex_whisply::SubscriptionAvailability::Available => {
                        WhisplyModelCatalogSubscriptionAvailability::Available
                    }
                    codex_whisply::SubscriptionAvailability::UpgradeRequired => {
                        WhisplyModelCatalogSubscriptionAvailability::UpgradeRequired
                    }
                    codex_whisply::SubscriptionAvailability::Unavailable => {
                        WhisplyModelCatalogSubscriptionAvailability::Unavailable
                    }
                },
                rate_card_revision: model.rate_card_revision,
                usage_multiplier_millis: model.usage_multiplier_millis,
                recommended_default: model.recommended_default,
                response_start_timeout_seconds: model.response_start_timeout_seconds,
                response_idle_timeout_seconds: model.response_idle_timeout_seconds,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whisply_model_catalog_response_preserves_every_public_model_field() {
        let response = whisply_model_catalog_response(codex_whisply::ModelCatalog {
            schema_version: codex_whisply::MODEL_CATALOG_SCHEMA_VERSION,
            catalog_revision: "catalog-test-1".to_string(),
            generated_at_unix_seconds: 1,
            expires_at_unix_seconds: 2,
            models: vec![codex_whisply::CatalogModel {
                id: codex_whisply::ModelId::parse("gpt-5.6-sol").expect("stable model id"),
                display_name: "GPT 5.6 Sol".to_string(),
                short_name: "GPT Sol".to_string(),
                description: "Flagship model".to_string(),
                provider_family: "openai".to_string(),
                route_revision: "opaque-route-revision-1".to_string(),
                capabilities: codex_whisply::ModelCapabilities {
                    input_modalities: vec!["text".to_string(), "image".to_string()],
                    output_modalities: vec!["text".to_string()],
                    supports_tool_calls: true,
                    supports_images: true,
                    supports_structured_output: true,
                    supports_safe_reasoning_summary: true,
                    supports_computer_use: true,
                    supports_native_visible_progress: false,
                    context_limit: 1_050_000,
                    output_limit: 4_096,
                },
                allowed_profiles: vec!["general".to_string(), "workspace".to_string()],
                allowed_reasoning_efforts: vec!["low".to_string(), "high".to_string()],
                availability: codex_whisply::ModelAvailability::Available,
                subscription_availability: codex_whisply::SubscriptionAvailability::Available,
                rate_card_revision: "rate-card-test-1".to_string(),
                usage_multiplier_millis: 2_500,
                recommended_default: true,
                response_start_timeout_seconds: 600,
                response_idle_timeout_seconds: 600,
            }],
        });

        assert_eq!(response.catalog_revision, "catalog-test-1");
        let model = &response.models[0];
        assert_eq!(model.short_name, "GPT Sol");
        assert_eq!(model.description, "Flagship model");
        assert_eq!(model.provider_family, "openai");
        assert_eq!(model.route_revision, "opaque-route-revision-1");
        assert_eq!(
            model.capabilities.input_modalities,
            vec!["text".to_string(), "image".to_string()]
        );
        assert_eq!(
            model.capabilities.output_modalities,
            vec!["text".to_string()]
        );
        assert_eq!(model.capabilities.output_limit, 4_096);
        assert!(model.capabilities.supports_tool_calls);
        assert_eq!(model.usage_multiplier_millis, 2_500);
        assert!(model.recommended_default);
        assert_eq!(
            model.response_start_timeout_seconds, 600,
            "the app-server projection must not discard response timing"
        );
    }
}
