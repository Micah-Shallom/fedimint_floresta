//! Fedimint server Bitcoin backend backed by a `florestad` JSON-RPC endpoint.
//!
//! [`FlorestaClient`] implements [`IServerBitcoinRpc`], the trait a Fedimint guardian
//! uses to observe the Bitcoin chain and broadcast transactions. Floresta's JSON-RPC
//! is a Bitcoin Core compatible subset, so every method is a thin passthrough; the
//! only intentional deviation is `get_feerate`, which Floresta cannot estimate.
//!
//! Pinned against fedimint `483a830`. Block fetch (`get_block`) and transaction
//! broadcast (`submit_transaction`) are not implemented yet.

pub mod error;
mod rpc;

pub use error::RpcError;

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use bitcoin::{Block, BlockHash, Transaction};
use fedimint_core::envs::BitcoinRpcConfig;
use fedimint_core::util::SafeUrl;
use fedimint_core::{ChainId, Feerate};
use fedimint_server_core::bitcoin_rpc::IServerBitcoinRpc;
use serde_json::json;

/// Backend kind reported through [`BitcoinRpcConfig`].
pub const FLORESTA_RPC_KIND: &str = "floresta";

/// A Fedimint Bitcoin backend that talks to a running `florestad` over JSON-RPC.
#[derive(Debug)]
pub struct FlorestaClient {
    url: SafeUrl,
    /// Optional HTTP Basic auth. Ignored by florestad builds without RPC auth support.
    auth: Option<(String, String)>,
    http: reqwest::Client,
}

impl FlorestaClient {
    /// Creates a client for the florestad JSON-RPC endpoint at `url`.
    pub fn new(url: &SafeUrl, auth: Option<(String, String)>) -> Result<Self> {
        Ok(Self {
            url: url.clone(),
            auth,
            http: reqwest::Client::builder().build()?,
        })
    }
}

#[async_trait]
impl IServerBitcoinRpc for FlorestaClient {
    fn get_bitcoin_rpc_config(&self) -> BitcoinRpcConfig {
        BitcoinRpcConfig {
            kind: FLORESTA_RPC_KIND.to_string(),
            url: self.url.clone(),
        }
    }

    fn get_url(&self) -> SafeUrl {
        self.url.clone()
    }

    async fn get_block_count(&self) -> Result<u64> {
        // `getblockcount` actually returns the tip height; the trait expects a
        // count where genesis is 1, mirroring fedimint's bitcoind backend.
        let height = self
            .call("getblockcount", vec![])
            .await?
            .as_u64()
            .context("getblockcount did not return an unsigned integer")?;
        Ok(height + 1)
    }

    async fn get_block_hash(&self, height: u64) -> Result<BlockHash> {
        let result = self.call("getblockhash", vec![json!(height)]).await?;
        serde_json::from_value(result).context("getblockhash did not return a block hash")
    }

    async fn get_block(&self, _block_hash: &BlockHash) -> Result<Block> {
        todo!("getblock verbosity 0 passthrough")
    }

    async fn get_feerate(&self) -> Result<Option<Feerate>> {
        // Floresta has no fee estimator. `None` is correct on regtest, where Fedimint
        // substitutes a fixed rate; other networks require an external feerate source.
        Ok(None)
    }

    async fn submit_transaction(&self, _transaction: Transaction) -> Result<()> {
        todo!("sendrawtransaction passthrough")
    }

    async fn get_sync_progress(&self) -> Result<Option<f64>> {
        let info = self.call("getblockchaininfo", vec![]).await?;
        let progress = info
            .get("verificationprogress")
            .and_then(serde_json::Value::as_f64)
            .context("getblockchaininfo did not report verificationprogress")?;
        Ok(Some(progress))
    }

    async fn get_chain_id(&self) -> Result<ChainId> {
        // The chain id is defined as the hash of block 1, which also lets fedimint
        // recognize known networks (mainnet, testnet, signet) and treat the rest as regtest.
        self.get_block_hash(1).await.map(ChainId::new)
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::*;

    /// Serves exactly one canned JSON-RPC response on an ephemeral local port and
    /// returns a client pointed at it, so tests exercise the full HTTP path.
    async fn client_with_stub(response_body: &'static str) -> FlorestaClient {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url: SafeUrl = format!("http://{}/", listener.local_addr().unwrap())
            .parse()
            .unwrap();

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            // The request fits one read on loopback; its content is irrelevant here.
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request).await.unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response_body}",
                response_body.len(),
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });

        FlorestaClient::new(&url, None).unwrap()
    }

    const HASH_1: &str = "644a2cc8bf2a5efc69ade867028a75c56c5e33ab29919ef6b8f50a58ea8a2e25";

    #[tokio::test]
    async fn block_count_is_height_plus_one() {
        let client = client_with_stub(r#"{"jsonrpc":"2.0","result":546,"id":0}"#).await;
        assert_eq!(client.get_block_count().await.unwrap(), 547);
    }

    #[tokio::test]
    async fn block_hash_round_trips() {
        let body: &str = r#"{"jsonrpc":"2.0","result":"644a2cc8bf2a5efc69ade867028a75c56c5e33ab29919ef6b8f50a58ea8a2e25","id":0}"#;
        let client = client_with_stub(body).await;
        assert_eq!(client.get_block_hash(1).await.unwrap().to_string(), HASH_1);
    }

    #[tokio::test]
    async fn sync_progress_reads_verificationprogress() {
        let body: &str = r#"{"jsonrpc":"2.0","result":{"chain":"regtest","blocks":546,"verificationprogress":1.0,"initialblockdownload":false},"id":0}"#;
        let client = client_with_stub(body).await;
        assert_eq!(client.get_sync_progress().await.unwrap(), Some(1.0));
    }

    #[tokio::test]
    async fn block_not_found_surfaces_as_typed_error() {
        let body: &str =
            r#"{"jsonrpc":"2.0","error":{"code":-32098,"message":"Block not found"},"id":0}"#;
        let client = client_with_stub(body).await;
        let error = client.get_block_hash(1_000_000).await.unwrap_err();
        let rpc_error = error
            .downcast_ref::<RpcError>()
            .expect("expected typed RPC error");
        assert_eq!(rpc_error.code, error::CODE_BLOCK_NOT_FOUND);
    }
}
