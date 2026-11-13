//! Admin token authentication for all /api/* endpoints.
//! Error codes map to docs/ERROR-CODES.md.

use std::collections::HashMap;

#[derive(Debug, PartialEq, Eq)]
pub enum AuthError {
    Required,
    Failed,
}

pub fn check(
    headers: &HashMap<String, String>,
    query: &str,
    expected: &str,
    loopback: bool,
) -> Result<(), AuthError> {
    if expected.is_empty() {
        return if loopback {
            Ok(())
        } else {
            Err(AuthError::Required)
        };
    }
    let mut supplied = headers.get("x-admin-token").cloned().unwrap_or_default();
    if supplied.is_empty() {
        supplied = query_token(query);
    }
    if supplied.is_empty() {
        return Err(AuthError::Required);
    }
    if constant_eq(&supplied, expected) {
        Ok(())
    } else {
        Err(AuthError::Failed)
    }
}

fn query_token(query: &str) -> String {
    for pair in query.split('&') {
        if let Some((key, value)) = pair.split_once('=') {
            if key == "token" {
                return value.to_string();
            }
        }
    }
    String::new()
}

fn constant_eq(a: &str, b: &str) -> bool {
    let (left, right) = (a.as_bytes(), b.as_bytes());
    let mut diff = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        let x = left.get(index).copied().unwrap_or(0);
        let y = right.get(index).copied().unwrap_or(0);
        diff |= (x ^ y) as usize;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(token: &str) -> HashMap<String, String> {
        let mut map = HashMap::new();
        map.insert("x-admin-token".to_string(), token.to_string());
        map
    }

    #[test]
    fn empty_token_allows_loopback_only() {
        assert_eq!(check(&HashMap::new(), "", "", true), Ok(()));
        assert_eq!(
            check(&HashMap::new(), "", "", false),
            Err(AuthError::Required)
        );
    }

    #[test]
    fn token_from_header_or_query() {
        assert_eq!(check(&headers("s3cret"), "", "s3cret", false), Ok(()));
        assert_eq!(
            check(&HashMap::new(), "token=s3cret", "s3cret", false),
            Ok(())
        );
        assert_eq!(
            check(&headers("wrong"), "", "s3cret", false),
            Err(AuthError::Failed)
        );
    }
}
