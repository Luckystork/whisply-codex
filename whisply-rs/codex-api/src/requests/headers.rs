use http::HeaderMap;
use http::HeaderValue;
use whisply_protocol::protocol::SessionSource;

pub fn build_session_headers(session_id: Option<String>, thread_id: Option<String>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Some(id) = session_id {
        insert_header(&mut headers, "session-id", &id);
    }
    if let Some(id) = thread_id {
        insert_header(&mut headers, "thread-id", &id);
    }
    headers
}

pub(crate) fn subagent_header(source: &Option<SessionSource>) -> Option<String> {
    let SessionSource::SubAgent(sub) = source.as_ref()? else {
        return None;
    };
    match sub {
        whisply_protocol::protocol::SubAgentSource::Review => Some("review".to_string()),
        whisply_protocol::protocol::SubAgentSource::Compact => Some("compact".to_string()),
        whisply_protocol::protocol::SubAgentSource::MemoryConsolidation => {
            Some("memory_consolidation".to_string())
        }
        whisply_protocol::protocol::SubAgentSource::ThreadSpawn { .. } => {
            Some("collab_spawn".to_string())
        }
        whisply_protocol::protocol::SubAgentSource::Other(label) => Some(label.clone()),
    }
}

/// Whisply's direct gateway accepts only stable classifications, never an
/// arbitrary `SubAgentSource::Other` label. These headers are informational
/// and non-authoritative; policy derives from the authenticated runtime state.
pub(crate) fn whisply_subagent_header(source: &Option<SessionSource>) -> Option<&'static str> {
    match source.as_ref()? {
        SessionSource::SubAgent(subagent) => Some(match subagent {
            whisply_protocol::protocol::SubAgentSource::Review => "review",
            whisply_protocol::protocol::SubAgentSource::Compact => "compact",
            whisply_protocol::protocol::SubAgentSource::MemoryConsolidation => {
                "memory_consolidation"
            }
            whisply_protocol::protocol::SubAgentSource::ThreadSpawn { .. } => "collab_spawn",
            whisply_protocol::protocol::SubAgentSource::Other(_) => "other",
        }),
        SessionSource::Internal(
            whisply_protocol::protocol::InternalSessionSource::MemoryConsolidation,
        ) => Some("memory_consolidation"),
        SessionSource::Cli
        | SessionSource::VSCode
        | SessionSource::Exec
        | SessionSource::Mcp
        | SessionSource::Custom(_)
        | SessionSource::Unknown => None,
    }
}

pub(crate) fn insert_header(headers: &mut HeaderMap, name: &str, value: &str) {
    if let (Ok(header_name), Ok(header_value)) = (
        name.parse::<http::HeaderName>(),
        HeaderValue::from_str(value),
    ) {
        headers.insert(header_name, header_value);
    }
}
