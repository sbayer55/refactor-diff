//! Request bodies the way the Python server read them: `await request.json()` then
//! `body.get(...)` with Python's loose coercions (`int("7")`, `x or default`).

use axum::body::Bytes;
use axum::extract::{FromRequest, Request};
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::error::ApiError;

/// A JSON body; a malformed or non-object body is a JSON 400 rather than a plain-text one.
pub struct ApiJson<T>(pub T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let bytes = Bytes::from_request(req, state)
            .await
            .map_err(|e| ApiError::BadRequest(format!("Couldn't read the request body: {e}")))?;
        serde_json::from_slice(&bytes)
            .map(ApiJson)
            .map_err(|e| ApiError::BadRequest(format!("The request body must be JSON: {e}")))
    }
}

/// Python truthiness of a JSON value (`x or default`).
pub fn truthy(v: &Value) -> bool {
    crate::settings::truthy(v)
}

/// A string field, or `""` for anything else (`body.get("x", "")` used as text).
pub fn str_or_empty(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

/// Python's `int(x)`: integers, floats (truncated), booleans and numeric strings.
pub fn py_int(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| {
            n.as_f64()
                .filter(|f| f.is_finite())
                .map(|f| f.trunc() as i64)
        }),
        Value::Bool(b) => Some(i64::from(*b)),
        Value::String(s) => {
            let s = s.trim();
            let digits = s.strip_prefix('+').unwrap_or(s);
            digits.parse::<i64>().ok()
        }
        _ => None,
    }
}

/// A value Python's `int()` would reject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("not an integer")]
pub struct NotAnInt;

/// `int(x) if x not in (None, "") else None`.
pub fn int_or_empty(v: &Value) -> Result<Option<i64>, NotAnInt> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) if s.is_empty() => Ok(None),
        other => py_int(other).map(Some).ok_or(NotAnInt),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn py_int_coerces_like_python() {
        assert_eq!(py_int(&json!(7)), Some(7));
        assert_eq!(py_int(&json!(7.9)), Some(7));
        assert_eq!(py_int(&json!(true)), Some(1));
        assert_eq!(py_int(&json!(" 42 ")), Some(42));
        assert_eq!(py_int(&json!("+3")), Some(3));
        assert_eq!(py_int(&json!("-3")), Some(-3));
        assert_eq!(py_int(&json!("7.5")), None);
        assert_eq!(py_int(&json!("")), None);
        assert_eq!(py_int(&json!(null)), None);
        assert_eq!(py_int(&json!([1])), None);
    }

    #[test]
    fn int_or_empty_treats_null_and_empty_as_absent() {
        assert_eq!(int_or_empty(&json!(null)), Ok(None));
        assert_eq!(int_or_empty(&json!("")), Ok(None));
        assert_eq!(int_or_empty(&json!("7")), Ok(Some(7)));
        assert_eq!(int_or_empty(&json!("x")), Err(NotAnInt));
    }
}
