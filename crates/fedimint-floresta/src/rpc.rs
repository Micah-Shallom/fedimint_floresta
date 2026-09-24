// SPDX-License-Identifier: MIT OR Apache-2.0

//! Minimal JSON-RPC 2.0 transport for florestad.
//!
//! Floresta's RPC surface is a Bitcoin Core compatible subset, so the adapter
//! needs nothing beyond "send one request, get one typed result". Response
//! parsing is a pure function, separate from the HTTP call, so it can be unit
//! tested against recorded florestad responses without a network.

use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::{Context as _, Result, bail, ensure};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use crate::FlorestaClient;
use crate::error::RpcError;

/// Upper bound on a single request; florestad answers locally in milliseconds,
/// and an on-demand block fetch from a healthy peer stays well under this.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

impl FlorestaClient {
    /// Sends a single JSON-RPC request to florestad and returns its `result`.
    pub(crate) async fn call(&self, method: &str, params: Vec<Value>) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let mut http_request = self.http.post(self.url.as_str()).json(&request);
        if let Some(auth) = &self.auth {
            http_request = http_request.basic_auth(&auth.user, Some(&auth.password));
        }

        let response = http_request
            .send()
            .await
            .with_context(|| format!("florestad request failed: {method}"))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .with_context(|| format!("florestad response unreadable: {method}"))?;

        let result = parse_response(&body, id);
        // A typed RPC error is self-describing; anything else (proxy error page,
        // truncated body) gets the HTTP status and a body snippet for diagnosis.
        if let Err(error) = &result
            && error.downcast_ref::<RpcError>().is_none()
        {
            let snippet: String = body.chars().take(200).collect();
            return result.with_context(|| {
                format!("florestad call failed: {method} (HTTP {status}, body: {snippet:?})")
            });
        }
        result.with_context(|| format!("florestad call failed: {method}"))
    }
}

#[derive(Debug, Deserialize)]
struct RpcResponse {
    // A present-but-null `result` must become `Some(Value::Null)`, not `None`,
    // so a missing field stays distinguishable from a null one.
    #[serde(default, deserialize_with = "some_even_if_null")]
    result: Option<Value>,
    error: Option<RpcError>,
    id: Option<Value>,
}

fn some_even_if_null<'de, D>(deserializer: D) -> Result<Option<Value>, D::Error>
where
    D: Deserializer<'de>,
{
    Value::deserialize(deserializer).map(Some)
}

/// Splits a raw JSON-RPC response body into its result, or the error the node
/// returned, verifying the response echoes the request id.
fn parse_response(body: &str, expected_id: u64) -> Result<Value> {
    let response: RpcResponse =
        serde_json::from_str(body).context("invalid JSON-RPC response from florestad")?;

    if let Some(error) = response.error {
        return Err(error.into());
    }

    ensure!(
        response.id == Some(json!(expected_id)),
        "JSON-RPC response id {:?} does not echo request id {expected_id}",
        response.id,
    );

    match response.result {
        Some(result) => Ok(result),
        None => bail!("JSON-RPC response from florestad has neither result nor error"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scalar_result() {
        let body = r#"{"jsonrpc":"2.0","result":546,"id":0}"#;
        assert_eq!(parse_response(body, 0).unwrap(), json!(546));
    }

    #[test]
    fn parses_null_result() {
        let body = r#"{"jsonrpc":"2.0","result":null,"id":0}"#;
        assert_eq!(parse_response(body, 0).unwrap(), Value::Null);
    }

    #[test]
    fn rejects_mismatched_response_id() {
        let body = r#"{"jsonrpc":"2.0","result":546,"id":7}"#;
        let error = parse_response(body, 0).unwrap_err();
        assert!(error.to_string().contains("does not echo request id 0"));
    }

    #[test]
    fn rejects_missing_result_field() {
        let body = r#"{"jsonrpc":"2.0","id":0}"#;
        assert!(parse_response(body, 0).is_err());
    }

    #[test]
    fn surfaces_block_not_found() {
        // florestad returns -32098 for a block it does not know, e.g. a height above the tip.
        let body =
            r#"{"jsonrpc":"2.0","error":{"code":-32098,"message":"Block not found"},"id":0}"#;
        let error = parse_response(body, 0).unwrap_err();
        let rpc_error = error.downcast_ref::<RpcError>().expect("typed RPC error");
        assert_eq!(rpc_error.code, crate::error::CODE_BLOCK_NOT_FOUND);
        assert_eq!(rpc_error.message, "Block not found");
    }

    #[test]
    fn surfaces_node_error_with_data() {
        // florestad returns -32091 for node-level failures, e.g. no peer to serve a block.
        let body = r#"{"jsonrpc":"2.0","error":{"code":-32091,"message":"Node error","data":"channel closed"},"id":0}"#;
        let error = parse_response(body, 0).unwrap_err();
        let rpc_error = error.downcast_ref::<RpcError>().expect("typed RPC error");
        assert_eq!(rpc_error.code, crate::error::CODE_NODE_ERROR);
        assert_eq!(
            rpc_error.to_string(),
            r#"florestad RPC error -32091: Node error ("channel closed")"#
        );
    }

    #[test]
    fn rejects_malformed_body() {
        assert!(parse_response("not json", 0).is_err());
    }
}
