// Copyright 2026 Salesforce, Inc. All rights reserved.

pub const POLICY_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/policies_config");
pub const COMMON_CONFIG_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/config");
// Set by `make test` via `export POLICY_REF_NAME=$(cargo anypoint get-policy-implementation-name)`.
// The fallback matches the value produced by that command for this policy.
pub const POLICY_NAME: &str = match option_env!("POLICY_REF_NAME") {
    Some(name) => name,
    None => "claude-bridge-policy-v1-0-impl",
};
