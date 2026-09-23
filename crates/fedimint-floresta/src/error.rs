// SPDX-License-Identifier: MIT OR Apache-2.0

use std::fmt;

/// `getblockhash`/`getblock` for a block the node does not know (e.g. height above tip).
pub const CODE_BLOCK_NOT_FOUND: i64 = -32098;
/// Internal node failure, e.g. no peer to serve an on-demand block fetch.
pub const CODE_NODE_ERROR: i64 = -32091;

/// A JSON-RPC error returned by florestad.
///
/// Floresta's codes differ from Bitcoin Core's (e.g. [`CODE_BLOCK_NOT_FOUND`]
/// where Core returns -8), so the raw code and message are preserved verbatim
/// for operators reading guardian logs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default)]
    pub data: Option<serde_json::Value>,
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "florestad RPC error {}: {}", self.code, self.message)?;
        if let Some(data) = &self.data {
            write!(f, " ({data})")?;
        }
        Ok(())
    }
}

impl std::error::Error for RpcError {}
