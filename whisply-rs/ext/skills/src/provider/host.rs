use std::collections::HashMap;

use whisply_core_skills::SkillLoadOutcome;
use whisply_skills::SkillMetadata;

use crate::catalog::SkillAuthority;
use crate::catalog::SkillCatalog;
use crate::catalog::SkillCatalogEntry;
use crate::catalog::SkillPackageId;
use crate::catalog::SkillProviderError;
use crate::catalog::SkillReadResult;
use crate::catalog::SkillResourceId;
use crate::catalog::SkillSearchResult;
use crate::catalog::SkillSourceKind;
use crate::host_snapshot::HostSkillsSnapshot;
use crate::provider::SkillListQuery;
use crate::provider::SkillProvider;
use crate::provider::SkillProviderFuture;
use crate::provider::SkillReadRequest;
use crate::provider::SkillSearchRequest;

const HOST_AUTHORITY_ID: &str = "host";

/// Host-owned skill provider backed by an immutable service snapshot.
///
/// Discovery and caching belong to `HostSkillsService`; this provider only maps a
/// snapshot into the authority-aware catalog/read contract.
#[derive(Clone, Default)]
pub struct HostSkillProvider;

impl HostSkillProvider {
    pub fn new() -> Self {
        Self
    }
}

impl SkillProvider for HostSkillProvider {
    fn list(&self, query: SkillListQuery) -> SkillProviderFuture<'_, SkillCatalog> {
        Box::pin(async move {
            let Some(host_snapshot) = query.host_snapshot else {
                return Err(SkillProviderError::new(
                    "host skill provider requires a host skills snapshot",
                ));
            };

            Ok(catalog_from_outcome(host_snapshot.outcome()))
        })
    }

    fn read(&self, request: SkillReadRequest) -> SkillProviderFuture<'_, SkillReadResult> {
        Box::pin(async move {
            let Some(host_snapshot) = request.host_snapshot else {
                return Err(SkillProviderError::new(
                    "host skill provider requires a host skills snapshot",
                ));
            };
            let Some(target) =
                resolve_host_resource(&host_snapshot, &request.package, request.resource.as_str())
            else {
                return Err(SkillProviderError::new(format!(
                    "host skill resource is not loaded: {}",
                    request.resource.as_str()
                )));
            };

            let contents = match &target {
                HostResource::MainPrompt(skill) => host_snapshot.read_skill_text(skill).await,
                HostResource::PackageFile { skill, relative } => {
                    host_snapshot
                        .read_skill_resource_text(skill, relative)
                        .await
                }
            }
            .map_err(|err| {
                SkillProviderError::new(format!(
                    "failed to read host skill resource {}: {err}",
                    request.resource.as_str()
                ))
            })?;

            Ok(SkillReadResult {
                resource: request.resource,
                contents,
            })
        })
    }

    fn search(&self, _request: SkillSearchRequest) -> SkillProviderFuture<'_, SkillSearchResult> {
        Box::pin(async { Ok(SkillSearchResult::default()) })
    }
}

/// What a requested host resource turned out to be.
enum HostResource<'a> {
    MainPrompt(&'a SkillMetadata),
    PackageFile {
        skill: &'a SkillMetadata,
        relative: String,
    },
}

/// Maps a requested resource onto the package that owns it.
///
/// Callers address a reference three ways and all of them reach the same file:
/// prefixed by the package id, which is how the executor authority already
/// binds package resources; by the absolute path under the package directory,
/// which is what `SKILL.md`'s own location suggests; or by a bare relative path
/// when the request already names the package.
///
/// Only the owning package is consulted, and the longest matching prefix wins
/// so a package nested inside another is not claimed by its parent.
///
/// This resolves ownership only. Whether the file is really inside the package
/// once symlinks are followed is decided by the read itself.
fn resolve_host_resource<'a>(
    snapshot: &'a HostSkillsSnapshot,
    package: &SkillPackageId,
    resource: &str,
) -> Option<HostResource<'a>> {
    let requested = resource.replace('\\', "/");
    let package_id = package.0.replace('\\', "/");
    let mut best: Option<(usize, HostResource<'a>)> = None;

    for skill in &snapshot.outcome().skills {
        let skill_path = skill.path_to_skills_md.to_string_lossy().replace('\\', "/");
        if skill_path == requested {
            return Some(HostResource::MainPrompt(skill));
        }

        let package_directory = skill_path.rsplit_once('/').map(|(directory, _)| directory);
        let owns_request = skill_path == package_id;
        let candidate = [Some(skill_path.as_str()), package_directory]
            .into_iter()
            .flatten()
            .filter_map(|prefix| {
                requested
                    .strip_prefix(prefix)
                    .and_then(|rest| rest.strip_prefix('/'))
                    .map(|relative| (prefix.len(), relative))
            })
            .chain((owns_request && !requested.starts_with('/')).then_some((0, requested.as_str())))
            .filter(|(_, relative)| !relative.is_empty())
            .max_by_key(|(matched, _)| *matched);

        let Some((matched, relative)) = candidate else {
            continue;
        };
        if best.as_ref().is_none_or(|(best, _)| matched > *best) {
            best = Some((
                matched,
                HostResource::PackageFile {
                    skill,
                    relative: relative.to_string(),
                },
            ));
        }
    }

    best.map(|(_, resource)| resource)
}

fn catalog_from_outcome(outcome: &SkillLoadOutcome) -> SkillCatalog {
    let root_order_by_path = outcome
        .skill_roots_in_discovery_order()
        .enumerate()
        .map(|(index, root)| (root.as_path(), index))
        .collect::<HashMap<_, _>>();
    let mut catalog = SkillCatalog {
        entries: Vec::new(),
        warnings: outcome
            .errors
            .iter()
            .map(|err| {
                format!(
                    "Failed to load skill at {}: {}",
                    err.path.display(),
                    err.message
                )
            })
            .collect(),
    };

    for (skill, enabled) in outcome.skills_with_enabled() {
        let mut entry = catalog_entry_from_skill(skill, enabled);
        if let Some(discovery_path) =
            outcome.skill_discovery_path_for_path(&skill.path_to_skills_md)
        {
            entry = entry.with_display_path(discovery_path.to_string_lossy().replace('\\', "/"));
        }
        if let Some(root) = outcome.skill_root_for_path(&skill.path_to_skills_md) {
            entry = entry.with_display_path_root(root.to_string_lossy().replace('\\', "/"));
            if let Some(root_order) = root_order_by_path.get(root.as_path()) {
                entry = entry.with_display_path_root_order(*root_order);
            }
        }
        catalog.push_entry(entry);
    }

    catalog
}

fn catalog_entry_from_skill(skill: &SkillMetadata, enabled: bool) -> SkillCatalogEntry {
    let skill_path = skill.path_to_skills_md.to_string_lossy().into_owned();
    let display_path = skill_path.replace('\\', "/");
    let mut entry = SkillCatalogEntry::new(
        SkillPackageId(skill_path.clone()),
        SkillAuthority::new(SkillSourceKind::Host, HOST_AUTHORITY_ID),
        skill.name.clone(),
        skill.description.clone(),
        SkillResourceId::new(skill_path),
    )
    .with_short_description(skill.short_description.clone())
    .with_display_path(display_path)
    .with_prompt_scope(skill.scope)
    .with_dependencies(skill.dependencies.clone());

    if !enabled {
        entry = entry.disabled();
    }
    if !skill.allows_implicit_invocation() {
        entry = entry.hidden_from_prompt();
    }

    entry
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod tests;
