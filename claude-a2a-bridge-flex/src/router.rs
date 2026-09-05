// Copyright 2026 Salesforce, Inc. All rights reserved.

//! JSON-RPC envelope parsing and response building for this bridge policy.
//!
//! The policy starts a request by calling `parse_request` and ends it by
//! calling `ok_response` or `error_response`. Nothing in this module knows
//! about any specific upstream platform.

use crate::jsonrpc::{JsonRpcId, JsonRpcRequest, JsonRpcResponse, RpcError};
use pdk::hl::Response;
use serde::Serialize;
use serde_json::value::RawValue;

const CONTENT_TYPE: &str = "content-type";
const APPLICATION_JSON: &str = "application/json";

/// Parse the raw request body as a JSON-RPC 2.0 envelope.
///
/// Returns `Ok(JsonRpcRequest)` on success, or an HTTP 400 `Response`
/// containing a JSON-RPC parse error that the caller can break with directly.
///
/// # Usage
/// ```ignore
/// match router::parse_request(&body) {
///     Ok(rpc) => { /* dispatch on rpc.method */ }
///     Err(response) => return Flow::Break(response),
/// }
/// ```
pub fn parse_request(body: &[u8]) -> Result<JsonRpcRequest, Response> {
    // TODO: detect and handle HTTP+JSON (bare body) transport in addition
    // to JSON-RPC envelope transport, once A2A v1 HTTP+JSON support is required.
    serde_json::from_slice::<JsonRpcRequest>(body).map_err(|e| {
        let err = if e.is_data() {
            RpcError::invalid_json_rpc(e.to_string())
        } else {
            RpcError::invalid_json(e.to_string())
        };
        error_response(None, err)
    })
}

/// Build a JSON-RPC 2.0 success response for the given `result` value.
pub fn ok_response<T: Serialize>(id: Option<JsonRpcId>, result: &T) -> Response {
    // TODO: propagate serialization errors instead of silently returning
    // an empty result. Currently uses `{}` as a safe fallback so the
    // response is always valid JSON-RPC.
    let raw = serde_json::to_string(result)
        .ok()
        .and_then(|s| RawValue::from_string(s).ok());

    let envelope = JsonRpcResponse {
        jsonrpc: Some("2.0".to_string()),
        id: id.unwrap_or(JsonRpcId::Int(0)),
        result: raw,
        error: None,
    };

    let body = serde_json::to_vec(&envelope).unwrap_or_else(|_| b"{}".to_vec());
    Response::new(200)
        .with_headers(vec![(CONTENT_TYPE.to_string(), APPLICATION_JSON.to_string())])
        .with_body(body)
}

/// Build a JSON-RPC 2.0 error response.
pub fn error_response(id: Option<JsonRpcId>, error: RpcError) -> Response {
    let envelope = JsonRpcResponse {
        jsonrpc: Some("2.0".to_string()),
        id: id.unwrap_or(JsonRpcId::Int(0)),
        result: None,
        error: Some(error),
    };

    let body = serde_json::to_vec(&envelope).unwrap_or_else(|_| b"{}".to_vec());
    Response::new(200)
        .with_headers(vec![(CONTENT_TYPE.to_string(), APPLICATION_JSON.to_string())])
        .with_body(body)
}
