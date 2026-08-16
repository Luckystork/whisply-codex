//! WCD-711: the working-directory selector.
//!
//! A thread's working directory is fixed for its lifetime, so this surface
//! chooses the directory for the *next* thread and starts it, in the same way
//! `/new` does. That is why the popup speaks in terms of starting a new chat
//! rather than moving the current one.

use super::*;
use codex_whisply::DirectorySelection;
use std::path::Path;
use std::path::PathBuf;

impl ChatWidget {
    /// Open the popup for choosing which folder Whisply works in.
    pub(crate) fn open_directory_popup(&mut self) {
        let current = self.current_directory_selection();
        let launch_directory = std::env::current_dir().ok();
        let mut items: Vec<SelectionItem> = Vec::new();

        let no_directory_is_current = matches!(current, DirectorySelection::NoDirectory);
        items.push(SelectionItem {
            name: "No directory".to_string(),
            description: Some(
                "Work in a private folder. No project files are read, and nothing outside \
                 the chat is available."
                    .to_string(),
            ),
            is_current: no_directory_is_current,
            is_default: true,
            actions: vec![Box::new(|tx| {
                tx.send(AppEvent::NewSessionInDirectory {
                    selection: DirectorySelection::NoDirectory,
                });
            })],
            dismiss_on_select: true,
            ..Default::default()
        });

        // The terminal Whisply was started from. Offering it by name is the
        // difference between a person being able to get back to their project
        // and having to remember its full path.
        if let Some(launch_directory) = launch_directory.filter(|directory| {
            !matches!(&current, DirectorySelection::Selected { canonical_path }
                if canonical_path == directory)
        }) {
            let label = Self::directory_label(&launch_directory);
            let path = launch_directory.clone();
            items.push(SelectionItem {
                name: format!("{label} — the folder you started in"),
                description: Some(launch_directory.display().to_string()),
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::NewSessionInDirectory {
                        selection: DirectorySelection::Selected {
                            canonical_path: path.clone(),
                        },
                    });
                })],
                dismiss_on_select: true,
                ..Default::default()
            });
        }

        if let DirectorySelection::Selected { canonical_path } = &current {
            items.push(SelectionItem {
                name: format!(
                    "{} — this chat's folder",
                    Self::directory_label(canonical_path)
                ),
                description: Some(canonical_path.display().to_string()),
                is_current: true,
                dismiss_on_select: true,
                ..Default::default()
            });
        }

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Which folder should Whisply work in?".to_string()),
            subtitle: Some(
                "Changing the folder starts a new chat. This one stays available under /resume."
                    .to_string(),
            ),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
        self.request_redraw();
    }

    /// Handle `/directory <absolute-path|none>`.
    ///
    /// The typed form exists because the popup can only offer directories it
    /// can name, and someone who knows where they want to be should not have
    /// to browse for it.
    pub(crate) fn select_directory_by_path(&mut self, argument: String) {
        let argument = argument.trim();
        if matches!(
            argument.to_ascii_lowercase().as_str(),
            "none" | "no" | "no-directory" | "no directory"
        ) {
            self.app_event_tx.send(AppEvent::NewSessionInDirectory {
                selection: DirectorySelection::NoDirectory,
            });
            return;
        }

        let path = PathBuf::from(argument);
        if !path.is_absolute() {
            self.add_error_message(format!(
                "'{argument}' is not an absolute path. Use /directory <absolute-path> or \
                 /directory none."
            ));
            return;
        }

        // Report why a folder cannot be used before starting a thread in it,
        // rather than letting the thread fail to start afterwards.
        match DirectorySelection::select(&path) {
            Ok(selection) => {
                self.app_event_tx
                    .send(AppEvent::NewSessionInDirectory { selection });
            }
            Err(err) => {
                self.add_error_message(format!("Cannot use {}: {err}", path.display()));
            }
        }
    }

    /// What this thread's configured cwd means as a user-visible choice.
    ///
    /// `No directory` is stored as a real directory inside the app's own home,
    /// so the two are told apart by where the directory is rather than by a
    /// separate flag that could disagree with it.
    fn current_directory_selection(&self) -> DirectorySelection {
        if whisply_config::is_inside_runtime_home(
            &self.config.cwd,
            self.config.codex_home.as_path(),
        ) {
            return DirectorySelection::NoDirectory;
        }
        DirectorySelection::Selected {
            canonical_path: self.config.cwd.to_path_buf(),
        }
    }

    fn directory_label(path: &Path) -> String {
        path.file_name()
            .and_then(|name| name.to_str())
            .map_or_else(|| path.display().to_string(), ToString::to_string)
    }
}
