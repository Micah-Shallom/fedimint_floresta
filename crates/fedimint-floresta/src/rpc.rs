// SPDX-License-Identifier: MIT OR Apache-2.0

//! Minimal JSON-RPC 2.0 transport for florestad.
//!
//! Floresta's RPC surface is a Bitcoin Core compatible subset, so the adapter
//! needs nothing beyond "send one request, get one typed result". Response
//! parsing is a pure function, separate from the HTTP call, so it can be unit
//! tested against recorded florestad responses without a network.

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::FlorestaClient;
use crate::error::RpcError;

impl FlorestaClient {
    /// Sends a single JSON-RPC request to florestad and returns its `result`.
    pub(crate) async fn call(&self, method: &str, params: Vec<Value>) -> Result<Value> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": method,
            "params": params,
        });

        let mut http_request = self.http.post(self.url.as_str()).json(&request);
        if let Some((user, password)) = &self.auth {
            http_request = http_request.basic_auth(user, Some(password));
        }

        let body = http_request
            .send()
            .await
            .with_context(|| format!("florestad request failed: {method}"))?
            .text()
            .await
            .with_context(|| format!("florestad response unreadable: {method}"))?;

        parse_response(&body).with_context(|| format!("florestad call failed: {method}"))
    }
}

#[derive(Debug, Deserialize)]
struct RpcResponse {
    result: Option<Value>,
    error: Option<RpcError>,
}

/// Splits a raw JSON-RPC response body into its result, or the error the node returned.
fn parse_response(body: &str) -> Result<Value> {
    let response: RpcResponse =
        serde_json::from_str(body).context("invalid JSON-RPC response from florestad")?;

    if let Some(error) = response.error {
        return Err(error.into());
    }

    match response.result {
        Some(result) => Ok(result),
        // A `null` result deserializes to `None` too, so distinguish it from a body
        // that has neither field, which is malformed.
        None if body.contains("\"result\"") => Ok(Value::Null),
        None => bail!("JSON-RPC response from florestad has neither result nor error"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scalar_result() {
        let body = r#"{"jsonrpc":"2.0","result":546,"id":0}"#;
        assert_eq!(parse_response(body).unwrap(), json!(546));
    }

    #[test]
    fn parses_null_result() {
        let body = r#"{"jsonrpc":"2.0","result":null,"id":0}"#;
        assert_eq!(parse_response(body).unwrap(), Value::Null);
    }

    #[test]
    fn surfaces_block_not_found() {
        // florestad returns -32098 for a block it does not know, e.g. a height above the tip.
        let body =
            r#"{"jsonrpc":"2.0","error":{"code":-32098,"message":"Block not found"},"id":0}"#;
        let error = parse_response(body).unwrap_err();
        let rpc_error = error.downcast_ref::<RpcError>().expect("typed RPC error");
        assert_eq!(rpc_error.code, crate::error::CODE_BLOCK_NOT_FOUND);
        assert_eq!(rpc_error.message, "Block not found");
    }

    #[test]
    fn surfaces_node_error_with_data() {
        // florestad returns -32091 for node-level failures, e.g. no peer to serve a block.
        let body = r#"{"jsonrpc":"2.0","error":{"code":-32091,"message":"Node error","data":"channel closed"},"id":0}"#;
        let error = parse_response(body).unwrap_err();
        let rpc_error = error.downcast_ref::<RpcError>().expect("typed RPC error");
        assert_eq!(rpc_error.code, crate::error::CODE_NODE_ERROR);
        assert_eq!(
            rpc_error.to_string(),
            r#"florestad RPC error -32091: Node error ("channel closed")"#
        );
    }

    #[test]
    fn rejects_malformed_body() {
        assert!(parse_response("not json").is_err());
        assert!(parse_response(r#"{"jsonrpc":"2.0","id":0}"#).is_err());
    }
}
