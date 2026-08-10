//! Built-in pet asset ownership and cache materialization.
//!
//! Built-in spritesheets ship inside the TUI binary. On first use, this module
//! validates and atomically materializes the selected image into the existing
//! versioned cache under CODEX_HOME. Custom pets remain user-owned local data
//! and never pass through this module.

use std::fs;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use uuid::Uuid;

use super::catalog;

const PET_PACK_VERSION: &str = "v1";
const PET_PACK_DIR: &str = "cache/tui-pets";
const BUNDLED_SPRITESHEETS: &[(&str, &[u8])] = &[
    (
        "codex-spritesheet-v4.webp",
        include_bytes!("../../assets/pets/codex-spritesheet-v4.webp"),
    ),
    (
        "dewey-spritesheet-v4.webp",
        include_bytes!("../../assets/pets/dewey-spritesheet-v4.webp"),
    ),
    (
        "fireball-spritesheet-v4.webp",
        include_bytes!("../../assets/pets/fireball-spritesheet-v4.webp"),
    ),
    (
        "rocky-spritesheet-v4.webp",
        include_bytes!("../../assets/pets/rocky-spritesheet-v4.webp"),
    ),
    (
        "seedy-spritesheet-v4.webp",
        include_bytes!("../../assets/pets/seedy-spritesheet-v4.webp"),
    ),
    (
        "stacky-spritesheet-v4.webp",
        include_bytes!("../../assets/pets/stacky-spritesheet-v4.webp"),
    ),
    (
        "bsod-spritesheet-v4.webp",
        include_bytes!("../../assets/pets/bsod-spritesheet-v4.webp"),
    ),
    (
        "null-signal-spritesheet-v4.webp",
        include_bytes!("../../assets/pets/null-signal-spritesheet-v4.webp"),
    ),
];

pub(crate) fn builtin_spritesheet_path(codex_home: &Path, file: &str) -> PathBuf {
    pack_dir(codex_home).join("assets").join(file)
}

/// Ensure that a built-in pet's spritesheet is present and structurally valid.
///
/// The cache key is the bundled asset filename. If a cached file is missing or
/// invalid, this installs a fresh copy from the embedded release asset and
/// validates its decoded image geometry before exposing it to callers.
pub(crate) async fn ensure_builtin_pet(codex_home: &Path, pet: catalog::BuiltinPet) -> Result<()> {
    let destination = builtin_spritesheet_path(codex_home, pet.spritesheet_file);
    let cache_destination = destination.clone();
    let cache_valid = tokio::task::spawn_blocking(move || {
        validate_cached_spritesheet(&cache_destination).is_ok()
    })
    .await
    .context("join pet spritesheet cache validation task")?;
    if cache_valid {
        return Ok(());
    }

    let bytes = bundled_spritesheet(pet.spritesheet_file)?;
    tokio::task::spawn_blocking(move || {
        materialize_bundled_spritesheet(&destination, pet.spritesheet_file, bytes)
    })
    .await
    .context("join pet spritesheet install task")?
}

fn bundled_spritesheet(file: &str) -> Result<&'static [u8]> {
    BUNDLED_SPRITESHEETS
        .iter()
        .find_map(|(candidate, bytes)| (*candidate == file).then_some(*bytes))
        .ok_or_else(|| anyhow::anyhow!("missing bundled pet spritesheet {file}"))
}

fn pack_dir(codex_home: &Path) -> PathBuf {
    codex_home.join(PET_PACK_DIR).join(PET_PACK_VERSION)
}

fn materialize_bundled_spritesheet(destination: &Path, filename: &str, bytes: &[u8]) -> Result<()> {
    let parent = destination
        .parent()
        .context("pet spritesheet path should include an assets directory")?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;

    let staging = destination.with_file_name(format!(".{filename}.bundle-{}.webp", Uuid::new_v4()));
    fs::write(&staging, bytes).with_context(|| format!("write {}", staging.display()))?;
    if let Err(err) = validate_cached_spritesheet(&staging) {
        let _ = fs::remove_file(&staging);
        return Err(err);
    }

    if install_bundled_spritesheet(&staging, destination).is_ok() {
        return Ok(());
    }

    if validate_cached_spritesheet(destination).is_ok() {
        let _ = fs::remove_file(&staging);
        return Ok(());
    }

    if destination.exists() {
        fs::remove_file(destination)
            .with_context(|| format!("remove {}", destination.display()))?;
    }
    install_bundled_spritesheet(&staging, destination)
}

fn install_bundled_spritesheet(staging: &Path, destination: &Path) -> Result<()> {
    fs::rename(staging, destination).with_context(|| format!("install {}", destination.display()))
}

fn validate_cached_spritesheet(path: &Path) -> Result<()> {
    let (width, height) =
        image::image_dimensions(path).with_context(|| format!("read {}", path.display()))?;
    if width != catalog::SPRITESHEET_WIDTH || height != catalog::SPRITESHEET_HEIGHT {
        bail!(
            "invalid pet spritesheet dimensions for {}: expected {}x{}, got {}x{}",
            path.display(),
            catalog::SPRITESHEET_WIDTH,
            catalog::SPRITESHEET_HEIGHT,
            width,
            height
        );
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn write_test_pack(codex_home: &Path) {
    let assets_dir = pack_dir(codex_home).join("assets");
    fs::create_dir_all(&assets_dir).unwrap();
    for pet in catalog::BUILTIN_PETS {
        let path = assets_dir.join(pet.spritesheet_file);
        catalog::write_test_spritesheet(&path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_spritesheets_cover_the_catalog_with_the_expected_geometry() {
        for pet in catalog::BUILTIN_PETS {
            let image = image::load_from_memory(bundled_spritesheet(pet.spritesheet_file).unwrap())
                .expect("bundled spritesheet should decode");
            assert_eq!(image.width(), catalog::SPRITESHEET_WIDTH);
            assert_eq!(image.height(), catalog::SPRITESHEET_HEIGHT);
        }
    }

    #[tokio::test]
    async fn missing_builtin_asset_is_materialized_from_the_bundled_pack() {
        let dir = tempfile::tempdir().unwrap();
        let pet = catalog::builtin_pet("dewey").unwrap();

        ensure_builtin_pet(dir.path(), pet).await.unwrap();

        let path = builtin_spritesheet_path(dir.path(), pet.spritesheet_file);
        assert_eq!(
            fs::read(&path).unwrap(),
            bundled_spritesheet(pet.spritesheet_file).unwrap()
        );
        validate_cached_spritesheet(&path).unwrap();
    }
}
