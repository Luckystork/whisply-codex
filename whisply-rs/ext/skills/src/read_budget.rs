//! What one turn is allowed to pull into itself by following skill references.
//!
//! A skill package is deliberately not expanded into the prompt: the model is
//! given the main prompt and reads the rest as resources. That is what keeps a
//! large package cheap, and it is also what makes the package a graph the model
//! can walk. A reference can name another resource, which can name another, and
//! nothing about a single bounded read stops the walk from continuing until the
//! whole package — or a chain of packages — is in the context window.
//!
//! Three separate things can run away, so three separate things are bounded.
//! Depth bounds how far from the skill the model can follow references. Fan-out
//! bounds how many different resources one turn can open. Total bytes bound
//! what all of it costs together, because many small reads are as expensive as
//! one large one. Each refusal says which bound it was and what the model still
//! has, since the useful next move is almost always to answer from what it has
//! already read.
//!
//! Depth is measured over the chain the model actually walked, not over the
//! directory layout. A resource is at depth 0 when it is the package's main
//! prompt, and at depth n+1 when something served at depth n named it. A
//! resource nobody has named yet is treated as one hop from its package, which
//! is what it is: the model got the name from the catalog, not by following a
//! reference.

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Mutex;

/// How far a chain of references may be followed. The main prompt is depth 0,
/// so this permits a skill that points at a reference sheet that points at an
/// example that points at a fixture, and stops the fifth hop.
pub(crate) const MAX_REFERENCE_DEPTH: usize = 4;
/// Distinct resources one turn may open across every package.
pub(crate) const MAX_TURN_RESOURCES: usize = 32;
/// Distinct resources one turn may open from a single package.
pub(crate) const MAX_PACKAGE_RESOURCES: usize = 16;
/// Total resource content one turn may take, across every page of every read.
pub(crate) const MAX_TURN_READ_BYTES: usize = 2 * 1024 * 1024;

/// One resource as the model names it.
///
/// The key is the model-supplied spelling rather than the resolved path,
/// because the spelling is what a reference in a served page will contain and
/// what the model will pass back on its next call.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ReadKey {
    pub(crate) authority: String,
    pub(crate) package: String,
    pub(crate) resource: String,
}

impl ReadKey {
    pub(crate) fn new(
        authority: impl Into<String>,
        package: impl Into<String>,
        resource: impl Into<String>,
    ) -> Self {
        Self {
            authority: authority.into(),
            package: package.into(),
            resource: normalize_resource(&resource.into()),
        }
    }

    fn sibling(&self, resource: &str) -> Self {
        Self {
            authority: self.authority.clone(),
            package: self.package.clone(),
            resource: normalize_resource(resource),
        }
    }
}

/// A read the budget allowed, carrying the depth it was allowed at so the
/// resources it names can be recorded one hop further out.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AdmittedRead {
    pub(crate) depth: usize,
}

#[derive(Default)]
pub(crate) struct SkillReadBudget {
    turn: Mutex<Option<TurnReads>>,
}

impl SkillReadBudget {
    /// Decides whether this turn may open this resource at all.
    ///
    /// Called before the provider does any work, so an exhausted turn costs a
    /// refusal rather than a filesystem walk. A resource the turn has already
    /// opened is always re-admitted: paging through something the model is
    /// already holding is not new context.
    pub(crate) fn admit(
        &self,
        turn_id: &str,
        key: &ReadKey,
        is_main_prompt: bool,
    ) -> Result<AdmittedRead, String> {
        let mut guard = self
            .turn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let turn = match guard.as_mut() {
            Some(turn) if turn.turn_id == turn_id => turn,
            _ => guard.insert(TurnReads::new(turn_id)),
        };
        turn.admit(key, is_main_prompt)
    }

    /// Records what a read actually cost and what it pointed at.
    ///
    /// Bytes are counted per page served rather than per resource, because a
    /// paged read spends the context window every time it returns.
    pub(crate) fn record(
        &self,
        turn_id: &str,
        key: &ReadKey,
        admitted: AdmittedRead,
        served_contents: &str,
    ) {
        let mut guard = self
            .turn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(turn) = guard.as_mut().filter(|turn| turn.turn_id == turn_id) else {
            return;
        };
        turn.record(key, admitted, served_contents);
    }
}

struct TurnReads {
    turn_id: String,
    opened: HashSet<ReadKey>,
    opened_per_package: HashMap<(String, String), usize>,
    depths: HashMap<ReadKey, usize>,
    bytes: usize,
}

impl TurnReads {
    fn new(turn_id: &str) -> Self {
        Self {
            turn_id: turn_id.to_string(),
            opened: HashSet::new(),
            opened_per_package: HashMap::new(),
            depths: HashMap::new(),
            bytes: 0,
        }
    }

    fn admit(&mut self, key: &ReadKey, is_main_prompt: bool) -> Result<AdmittedRead, String> {
        if self.bytes >= MAX_TURN_READ_BYTES {
            return Err(format!(
                "skill reads in this turn have used the whole {MAX_TURN_READ_BYTES} byte budget; \
                 answer from what you have already read, or ask for a new turn"
            ));
        }

        let depth = if is_main_prompt {
            0
        } else {
            // Unrecorded means the model has the name from the catalog rather
            // than from a page it read, which is one hop from the package.
            self.depths.get(key).copied().unwrap_or(1)
        };
        if depth > MAX_REFERENCE_DEPTH {
            return Err(format!(
                "\"{}\" is {depth} references deep from its skill and the limit is \
                 {MAX_REFERENCE_DEPTH}; read the resources the skill names directly, or answer \
                 from what you have",
                key.resource
            ));
        }

        let already_open = self.opened.contains(key);
        if !already_open {
            if self.opened.len() >= MAX_TURN_RESOURCES {
                return Err(format!(
                    "this turn has already opened {MAX_TURN_RESOURCES} skill resources, which is \
                     the limit; answer from what you have already read"
                ));
            }
            let package_count = self
                .opened_per_package
                .get(&(key.authority.clone(), key.package.clone()))
                .copied()
                .unwrap_or(0);
            if package_count >= MAX_PACKAGE_RESOURCES {
                return Err(format!(
                    "this turn has already opened {MAX_PACKAGE_RESOURCES} resources from \
                     \"{}\", which is the limit for one skill; answer from what you have already \
                     read",
                    key.package
                ));
            }
        }

        Ok(AdmittedRead { depth })
    }

    fn record(&mut self, key: &ReadKey, admitted: AdmittedRead, served_contents: &str) {
        self.bytes = self.bytes.saturating_add(served_contents.len());
        if self.opened.insert(key.clone()) {
            *self
                .opened_per_package
                .entry((key.authority.clone(), key.package.clone()))
                .or_insert(0) += 1;
        }
        self.depths.insert(key.clone(), admitted.depth);

        let child_depth = admitted.depth.saturating_add(1);
        for reference in referenced_resources(served_contents) {
            let child = key.sibling(&reference);
            if child == *key {
                continue;
            }
            // A resource reachable two ways is at its shortest distance from
            // the skill; the deeper path does not make it more expensive.
            let recorded = self.depths.entry(child).or_insert(child_depth);
            *recorded = (*recorded).min(child_depth);
        }
    }
}

fn normalize_resource(resource: &str) -> String {
    resource.trim().trim_start_matches("./").to_string()
}

/// Pulls the resource names a served page points at.
///
/// Deliberately conservative in the other direction from a parser: it is
/// cheaper to record a name that is never read than to miss a reference and
/// let a chain continue uncounted. Anything recorded here only ever sets a
/// depth; it never grants access, which the resolver and its containment check
/// still decide on their own.
fn referenced_resources(contents: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    for candidate in markdown_link_targets(contents).chain(bare_path_tokens(contents)) {
        let candidate = normalize_resource(&candidate);
        if !looks_like_resource(&candidate) {
            continue;
        }
        if seen.insert(candidate.clone()) {
            found.push(candidate);
        }
    }
    found
}

fn markdown_link_targets(contents: &str) -> impl Iterator<Item = String> + '_ {
    contents.split("](").skip(1).filter_map(|rest| {
        let target = rest.split(')').next()?;
        Some(
            target
                .split_whitespace()
                .next()
                .unwrap_or(target)
                .to_string(),
        )
    })
}

fn bare_path_tokens(contents: &str) -> impl Iterator<Item = String> + '_ {
    // A leading dot is left alone: "./tone.md" is a relative reference, and
    // trimming it would turn it into something that reads as absolute.
    contents.split_whitespace().map(|token| {
        token
            .trim_start_matches(['`', '"', '\'', '(', '[', '<', '*'])
            .trim_end_matches(['`', '"', '\'', ')', ']', '>', '*', ',', ';', ':', '.'])
            .to_string()
    })
}

fn looks_like_resource(candidate: &str) -> bool {
    if candidate.is_empty() || candidate.len() > 512 {
        return false;
    }
    if candidate.contains("://") || candidate.starts_with('#') || candidate.starts_with("mailto:") {
        return false;
    }
    // Markup left inside a token means the token is a fragment of a sentence
    // rather than a path, and the link scan already found the real target.
    if candidate.chars().any(|character| {
        character.is_whitespace()
            || character.is_control()
            || matches!(character, '|' | '(' | ')' | '[' | ']' | '{' | '}' | '`')
    }) {
        return false;
    }
    let extension = candidate
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase());
    matches!(
        extension.as_deref(),
        Some(
            "md" | "markdown"
                | "txt"
                | "json"
                | "yaml"
                | "yml"
                | "toml"
                | "csv"
                | "py"
                | "sh"
                | "bash"
                | "zsh"
                | "js"
                | "ts"
                | "rs"
                | "sql"
                | "html"
                | "xml"
        )
    )
}

#[cfg(test)]
#[path = "read_budget_tests.rs"]
mod tests;
