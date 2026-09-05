// Copyright 2026 Salesforce, Inc. All rights reserved.

//! Bridge-level A2A error factories.
//!
//! Wraps `crate::jsonrpc::RpcError` constructors with bridge-specific
//! defaults. Use these instead of constructing `RpcError` directly so the
//! policy's error shapes stay consistent.

use crate::jsonrpc::RpcError;

/// The method name received was not recognised by this bridge.
pub fn method_not_found(method: &str, valid_methods: Vec<&str>) -> RpcError {
    // TODO: delegate to a v1-specific error constructor once confirmed
    // that all callers speak A2A v1 JSON-RPC transport.
    RpcError::invalid_methods(method.to_string(), valid_methods)
}

/// Required params field was absent or malformed.
pub fn invalid_params(detail: &str) -> RpcError {
    RpcError::invalid_params(detail.to_string())
}

/// An internal bridge error (session store failure, serialization error, etc.).
pub fn internal_error() -> RpcError {
    RpcError::internal_error_rpc()
}
