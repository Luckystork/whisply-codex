use std::sync::Arc;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use pretty_assertions::assert_eq;
use tokio::sync::Semaphore;
use whisply_core_skills::loader::SkillRoot;
use whisply_core_skills::loader::load_skills_from_roots;
use whisply_exec_server::LOCAL_FS;
use whisply_protocol::protocol::SkillScope;
use whisply_utils_absolute_path::AbsolutePathBuf;

use super::HostSkillProvider;
use super::catalog_from_outcome;
use crate::catalog::SkillAuthority;
use crate::catalog::SkillPackageId;
use crate::catalog::SkillResourceId;
use crate::catalog::SkillSourceKind;
use crate::host_snapshot::HostSkillsSnapshot;
use crate::provider::SkillProvider;
use crate::provider::SkillReadRequest;

/// Loads a directory as a user skill root and returns a provider-ready snapshot.
async fn host_snapshot_for_root(
    root: &AbsolutePathBuf,
) -> Result<Arc<HostSkillsSnapshot>, Box<dyn std::error::Error>> {
    let outcome = load_skills_from_roots(
        [SkillRoot {
            path: root.clone(),
            scope: SkillScope::User,
            file_system: Arc::clone(&LOCAL_FS),
            plugin_identity: None,
            plugin_namespace: None,
            plugin_root: None,
            discovery_mode: Default::default(),
        }],
        /*plugin_skill_snapshots*/ None,
        Arc::new(Semaphore::new(1)),
    )
    .await;
    Ok(Arc::new(HostSkillsSnapshot::new(Arc::new(outcome))))
}

async fn read_host_resource(
    snapshot: &Arc<HostSkillsSnapshot>,
    package: &str,
    resource: &str,
) -> Result<String, String> {
    HostSkillProvider::new()
        .read(SkillReadRequest {
            authority: SkillAuthority::new(SkillSourceKind::Host, super::HOST_AUTHORITY_ID),
            package: SkillPackageId(package.to_string()),
            resource: SkillResourceId::new(resource),
            resolved_executor_roots: Vec::new(),
            sandbox: None,
            host_snapshot: Some(Arc::clone(snapshot)),
            mcp_resources: None,
        })
        .await
        .map(|result| result.contents)
        .map_err(|err| err.message)
}

/// A skill is a package, not a single file: progressive disclosure only works
/// if the model can pull in the references that `SKILL.md` points at.
///
/// Every way a caller might spell that reference has to land on the same file,
/// because the catalog shows the `SKILL.md` path while the prompt body refers
/// to its references relatively.
#[tokio::test]
async fn host_read_serves_reference_files_inside_the_package()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let skill_dir = root.path().join("demo");
    std::fs::create_dir_all(skill_dir.join("references"))?;
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: demo\ndescription: Demo skill.\n---\nSee references/guide.md\n",
    )?;
    std::fs::write(skill_dir.join("references/guide.md"), "# Deep guide\n")?;

    let root = AbsolutePathBuf::try_from(std::fs::canonicalize(root.path())?)?;
    let snapshot = host_snapshot_for_root(&root).await?;
    let package = root.join("demo/SKILL.md").to_string_lossy().to_string();
    let package_directory = root.join("demo").to_string_lossy().to_string();

    for resource in [
        format!("{package_directory}/references/guide.md"),
        format!("{package}/references/guide.md"),
        "references/guide.md".to_string(),
    ] {
        assert_eq!(
            read_host_resource(&snapshot, &package, &resource).await?,
            "# Deep guide\n",
            "reference should be readable as {resource}"
        );
    }
    assert!(
        read_host_resource(&snapshot, &package, &package)
            .await?
            .contains("See references/guide.md"),
        "the main prompt must still be readable"
    );

    Ok(())
}

/// The package boundary is the authority boundary. A file the package does not
/// own is not reachable just because a path was spelled relative to one.
#[tokio::test]
async fn host_read_refuses_paths_outside_the_package() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let skill_dir = root.path().join("demo");
    std::fs::create_dir_all(&skill_dir)?;
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: demo\ndescription: Demo skill.\n---\n# Demo\n",
    )?;
    std::fs::write(root.path().join("secret.txt"), "classified\n")?;

    let root = AbsolutePathBuf::try_from(std::fs::canonicalize(root.path())?)?;
    let snapshot = host_snapshot_for_root(&root).await?;
    let package = root.join("demo/SKILL.md").to_string_lossy().to_string();

    for resource in [
        root.join("demo/../secret.txt")
            .to_string_lossy()
            .to_string(),
        root.join("secret.txt").to_string_lossy().to_string(),
        "../secret.txt".to_string(),
        "/etc/hosts".to_string(),
    ] {
        let error = read_host_resource(&snapshot, &package, &resource)
            .await
            .expect_err("resource outside the package must be refused");
        assert!(
            !error.contains("classified"),
            "refusal must not leak contents: {error}"
        );
    }

    Ok(())
}

/// Lexical containment is not enough: a symlink planted inside a package would
/// otherwise turn a package-relative read into an arbitrary file read.
#[cfg(unix)]
#[tokio::test]
async fn host_read_refuses_symlinks_that_escape_the_package()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let secret = outside.path().join("secret.txt");
    std::fs::write(&secret, "classified\n")?;

    let skill_dir = root.path().join("demo");
    std::fs::create_dir_all(&skill_dir)?;
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: demo\ndescription: Demo skill.\n---\n# Demo\n",
    )?;
    std::os::unix::fs::symlink(&secret, skill_dir.join("escape.txt"))?;

    let root = AbsolutePathBuf::try_from(std::fs::canonicalize(root.path())?)?;
    let snapshot = host_snapshot_for_root(&root).await?;
    let package = root.join("demo/SKILL.md").to_string_lossy().to_string();
    let escape = root.join("demo/escape.txt").to_string_lossy().to_string();

    let error = read_host_resource(&snapshot, &package, &escape)
        .await
        .expect_err("a symlink out of the package must be refused");
    assert!(
        !error.contains("classified"),
        "refusal must not leak contents: {error}"
    );

    Ok(())
}

#[tokio::test]
async fn host_catalog_entries_carry_their_render_metadata() -> Result<(), Box<dyn std::error::Error>>
{
    let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let root = std::env::temp_dir().join(format!(
        "codex-skills-extension-host-provider-{}-{unique}",
        std::process::id()
    ));
    let skill_path = root.join("demo").join("SKILL.md");
    std::fs::create_dir_all(
        skill_path
            .parent()
            .ok_or("skill path should have a parent")?,
    )?;
    std::fs::write(
        &skill_path,
        "---\nname: demo\ndescription: Demo skill.\n---\n# Demo\n",
    )?;
    let root = AbsolutePathBuf::try_from(std::fs::canonicalize(root)?)?;
    let outcome = load_skills_from_roots(
        [SkillRoot {
            path: root.clone(),
            scope: SkillScope::User,
            file_system: Arc::clone(&LOCAL_FS),
            plugin_identity: None,
            plugin_namespace: None,
            plugin_root: None,
            discovery_mode: Default::default(),
        }],
        /*plugin_skill_snapshots*/ None,
        Arc::new(Semaphore::new(1)),
    )
    .await;

    let catalog = catalog_from_outcome(&outcome);

    assert_eq!(catalog.entries.len(), 1);
    assert_eq!(
        (
            catalog.entries[0].display_path_root(),
            catalog.entries[0].prompt_scope(),
        ),
        (
            Some(root.to_string_lossy().replace('\\', "/").as_str()),
            Some(SkillScope::User),
        )
    );

    std::fs::remove_dir_all(root.as_path())?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn host_catalog_preserves_symlinked_skill_discovery_paths()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let source = tempfile::tempdir()?;
    let source_skill_dir = source.path().join("linked-skill");
    std::fs::create_dir_all(&source_skill_dir)?;
    std::fs::write(
        source_skill_dir.join("SKILL.md"),
        "---\nname: linked-skill\ndescription: Linked skill.\n---\n# Linked skill\n",
    )?;
    std::os::unix::fs::symlink(&source_skill_dir, root.path().join("linked-skill"))?;

    let root = AbsolutePathBuf::try_from(std::fs::canonicalize(root.path())?)?;
    let outcome = load_skills_from_roots(
        [SkillRoot {
            path: root.clone(),
            scope: SkillScope::User,
            file_system: Arc::clone(&LOCAL_FS),
            plugin_identity: None,
            plugin_namespace: None,
            plugin_root: None,
            discovery_mode: Default::default(),
        }],
        /*plugin_skill_snapshots*/ None,
        Arc::new(Semaphore::new(1)),
    )
    .await;
    let catalog = catalog_from_outcome(&outcome);
    let canonical_path = std::fs::canonicalize(source_skill_dir.join("SKILL.md"))?;
    let discovery_path = root.join("linked-skill/SKILL.md");

    assert_eq!(catalog.entries.len(), 1);
    assert_eq!(
        (
            catalog.entries[0].main_prompt.as_str(),
            catalog.entries[0].display_path.as_deref(),
            catalog.entries[0].display_path_root(),
        ),
        (
            canonical_path.to_string_lossy().as_ref(),
            Some(discovery_path.to_string_lossy().as_ref()),
            Some(root.to_string_lossy().as_ref()),
        )
    );

    Ok(())
}
