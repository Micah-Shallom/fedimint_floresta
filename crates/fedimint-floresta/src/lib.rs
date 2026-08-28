//! Fedimint server Bitcoin backend backed by a `florestad` JSON-RPC endpoint.
//!
//! [`FlorestaClient`] implements [`IServerBitcoinRpc`], the trait a Fedimint guardian
//! uses to observe the Bitcoin chain and broadcast transactions. Floresta's JSON-RPC
//! is a Bitcoin Core compatible subset, so every method is a thin passthrough; the
//! only intentional deviation is `get_feerate`, which Floresta cannot estimate.
//!
//! Scaffold checkpoint: trait signatures are pinned against fedimint `483a830`;
//! method bodies are filled in in the next step.

use anyhow::Result;
use async_trait::async_trait;
use bitcoin::{Block, BlockHash, Transaction};
use fedimint_core::envs::BitcoinRpcConfig;
use fedimint_core::util::SafeUrl;
use fedimint_core::{ChainId, Feerate};
use fedimint_server_core::bitcoin_rpc::IServerBitcoinRpc;

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
        todo!("getblockcount passthrough")
    }

    async fn get_block_hash(&self, _height: u64) -> Result<BlockHash> {
        todo!("getblockhash passthrough")
    }

    async fn get_block(&self, _block_hash: &BlockHash) -> Result<Block> {
        todo!("getblock verbosity 0 passthrough")
    }

    async fn get_feerate(&self) -> Result<Option<Feerate>> {
        // Floresta has no fee estimator. `None` is correct on regtest (Fedimint hardcodes
        // the rate there); other networks need an external source, see notes/spike-notes.md.
        Ok(None)
    }

    async fn submit_transaction(&self, _transaction: Transaction) -> Result<()> {
        todo!("sendrawtransaction passthrough")
    }

    async fn get_sync_progress(&self) -> Result<Option<f64>> {
        todo!("getblockchaininfo.verificationprogress passthrough")
    }

    async fn get_chain_id(&self) -> Result<ChainId> {
        todo!("getblockhash 1")
    }
}
