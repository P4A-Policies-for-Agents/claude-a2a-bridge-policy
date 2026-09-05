// Copyright 2026 Salesforce, Inc. All rights reserved.

use pdk::logger;

pub const ACCESS_LOG: &str = "[accessLog]";

pub fn info(value_string: String) {
    // log-lint: allow-info
    // This file is the `access_log` audit-log abstraction. Callers use `access_log::info`
    // deliberately to record user-requested audit events; troubleshooting logs go via `logger::*!`.
    logger::info!("{} {}", ACCESS_LOG, value_string);
}

pub fn warn(value_string: String) {
    // log-lint: allow-level
    // Format is fixed; actual message comes from the caller via `value_string`.
    logger::warn!("{} {}", ACCESS_LOG, value_string);
}
