use serde::Deserialize;
use serde::Serialize;
use std::cmp::Ordering;
use std::fmt;

/// Namespace used for top-level function and custom tools.
pub const DEFAULT_FUNCTION_NAMESPACE: &str = "functions";

/// The namespace Whisply's own capabilities answer in.
///
/// Namespaces with the same name are coalesced into one group before the model
/// sees them, and the model identifies a tool by nothing but that group and
/// name. So a tool registered here is presented as one of the user-approved
/// Whisply capabilities whether or not it is one.
pub const WHISPLY_FUNCTION_NAMESPACE: &str = "whisply";

/// Whether a namespace presents its tools to the model as Whisply's own.
///
/// Compared without case because the impersonation is what the model reads,
/// not what it can address: `Whisply` cannot receive a call meant for
/// `whisply`, but it can still offer the model something that looks like the
/// product's own capability.
pub fn is_whisply_function_namespace(namespace: &str) -> bool {
    namespace.eq_ignore_ascii_case(WHISPLY_FUNCTION_NAMESPACE)
}

/// Identifies a callable tool, preserving the namespace split when the model
/// provides one.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct ToolName {
    pub name: String,
    pub namespace: Option<String>,
}

impl ToolName {
    pub fn new(namespace: Option<String>, name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            namespace,
        }
    }

    pub fn plain(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            namespace: None,
        }
    }

    pub fn namespaced(namespace: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            namespace: Some(namespace.into()),
        }
    }

    pub fn with_default_namespace(mut self) -> Self {
        if self.namespace.as_deref().is_none_or(str::is_empty) {
            self.namespace = Some(DEFAULT_FUNCTION_NAMESPACE.to_string());
        }
        self
    }

    pub fn is_default_namespace(&self) -> bool {
        matches!(
            self.namespace.as_deref(),
            None | Some("") | Some(DEFAULT_FUNCTION_NAMESPACE)
        )
    }

    /// Whether this name is addressed to the model as a Whisply capability.
    pub fn is_whisply_namespace(&self) -> bool {
        self.namespace
            .as_deref()
            .is_some_and(is_whisply_function_namespace)
    }
}

impl fmt::Display for ToolName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.namespace {
            Some(_) if self.is_default_namespace() => f.write_str(&self.name),
            Some(namespace) => write!(f, "{namespace}{}", self.name),
            None => f.write_str(&self.name),
        }
    }
}

impl Ord for ToolName {
    fn cmp(&self, other: &Self) -> Ordering {
        let lhs = match &self.namespace {
            Some(namespace) => (namespace.as_str(), Some(self.name.as_str())),
            None => (self.name.as_str(), None),
        };
        let rhs = match &other.namespace {
            Some(namespace) => (namespace.as_str(), Some(other.name.as_str())),
            None => (other.name.as_str(), None),
        };
        lhs.cmp(&rhs)
    }
}

impl PartialOrd for ToolName {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl From<String> for ToolName {
    fn from(name: String) -> Self {
        Self::plain(name)
    }
}

impl From<&str> for ToolName {
    fn from(name: &str) -> Self {
        Self::plain(name)
    }
}
