// Copyright 2026 Salesforce, Inc. All rights reserved.

//! Upstream-service helpers for this A2A bridge policy.

use anyhow::{anyhow, Result};
use pdk::hl::{PropertyAccessor, Service, StreamProperties, Uri};
use std::str::FromStr;

const XDS_CLUSTER_NAME: &[&str] = &["xds", "cluster_name"];

/// Build the outbound `Service` that targets the cluster the inbound route
/// resolved (`xds.cluster_name`), so outbound policies bound to that cluster
/// (e.g. credential-injection) apply to a bridge's own callouts. The host is
/// taken from the configured upstream API URL.
pub fn build_upstream_service(
    stream_properties: &StreamProperties,
    upstream_url: &str,
) -> Result<Service> {
    let cluster_name = stream_properties
        .read_property(XDS_CLUSTER_NAME)
        .ok_or_else(|| anyhow!("xds.cluster_name not available — is the route wired?"))?;
    let cluster_name = String::from_utf8_lossy(cluster_name.as_slice()).to_string();
    let uri = Uri::from_str(upstream_url)
        .map_err(|e| anyhow!("invalid upstream URL '{}': {}", upstream_url, e))?;
    Ok(Service::new(&cluster_name, uri))
}
