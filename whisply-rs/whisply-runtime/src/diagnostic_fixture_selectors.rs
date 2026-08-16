//! Catalog-bound selector projections for deterministic presentation fixtures.
//!
//! These selectors intentionally have no caller-provided IDs and no route to
//! a real account, model catalog, profile, tool, provider, or broker. They
//! give a future fixture host one exact, reproducible synthetic selection set
//! to validate before it renders a catalog scenario.

use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;

use crate::DiagnosticFixtureAdapterKind;
use crate::DiagnosticFixtureAdapterMode;
use crate::DiagnosticFixtureThreadPlanError;
use crate::DiagnosticScenarioRegistry;
use crate::build_diagnostic_fixture_thread_plan;

/// Schema revision for deterministic fixture selector projections.
pub const DIAGNOSTIC_FIXTURE_SELECTOR_SCHEMA_VERSION: u16 = 1;
/// Stable protocol for fixture-only selector projections.
pub const DIAGNOSTIC_FIXTURE_SELECTOR_PROTOCOL: &str = "whisply.diagnostics.fixture-selectors.v1";

/// Failure to derive or validate a fixture-only selector projection.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum DiagnosticFixtureSelectorError {
    #[error(transparent)]
    FixtureThread(#[from] DiagnosticFixtureThreadPlanError),
    #[error("The deterministic fixture selector projection is invalid.")]
    InvalidSelectors,
}

/// The only account states a deterministic fixture may select. Neither state
/// carries an account identifier, login handle, session, or credential.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticFixtureAccountSelector {
    None,
    SyntheticLocalAccount,
}

/// A fixed simulated model choice. It is deliberately not a model-catalog ID
/// and cannot cause a provider request or Usage reservation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticFixtureModelSelector {
    SimulatedPresentation,
}

/// A fixture presentation profile reflects only the catalog's synthetic
/// subscription copy. It is never an entitlement, rollout, or paid profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticFixtureProfileSelector {
    SyntheticPresentation,
}

/// One fixture tool projection. `adapter` names a fixed no-op category rather
/// than a product tool descriptor or executable handle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticFixtureToolSelector {
    pub id: String,
    pub adapter: DiagnosticFixtureAdapterKind,
    pub mode: DiagnosticFixtureAdapterMode,
    pub can_execute: bool,
    pub can_resolve_real_authority: bool,
}

/// The one exact synthetic account/model/profile/tool selection set for a
/// catalog scenario. It is metadata only; constructing it never launches a
/// fixture, opens a process, reads persistence, or acquires authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticFixtureSelectors {
    pub schema_version: u16,
    pub protocol: String,
    pub fixture_id: String,
    pub scenario_id: String,
    pub account: DiagnosticFixtureAccountSelector,
    pub model: DiagnosticFixtureModelSelector,
    pub profile: DiagnosticFixtureProfileSelector,
    pub subscription_presentation: String,
    pub tools: Vec<DiagnosticFixtureToolSelector>,
}

impl DiagnosticFixtureSelectors {
    /// Rejects any selector drift by regenerating the sole allowed projection
    /// from the immutable scenario catalog.
    pub fn validate(
        &self,
        registry: &DiagnosticScenarioRegistry,
    ) -> Result<(), DiagnosticFixtureSelectorError> {
        if self != &Self::from_catalog(registry, &self.scenario_id)? {
            return Err(DiagnosticFixtureSelectorError::InvalidSelectors);
        }
        Ok(())
    }

    fn from_catalog(
        registry: &DiagnosticScenarioRegistry,
        scenario_id: &str,
    ) -> Result<Self, DiagnosticFixtureSelectorError> {
        let plan = build_diagnostic_fixture_thread_plan(registry, scenario_id)?;
        let account = match plan.fixture.account_state.as_str() {
            "none" | "synthetic-signed-out" => DiagnosticFixtureAccountSelector::None,
            "synthetic-local-account" => DiagnosticFixtureAccountSelector::SyntheticLocalAccount,
            _ => return Err(DiagnosticFixtureSelectorError::InvalidSelectors),
        };

        Ok(Self {
            schema_version: DIAGNOSTIC_FIXTURE_SELECTOR_SCHEMA_VERSION,
            protocol: DIAGNOSTIC_FIXTURE_SELECTOR_PROTOCOL.to_string(),
            fixture_id: plan.fixture_id,
            scenario_id: plan.scenario_id,
            account,
            model: DiagnosticFixtureModelSelector::SimulatedPresentation,
            profile: DiagnosticFixtureProfileSelector::SyntheticPresentation,
            subscription_presentation: plan.fixture.subscription_presentation,
            tools: fixture_tool_selectors(),
        })
    }
}

/// Builds the sole allowed account/model/profile/tool choices for one exact
/// bundled scenario. There are no arguments for a real account, model,
/// profile, tool, endpoint, executable, or credential.
pub fn build_diagnostic_fixture_selectors(
    registry: &DiagnosticScenarioRegistry,
    scenario_id: &str,
) -> Result<DiagnosticFixtureSelectors, DiagnosticFixtureSelectorError> {
    let selectors = DiagnosticFixtureSelectors::from_catalog(registry, scenario_id)?;
    selectors.validate(registry)?;
    Ok(selectors)
}

fn fixture_tool_selectors() -> Vec<DiagnosticFixtureToolSelector> {
    [
        DiagnosticFixtureAdapterKind::Network,
        DiagnosticFixtureAdapterKind::Provider,
        DiagnosticFixtureAdapterKind::Tool,
        DiagnosticFixtureAdapterKind::Usage,
        DiagnosticFixtureAdapterKind::Receipt,
        DiagnosticFixtureAdapterKind::Policy,
        DiagnosticFixtureAdapterKind::Compaction,
    ]
    .into_iter()
    .map(|adapter| DiagnosticFixtureToolSelector {
        id: format!("fixture-{}-noop", adapter_id(adapter)),
        adapter,
        mode: DiagnosticFixtureAdapterMode::FixtureOnlyNoop,
        can_execute: false,
        can_resolve_real_authority: false,
    })
    .collect()
}

fn adapter_id(adapter: DiagnosticFixtureAdapterKind) -> &'static str {
    match adapter {
        DiagnosticFixtureAdapterKind::Network => "network",
        DiagnosticFixtureAdapterKind::Provider => "provider",
        DiagnosticFixtureAdapterKind::Tool => "tool",
        DiagnosticFixtureAdapterKind::Usage => "usage",
        DiagnosticFixtureAdapterKind::Receipt => "receipt",
        DiagnosticFixtureAdapterKind::Policy => "policy",
        DiagnosticFixtureAdapterKind::Compaction => "compaction",
    }
}
