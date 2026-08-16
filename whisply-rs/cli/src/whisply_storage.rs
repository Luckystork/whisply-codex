//! `whisply storage` — what Whisply keeps on this machine.
//!
//! Three answers: what is stored, a copy of it, and a bounded way to delete
//! the parts that are accumulation. Every path decision belongs to
//! `codex_whisply::local_storage`; this module names categories and prints.
//! Deletion is a dry run unless the caller says otherwise, because a person
//! should be able to see the number before agreeing to it.

use std::path::PathBuf;

use clap::Args;
use codex_whisply::StorageCategoryId;
use codex_whisply::StorageInventory;
use serde::Serialize;
use whisply_utils_cli::CliConfigOverrides;

use crate::whisply_skills::managed_home;

#[derive(Debug, Args)]
pub(crate) struct StorageCommand {
    #[command(subcommand)]
    action: StorageSubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum StorageSubcommand {
    /// Show what Whisply stores locally, category by category.
    List(StorageListArgs),
    /// Copy your local Whisply data into a new empty directory.
    Export(StorageExportArgs),
    /// Delete a category of accumulated local data.
    Clean(StorageCleanArgs),
}

#[derive(Debug, Args)]
struct StorageListArgs {
    /// Emit machine-readable JSON instead of a table.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct StorageExportArgs {
    /// Absolute path to a new or empty directory outside the Whisply home.
    #[arg(long, value_name = "DIR")]
    out: PathBuf,

    /// Emit machine-readable JSON instead of a sentence.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct StorageCleanArgs {
    /// One or more category ids from `whisply storage list`.
    #[arg(value_name = "CATEGORY", required = true)]
    categories: Vec<String>,

    /// Actually delete. Without this the command only reports what it would remove.
    #[arg(long)]
    yes: bool,

    /// Emit machine-readable JSON instead of sentences.
    #[arg(long)]
    json: bool,
}

#[derive(Serialize)]
struct CategoryReport {
    id: &'static str,
    title: &'static str,
    description: &'static str,
    bytes: u64,
    items: usize,
    present: bool,
    removable: bool,
    exportable: bool,
    paths: Vec<String>,
    /// False when `bytes` and `items` are a floor rather than a total.
    complete: bool,
}

#[derive(Serialize)]
struct InventoryReport {
    home: String,
    total_bytes: u64,
    categories: Vec<CategoryReport>,
    complete: bool,
}

#[derive(Serialize)]
struct RemovalReport {
    id: &'static str,
    title: &'static str,
    bytes: u64,
    items: usize,
    performed: bool,
    paths: Vec<String>,
    complete: bool,
}

#[derive(Serialize)]
struct ExportReport {
    destination: String,
    categories: Vec<&'static str>,
    bytes: u64,
    items: usize,
}

pub(crate) async fn run(
    command: StorageCommand,
    root_config_overrides: &CliConfigOverrides,
) -> anyhow::Result<()> {
    let home = managed_home()?;
    match command.action {
        StorageSubcommand::List(args) => {
            let inventory = codex_whisply::local_storage_inventory(&home)
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&report(&inventory))?);
            } else {
                print_inventory(&inventory);
            }
        }
        StorageSubcommand::Export(args) => {
            let outcome = codex_whisply::export_local_storage(&home, &args.out)
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            let report = ExportReport {
                destination: outcome.destination.display().to_string(),
                categories: outcome
                    .categories
                    .iter()
                    .map(|id| id.as_str())
                    .collect::<Vec<_>>(),
                bytes: outcome.bytes,
                items: outcome.items,
            };
            if args.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "Exported {} in {} file(s) to {}.",
                    format_bytes(report.bytes),
                    report.items,
                    report.destination
                );
                println!("Your sign-in was not included.");
            }
        }
        StorageSubcommand::Clean(args) => {
            let mut ids = Vec::with_capacity(args.categories.len());
            for name in &args.categories {
                let id = StorageCategoryId::parse(name).ok_or_else(|| {
                    anyhow::anyhow!(
                        "`{name}` is not a storage category. Run `whisply storage list` to see them."
                    )
                })?;
                if !id.is_removable() {
                    anyhow::bail!(
                        "`{name}` is not something Whisply deletes from here: {}",
                        id.description()
                    );
                }
                ids.push(id);
            }

            let mut reports = Vec::with_capacity(ids.len());
            for id in ids {
                if id.needs_owner_removal() {
                    reports.push(clean_by_owner(&home, id, args.yes, root_config_overrides).await?);
                    continue;
                }
                let outcome = codex_whisply::remove_local_storage_category(&home, id, args.yes)
                    .map_err(|error| anyhow::anyhow!("{error}"))?;
                reports.push(RemovalReport {
                    id: id.as_str(),
                    title: id.title(),
                    bytes: outcome.bytes,
                    items: outcome.items,
                    performed: outcome.performed,
                    complete: outcome.complete,
                    paths: outcome
                        .removed_paths
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect(),
                });
            }

            if args.json {
                println!("{}", serde_json::to_string_pretty(&reports)?);
            } else {
                for report in &reports {
                    let amount = format_amount(report.bytes, report.complete);
                    if report.performed {
                        println!(
                            "Deleted {} ({amount} in {} file(s)).",
                            report.title, report.items
                        );
                    } else {
                        println!(
                            "Would delete {} ({amount} in {} file(s)).",
                            report.title, report.items
                        );
                    }
                    if !report.complete {
                        // The removal is not bounded even though the count is,
                        // so this is the direction where more goes than the
                        // number said.
                        println!(
                            "  There is more here than could be counted; everything in it goes."
                        );
                    }
                }
                if !args.yes {
                    println!("Nothing was deleted. Add --yes to go ahead.");
                }
            }
        }
    }
    Ok(())
}

/// Removes a category whose owner has to do it, and reports what was there.
///
/// Memory is the case: its records live in a database that would rebuild the
/// files, so the memory subsystem clears both halves. The size is measured
/// first, from the same inventory the person was shown, because after the
/// clear there is nothing left to count.
async fn clean_by_owner(
    home: &std::path::Path,
    id: StorageCategoryId,
    perform: bool,
    root_config_overrides: &CliConfigOverrides,
) -> anyhow::Result<RemovalReport> {
    let inventory =
        codex_whisply::local_storage_inventory(home).map_err(|error| anyhow::anyhow!("{error}"))?;
    let category = inventory
        .category(id)
        .ok_or_else(|| anyhow::anyhow!("{} is not part of this home", id.title()))?;
    let paths = category
        .paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();

    if perform {
        crate::clear_all_memory_state(root_config_overrides).await?;
    }

    Ok(RemovalReport {
        id: id.as_str(),
        title: id.title(),
        bytes: category.bytes,
        items: category.items,
        performed: perform,
        complete: category.complete,
        paths,
    })
}

fn report(inventory: &StorageInventory) -> InventoryReport {
    InventoryReport {
        home: inventory.home.display().to_string(),
        total_bytes: inventory.total_bytes(),
        categories: inventory
            .categories
            .iter()
            .map(|category| CategoryReport {
                id: category.id.as_str(),
                title: category.id.title(),
                description: category.id.description(),
                bytes: category.bytes,
                items: category.items,
                present: category.present,
                removable: category.id.is_removable(),
                exportable: category.id.is_exportable(),
                complete: category.complete,
                paths: category
                    .paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect(),
            })
            .collect(),
        complete: inventory.is_complete(),
    }
}

fn print_inventory(inventory: &StorageInventory) {
    println!(
        "Whisply stores this on your Mac, in {}:",
        inventory.home.display()
    );
    println!();
    for category in &inventory.categories {
        if !category.present {
            continue;
        }
        let note = if category.id.is_removable() {
            format!(
                "delete with `whisply storage clean {}`",
                category.id.as_str()
            )
        } else {
            "kept".to_string()
        };
        println!(
            "{:<22} {:>12}  {:>6} file(s)  {note}",
            category.id.title(),
            format_amount(category.bytes, category.complete),
            category.items
        );
    }
    println!();
    println!(
        "Total: {}",
        format_amount(inventory.total_bytes(), inventory.is_complete())
    );
    if !inventory.is_complete() {
        println!(
            "Some of this was too large or too deep to finish counting, so the figures marked \
             `at least` are floors."
        );
    }
}

/// Writes a size, and says so when it is a floor rather than a total.
fn format_amount(bytes: u64, complete: bool) -> String {
    if complete {
        format_bytes(bytes)
    } else {
        format!("at least {}", format_bytes(bytes))
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [(&str, u64); 4] = [
        ("GB", 1024 * 1024 * 1024),
        ("MB", 1024 * 1024),
        ("KB", 1024),
        ("bytes", 1),
    ];
    for (label, size) in UNITS {
        if bytes >= size && size > 1 {
            return format!("{:.1} {label}", bytes as f64 / size as f64);
        }
    }
    format!("{bytes} bytes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_accumulated_data_is_offered_for_deletion() {
        let removable: Vec<&str> = StorageCategoryId::ALL
            .into_iter()
            .filter(|id| id.is_removable())
            .map(StorageCategoryId::as_str)
            .collect();
        assert_eq!(
            removable,
            vec![
                "threads",
                "archived-threads",
                "memories",
                "logs",
                "plugin-cache",
                "temporary"
            ]
        );
    }

    #[test]
    fn sizes_read_as_sizes() {
        assert_eq!(format_bytes(512), "512 bytes");
        assert_eq!(format_bytes(2048), "2.0 KB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MB");
    }

    /// A figure the walk could not finish is a floor, and printing it as a
    /// total is the one reading of it that is wrong.
    #[test]
    fn a_size_that_was_not_finished_does_not_read_as_a_total() {
        assert_eq!(format_amount(2048, /*complete*/ true), "2.0 KB");
        assert_eq!(format_amount(2048, /*complete*/ false), "at least 2.0 KB");
    }
}
