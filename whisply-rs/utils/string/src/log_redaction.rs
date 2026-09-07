//! Keeps credentials and personal identifiers out of anything the product
//! persists locally.
//!
//! Both local log sinks need the same answer: the file log the TUI and `login`
//! write, and the SQLite log store the app-server and TUI keep under the
//! account home. Living in a leaf crate is what lets one scrubber serve both
//! without the low-level state crate taking a dependency on the runtime.
//!
//! The scrubber is deliberately single-pass and pattern-engine-free. It runs on
//! every line the product records, and a regex at that position is both slower
//! and harder to reason about than the small number of shapes that matter.

use std::borrow::Cow;

/// What replaces a redacted value. Kept distinct from an empty string so a
/// reader can tell a removed secret from a field that was genuinely absent.
pub const REDACTED: &str = "<redacted>";

/// Field names whose value is a credential wherever it appears.
const SENSITIVE_KEYS: &[&str] = &[
    "access_token",
    "accesstoken",
    "api_key",
    "apikey",
    "authorization",
    "client_secret",
    "cookie",
    "id_token",
    "password",
    "private_key",
    "proxy-authorization",
    "refresh_token",
    "secret",
    "session_key",
    "set-cookie",
    "sessiontoken",
    "session_token",
    "x-whisply-usage-session",
    "x-whisply-contextual-execution-lease",
    "executionleasetoken",
    "execution_lease_token",
    "whisplyexamturnreference",
    "whisply_exam_turn_reference",
    "exam_turn_reference",
    "signing_key",
];
// A bare `token` is deliberately absent. Token counts are logged constantly,
// and redacting `tokens = 1843` would cost real diagnostic value to protect a
// number that is not a credential.

/// Authentication schemes that precede the credential rather than being one.
/// Keeping the scheme makes a redacted line still say what kind of auth it was.
const AUTH_SCHEMES: &[&str] = &["Bearer", "Basic", "Digest", "Token"];

/// Redacts one line of log output.
///
/// Returns the input untouched when nothing matched, so the common case does
/// not allocate.
pub fn redact_log_line(line: &str) -> Cow<'_, str> {
    let stage = redact_keyed_values(line);
    let stage = match redact_scheme_credentials(stage.as_ref()) {
        Cow::Borrowed(_) => stage,
        Cow::Owned(owned) => Cow::Owned(owned),
    };
    match redact_standalone_secrets(stage.as_ref()) {
        Cow::Borrowed(_) => stage,
        Cow::Owned(owned) => Cow::Owned(owned),
    }
}

/// Replaces the credential after an authentication scheme that appears with no
/// field name in front of it.
///
/// A call site that logs a whole request line or an error body writes
/// `Bearer <credential>` with nothing the keyed pass can match on, and the
/// credential's own shape is opaque by design, so the standalone pass cannot
/// recognise it either. The scheme is the announcement, and it is enough.
fn redact_scheme_credentials(line: &str) -> Cow<'_, str> {
    let bytes = line.as_bytes();
    let mut out: Option<String> = None;
    let mut copied = 0;
    let mut index = 0;

    while index < bytes.len() {
        let is_word_start = index == 0 || !is_key_byte(bytes[index - 1]);
        let Some(scheme_length) = (if is_word_start {
            auth_scheme_at(bytes, index)
        } else {
            None
        }) else {
            index += 1;
            continue;
        };

        let mut cursor = index + scheme_length;
        while cursor < bytes.len() && bytes[cursor] == b' ' {
            cursor += 1;
        }
        let value_end = value_end_from(bytes, cursor);
        if value_end > cursor && looks_like_opaque_credential(&line[cursor..value_end]) {
            let output = out.get_or_insert_with(String::new);
            output.push_str(&line[copied..cursor]);
            output.push_str(REDACTED);
            copied = value_end;
        }
        index = value_end.max(cursor);
    }

    match out {
        Some(mut output) => {
            output.push_str(&line[copied..]);
            Cow::Owned(output)
        }
        None => Cow::Borrowed(line),
    }
}

/// Whether a run after an authentication scheme is a credential rather than the
/// next word of an English sentence. Length alone would redact
/// `Token authentication failed`; requiring the run to look machine-generated
/// keeps that line readable.
fn looks_like_opaque_credential(run: &str) -> bool {
    const MIN_CREDENTIAL_LENGTH: usize = 12;
    if run.len() < MIN_CREDENTIAL_LENGTH || run == REDACTED {
        return false;
    }
    if !run.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+' | b'/' | b'=')
    }) {
        return false;
    }
    let has_digit = run.bytes().any(|byte| byte.is_ascii_digit());
    let has_symbol = run
        .bytes()
        .any(|byte| matches!(byte, b'-' | b'_' | b'.' | b'+' | b'/' | b'='));
    let mixed_case = run.bytes().any(|byte| byte.is_ascii_uppercase())
        && run.bytes().any(|byte| byte.is_ascii_lowercase());
    has_digit || has_symbol || mixed_case
}

/// Replaces the value of any `key=value` or `key: value` whose key names a
/// credential, whatever quoting or separator the emitting call site chose.
fn redact_keyed_values(line: &str) -> Cow<'_, str> {
    let bytes = line.as_bytes();
    let mut out: Option<String> = None;
    let mut copied = 0;
    let mut index = 0;

    while index < bytes.len() {
        let Some(key_length) = sensitive_key_at(bytes, index) else {
            index += 1;
            continue;
        };
        let after_key = index + key_length;
        let Some(value_start) = value_start_after_key(bytes, after_key) else {
            index = after_key;
            continue;
        };

        let output = out.get_or_insert_with(String::new);
        output.push_str(&line[copied..value_start]);
        copied = value_start;

        // An authentication scheme is a description, not the credential. Saying
        // `Bearer <redacted>` keeps the line diagnostic; `<redacted>` alone
        // loses the one part that was safe to read.
        let mut cursor = value_start;
        if let Some(scheme_length) = auth_scheme_at(bytes, cursor) {
            cursor += scheme_length;
            while cursor < bytes.len() && bytes[cursor] == b' ' {
                cursor += 1;
            }
            output.push_str(&line[copied..cursor]);
            copied = cursor;
        }

        let value_end = value_end_from(bytes, cursor);
        if value_end > cursor {
            output.push_str(REDACTED);
            copied = value_end;
        }
        index = value_end.max(cursor + 1);
    }

    match out {
        Some(mut output) => {
            output.push_str(&line[copied..]);
            Cow::Owned(output)
        }
        None => Cow::Borrowed(line),
    }
}

/// Replaces values that are a credential or a personal identifier by their own
/// shape, with no key to announce them: JSON web tokens, provider API keys, and
/// email addresses.
///
/// This is the half that survives a call site inventing a new field name, which
/// is the failure the keyed pass cannot see coming.
fn redact_standalone_secrets(line: &str) -> Cow<'_, str> {
    let bytes = line.as_bytes();
    let mut out: Option<String> = None;
    let mut copied = 0;
    let mut index = 0;
    let mut run_start = usize::MAX;

    while index <= bytes.len() {
        if index < bytes.len() && is_secret_run_byte(bytes[index]) {
            if run_start == usize::MAX {
                run_start = index;
            }
            index += 1;
            continue;
        }
        if run_start != usize::MAX && looks_like_secret(&line[run_start..index]) {
            let output = out.get_or_insert_with(String::new);
            output.push_str(&line[copied..run_start]);
            output.push_str(REDACTED);
            copied = index;
        }
        run_start = usize::MAX;
        index += 1;
    }

    match out {
        Some(mut output) => {
            output.push_str(&line[copied..]);
            Cow::Owned(output)
        }
        None => Cow::Borrowed(line),
    }
}

fn is_secret_run_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+' | b'@' | b'%')
}

fn looks_like_secret(run: &str) -> bool {
    looks_like_jwt(run) || looks_like_provider_key(run) || looks_like_email(run)
}

/// A JSON web token: three base64url segments, the first of which is a JSON
/// object and so always begins `eyJ`.
fn looks_like_jwt(run: &str) -> bool {
    let mut segments = run.split('.');
    let (Some(header), Some(payload), Some(signature), None) = (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    ) else {
        return false;
    };
    header.starts_with("eyJ")
        && header.len() >= 8
        && payload.len() >= 8
        && !signature.is_empty()
        && [header, payload, signature]
            .iter()
            .all(|segment| segment.bytes().all(is_base64url_byte))
}

fn is_base64url_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
}

/// A provider API key, which is a recognisable prefix and a long opaque tail.
fn looks_like_provider_key(run: &str) -> bool {
    for prefix in [
        "sk-", "sk_", "pk_", "rk_", "whsec_", "ghp_", "gho_", "xoxb-", "wus_",
    ] {
        if let Some(tail) = run.strip_prefix(prefix)
            && tail.len() >= 16
            && tail
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return true;
        }
    }
    false
}

/// An email address. The product already refuses to let one become a path
/// segment; a log file is the same kind of place.
fn looks_like_email(run: &str) -> bool {
    let mut parts = run.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    if local.is_empty() || domain.len() < 3 {
        return false;
    }
    let Some((name, top_level)) = domain.rsplit_once('.') else {
        return false;
    };
    !name.is_empty()
        && top_level.len() >= 2
        && top_level.bytes().all(|byte| byte.is_ascii_alphabetic())
}

/// The length of a sensitive key starting at `index`, when one starts there and
/// is not part of a longer word.
fn sensitive_key_at(bytes: &[u8], index: usize) -> Option<usize> {
    if index > 0 && is_key_byte(bytes[index - 1]) {
        return None;
    }
    for key in SENSITIVE_KEYS {
        let key = key.as_bytes();
        let end = index + key.len();
        if end <= bytes.len()
            && bytes[index..end]
                .iter()
                .zip(key)
                .all(|(left, right)| left.eq_ignore_ascii_case(right))
            && (end == bytes.len() || !is_key_byte(bytes[end]))
        {
            return Some(key.len());
        }
    }
    None
}

fn is_key_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

/// Where the value begins after a key, when the key is actually being assigned
/// one. A bare mention of the word `secret` in a sentence is not a credential.
fn value_start_after_key(bytes: &[u8], mut index: usize) -> Option<usize> {
    if index < bytes.len() && matches!(bytes[index], b'"' | b'\'') {
        index += 1;
    }
    while index < bytes.len() && bytes[index] == b' ' {
        index += 1;
    }
    if index >= bytes.len() || !matches!(bytes[index], b'=' | b':') {
        return None;
    }
    index += 1;
    while index < bytes.len() && matches!(bytes[index], b' ' | b'"' | b'\'') {
        index += 1;
    }
    Some(index)
}

fn auth_scheme_at(bytes: &[u8], index: usize) -> Option<usize> {
    for scheme in AUTH_SCHEMES {
        let scheme = scheme.as_bytes();
        let end = index + scheme.len();
        if end < bytes.len()
            && bytes[index..end]
                .iter()
                .zip(scheme)
                .all(|(left, right)| left.eq_ignore_ascii_case(right))
            && bytes[end] == b' '
        {
            return Some(scheme.len());
        }
    }
    None
}

/// Where a value ends: at whitespace or at a structural character, so the rest
/// of a structured line survives redaction and stays readable.
fn value_end_from(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len()
        && !matches!(
            bytes[index],
            b' ' | b'\t' | b'\r' | b'\n' | b',' | b';' | b'"' | b'\'' | b'}' | b')' | b']' | b'&'
        )
    {
        index += 1;
    }
    index
}

#[cfg(test)]
#[path = "log_redaction_tests.rs"]
mod tests;
