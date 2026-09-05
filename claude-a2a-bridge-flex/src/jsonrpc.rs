// Copyright 2026 Salesforce, Inc. All rights reserved.

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::Value;

/// JSON-RPC identifier can be string or integer per the spec
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JsonRpcId {
    String(String),
    Int(i64),
    Uint(u64),
}

/// A JSONRPC request object.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonRpcRequest {
    /// The name of the RPC call.
    pub method: String,
    /// Parameters to the RPC call.
    pub params: Option<Box<RawValue>>,
    /// Identifier for this request, which should appear in the response.
    pub id: Option<JsonRpcId>,
    /// jsonrpc field, MUST be "2.0".
    pub jsonrpc: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JsonRpcResponse {
    /// jsonrpc field, MUST be "2.0".
    pub jsonrpc: Option<String>,
    /// Identifier for this response, which should match that of the request.
    pub id: JsonRpcId,
    /// A result if there is one, or [`None`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Box<RawValue>>,
    /// An error if there is one, or [`None`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

/// A JSONRPC error object
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RpcError {
    /// The integer identifier of the error
    pub code: i32,
    /// A string describing the error
    pub message: String,
    /// Additional data specific to the error
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn invalid_json(data: String) -> RpcError {
        RpcError {
            code: -32700,
            message: "Invalid JSON data".to_string(),
            data: Some(serde_json::Value::String(data)),
        }
    }

    pub fn invalid_json_rpc(data: String) -> RpcError {
        RpcError {
            code: -32600,
            message: "Invalid JSON RPC Request".to_string(),
            data: Some(serde_json::Value::String(data)),
        }
    }

    pub fn internal_error_rpc() -> RpcError {
        RpcError {
            code: -32603,
            message: "An unexpected error occurred on the server during processing.".to_string(),
            data: None,
        }
    }

    pub fn invalid_methods(invalid_method: String, valid_methods: Vec<&str>) -> RpcError {
        let error_message = format!(
            "Invalid methods: `{}`. Valid Methods: {}",
            invalid_method,
            valid_methods.join(", ")
        );
        RpcError {
            code: -32601,
            message: "Request payload validation error".to_string(),
            data: Some(serde_json::Value::String(error_message)),
        }
    }

    pub fn invalid_params(error: String) -> RpcError {
        RpcError {
            code: -32602,
            message: "Request payload validation error".to_string(),
            data: Some(serde_json::Value::String(error)),
        }
    }
}
