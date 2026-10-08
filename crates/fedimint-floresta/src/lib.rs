// SPDX-License-Identifier: MIT OR Apache-2.0

//! Fedimint server Bitcoin backend backed by a `florestad` JSON-RPC endpoint.
//!
//! [`FlorestaClient`] implements [`IServerBitcoinRpc`], the trait a Fedimint guardian
//! uses to observe the Bitcoin chain and broadcast transactions, over Floresta's
//! Bitcoin Core compatible JSON-RPC. Deliberate deviations from a plain passthrough:
//! the block count follows the validated height (`getblockchaininfo.blocks`), not
//! the header chain, and `get_feerate` returns `None` since Floresta has no fee
//! estimator (fedimint substitutes a fixed rate on regtest).
//!
//! Pinned against fedimint `483a830`.

use std::collections::HashSet;
use std::sync::atomic::AtomicU64;

mod error;
mod rpc;

pub use error::{CODE_BLOCK_NOT_FOUND, CODE_NODE_ERROR, RpcError};

use anyhow::{Context as _, Result, ensure};
use async_trait::async_trait;
use bitcoin::consensus::encode::{deserialize_hex, serialize_hex};
use bitcoin::{Block, BlockHash, Transaction};
use fedimint_core::envs::BitcoinRpcConfig;
use fedimint_core::util::SafeUrl;
use fedimint_core::{ChainId, Feerate};
use fedimint_server_core::bitcoin_rpc::IServerBitcoinRpc;
use serde_json::json;

/// Backend kind reported through [`BitcoinRpcConfig`].
pub const FLORESTA_RPC_KIND: &str = "floresta";

/// HTTP Basic auth credentials for the florestad RPC endpoint.
pub struct RpcAuth {
    pub(crate) user: String,
    pub(crate) password: String,
}

impl RpcAuth {
    pub fn new(user: String, password: String) -> Self {
        Self { user, password }
    }
}

impl std::fmt::Debug for RpcAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RpcAuth")
            .field("user", &self.user)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// A Fedimint Bitcoin backend that talks to a running `florestad` over JSON-RPC.
#[derive(Debug)]
pub struct FlorestaClient {
    url: SafeUrl,
    /// Optional HTTP Basic auth. Ignored by florestad builds without RPC auth support.
    auth: Option<RpcAuth>,
    http: reqwest::Client,
    /// Monotonic JSON-RPC request id, echoed back by the server per request.
    next_id: AtomicU64,
}

/// The fields of `getblockchaininfo` the adapter relies on.
#[derive(Debug, serde::Deserialize)]
struct BlockchainInfo {
    /// Fully validated chain height. Distinct from `headers`, which tracks the
    /// most-work header chain and runs ahead of validation during sync.
    blocks: u64,
    verificationprogress: f64,
}

impl FlorestaClient {
    async fn blockchain_info(&self) -> Result<BlockchainInfo> {
        let info = self.call("getblockchaininfo", vec![]).await?;
        serde_json::from_value(info).context("unexpected getblockchaininfo shape")
    }

    /// Creates a client for the florestad JSON-RPC endpoint at `url`.
    ///
    /// Credentials embedded in `url` are stripped from the stored copy, so the
    /// URL fedimint exposes on its dashboard never carries secrets.
    pub fn new(url: &SafeUrl, auth: Option<RpcAuth>) -> Result<Self> {
        Ok(Self {
            url: url
                .without_auth()
                .ok()
                .context("could not strip credentials from florestad URL")?,
            auth,
            http: reqwest::Client::builder()
                .timeout(rpc::REQUEST_TIMEOUT)
                .build()?,
            next_id: AtomicU64::new(0),
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
        // Validated height, not `getblockcount`, which reports the header chain
        // and runs ahead of validation during sync. Genesis counts as 1.
        Ok(self.blockchain_info().await?.blocks + 1)
    }

    async fn get_block_hash(&self, height: u64) -> Result<BlockHash> {
        let result = self.call("getblockhash", vec![json!(height)]).await?;
        serde_json::from_value(result).context("getblockhash did not return a block hash")
    }

    async fn get_block(&self, block_hash: &BlockHash) -> Result<Block> {
        // Verbosity 0 returns the raw block as hex; florestad fetches it from
        // a peer on demand and errors cleanly when it has none.
        let result = self
            .call("getblock", vec![json!(block_hash), json!(0)])
            .await?;
        let block_hex = result
            .as_str()
            .with_context(|| format!("getblock did not return a string for {block_hash}"))?;
        let block: Block = deserialize_hex(block_hex)
            .with_context(|| format!("getblock returned undecodable hex for {block_hash}"))?;

        // florestad serves user-requested blocks from an arbitrary peer without
        // validating contents, so bind them to the trusted header here.
        ensure!(
            block.block_hash() == *block_hash,
            "getblock returned a different block than {block_hash}"
        );
        ensure!(
            block.check_merkle_root(),
            "getblock returned a block with a bad merkle root: {block_hash}"
        );
        // A fully witness-stripped block passes by design; deposit scanning
        // reads outputs and txids only, which are witness-independent.
        ensure!(
            block.check_witness_commitment(),
            "getblock returned a block with a bad witness commitment: {block_hash}"
        );
        // Duplicated trailing transactions preserve the merkle root (CVE-2012-2459).
        let mut seen_txids = HashSet::with_capacity(block.txdata.len());
        ensure!(
            block
                .txdata
                .iter()
                .all(|tx| seen_txids.insert(tx.compute_txid())),
            "getblock returned a block with duplicate transactions: {block_hash}"
        );

        Ok(block)
    }

    /// Always `Ok(None)`: Floresta has no fee estimator.
    ///
    /// On regtest fedimint substitutes a fixed rate and never consults this.
    /// On any other network `None` fails the monitor's status poll, so the
    /// guardian never reports connected and refuses all backend calls: a real
    /// feerate source is required before non-regtest use. Quirk: fedimint
    /// derives the network from the block-1 hash and treats unknown chains,
    /// including testnet4, as regtest, so those work by accident.
    async fn get_feerate(&self) -> Result<Option<Feerate>> {
        Ok(None)
    }

    async fn submit_transaction(&self, transaction: Transaction) -> Result<()> {
        // The returned txid is informational; fedimint retries broadcasts, and
        // florestad accepts rebroadcasts of known transactions without error.
        // Its mempool does not accept replacements, which only affects
        // fedimint's deprecated opt-in RBF withdrawals.
        let txid = transaction.compute_txid();
        let tx_hex = serialize_hex(&transaction);
        self.call("sendrawtransaction", vec![json!(tx_hex)])
            .await
            .map(|_| ())
            .with_context(|| format!("failed to broadcast transaction {txid}"))
    }

    async fn get_sync_progress(&self) -> Result<Option<f64>> {
        Ok(Some(self.blockchain_info().await?.verificationprogress))
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
    /// returns a client pointed at it plus a receiver yielding the request body
    /// the client sent, so tests can assert both sides of the exchange.
    async fn client_with_stub(
        response_body: &'static str,
    ) -> (FlorestaClient, tokio::sync::oneshot::Receiver<Vec<u8>>) {
        let (request_tx, request_rx) = tokio::sync::oneshot::channel();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url: SafeUrl = format!("http://{}/", listener.local_addr().unwrap())
            .parse()
            .unwrap();

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            // Drain the full request (headers, then content-length bytes) so the
            // response is never written mid-request, even if reads fragment.
            let mut request = Vec::new();
            let mut buf = [0u8; 1024];
            let body_start = loop {
                let n = stream.read(&mut buf).await.unwrap();
                assert!(n > 0, "request ended before headers were complete");
                request.extend_from_slice(&buf[..n]);
                if let Some(pos) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos + 4;
                }
            };
            let headers = String::from_utf8_lossy(&request[..body_start]).to_lowercase();
            let content_length: usize = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .expect("request must declare content-length")
                .trim()
                .parse()
                .unwrap();
            while request.len() < body_start + content_length {
                let n = stream.read(&mut buf).await.unwrap();
                assert!(n > 0, "request ended before the body was complete");
                request.extend_from_slice(&buf[..n]);
            }
            // Hand the JSON body back so the test can assert what was asked.
            let _ = request_tx.send(request[body_start..body_start + content_length].to_vec());
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response_body}",
                response_body.len(),
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });

        (FlorestaClient::new(&url, None).unwrap(), request_rx)
    }

    /// Parses the captured request body into JSON for assertions.
    async fn sent_request(
        request_rx: tokio::sync::oneshot::Receiver<Vec<u8>>,
    ) -> serde_json::Value {
        serde_json::from_slice(&request_rx.await.expect("stub captured a request"))
            .expect("request body is JSON")
    }

    const HASH_1: &str = "644a2cc8bf2a5efc69ade867028a75c56c5e33ab29919ef6b8f50a58ea8a2e25";

    /// A txid distinct from [`HASH_1`], used where the stub answers broadcasts.
    const TXID_1: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    /// A minimally realistic transaction: one input, one output. Floresta's
    /// mempool rejects transactions with empty inputs or outputs.
    fn test_transaction() -> Transaction {
        Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: bitcoin::absolute::LockTime::ZERO,
            input: vec![bitcoin::TxIn {
                previous_output: bitcoin::OutPoint::new(TXID_1.parse().unwrap(), 0),
                script_sig: bitcoin::ScriptBuf::new(),
                sequence: bitcoin::Sequence::MAX,
                witness: bitcoin::Witness::new(),
            }],
            output: vec![bitcoin::TxOut {
                value: bitcoin::Amount::from_sat(10_000),
                script_pubkey: bitcoin::ScriptBuf::new(),
            }],
        }
    }

    /// Block 150 of a regtest chain, exactly as served by `florestad getblock
    /// <hash> 0` (2026-10-08). Unlike the constructed fixtures, these bytes
    /// come from a real node and carry a segwit coinbase: a witness commitment
    /// output plus the 32-byte witness nonce.
    const RECORDED_REGTEST_BLOCK: &str = "000000305d60e22417d61d96652ed68c25a7d04ecf023689640b4b1de1fc946e0d83c578b57c00f98e7406e242fbd2d3fbe43bc0e2e06ebdf30ea72bfe4550e1eb8bf59315c9c66affff7f200000000001020000000001010000000000000000000000000000000000000000000000000000000000000000ffffffff0402960000ffffffff0200f9029500000000160014ea03203d082cf1a2e4d80ab0a5260d58e730cb410000000000000000266a24aa21a9ede2f61c3f71d1defd3fa999dfa36953755c690689799962b48bebd836974e8cf90120000000000000000000000000000000000000000000000000000000000000000000000000";

    #[tokio::test]
    async fn block_count_is_validated_height_plus_one() {
        // blocks lags headers while syncing; the count must follow blocks.
        let body = r#"{"jsonrpc":"2.0","result":{"chain":"regtest","blocks":540,"headers":546,"verificationprogress":0.98,"initialblockdownload":true},"id":0}"#;
        let (client, request_rx) = client_with_stub(body).await;
        assert_eq!(client.get_block_count().await.unwrap(), 541);
        let request = sent_request(request_rx).await;
        assert_eq!(request["method"], "getblockchaininfo");
        assert_eq!(request["params"], json!([]));
    }

    #[tokio::test]
    async fn block_hash_round_trips() {
        let body = format!(r#"{{"jsonrpc":"2.0","result":"{HASH_1}","id":0}}"#).leak();
        let (client, request_rx) = client_with_stub(body).await;
        assert_eq!(client.get_block_hash(1).await.unwrap().to_string(), HASH_1);
        let request = sent_request(request_rx).await;
        assert_eq!(request["method"], "getblockhash");
        assert_eq!(request["params"], json!([1]));
    }

    #[tokio::test]
    async fn sync_progress_reads_verificationprogress() {
        let body = r#"{"jsonrpc":"2.0","result":{"chain":"regtest","blocks":546,"verificationprogress":1.0,"initialblockdownload":false},"id":0}"#;
        let (client, _request) = client_with_stub(body).await;
        assert_eq!(client.get_sync_progress().await.unwrap(), Some(1.0));
    }

    #[tokio::test]
    async fn block_not_found_surfaces_as_typed_error() {
        let body =
            r#"{"jsonrpc":"2.0","error":{"code":-32098,"message":"Block not found"},"id":0}"#;
        let (client, _request) = client_with_stub(body).await;
        let error = client.get_block_hash(1_000_000).await.unwrap_err();
        let rpc_error = error
            .downcast_ref::<RpcError>()
            .expect("expected typed RPC error");
        assert_eq!(rpc_error.code, error::CODE_BLOCK_NOT_FOUND);
    }

    #[tokio::test]
    async fn block_fetch_node_error_surfaces_as_typed_error() {
        // florestad with no peer to serve the block answers -32091.
        let body = r#"{"jsonrpc":"2.0","error":{"code":-32091,"message":"Node error","data":"channel closed"},"id":0}"#;
        let (client, _request) = client_with_stub(body).await;
        let hash: BlockHash = HASH_1.parse().unwrap();
        let error = client.get_block(&hash).await.unwrap_err();
        let rpc_error = error.downcast_ref::<RpcError>().expect("typed RPC error");
        assert_eq!(rpc_error.code, CODE_NODE_ERROR);
    }

    #[tokio::test]
    async fn block_fetch_non_string_result_is_rejected() {
        let body = r#"{"jsonrpc":"2.0","result":42,"id":0}"#;
        let (client, _request) = client_with_stub(body).await;
        let hash: BlockHash = HASH_1.parse().unwrap();
        let error = client.get_block(&hash).await.unwrap_err();
        assert!(error.to_string().contains("did not return a string"));
    }

    #[tokio::test]
    async fn block_fetch_undecodable_hex_is_rejected() {
        let body = r#"{"jsonrpc":"2.0","result":"deadbeef","id":0}"#;
        let (client, _request) = client_with_stub(body).await;
        let hash: BlockHash = HASH_1.parse().unwrap();
        let error = client.get_block(&hash).await.unwrap_err();
        assert!(error.to_string().contains("undecodable hex"));
    }

    #[tokio::test]
    async fn rejected_broadcast_surfaces_as_typed_error() {
        let body = r#"{"jsonrpc":"2.0","error":{"code":-26,"message":"tx-rejected"},"id":0}"#;
        let (client, _request) = client_with_stub(body).await;
        let error = client
            .submit_transaction(test_transaction())
            .await
            .unwrap_err();
        let rpc_error = error.downcast_ref::<RpcError>().expect("typed RPC error");
        assert_eq!(rpc_error.code, -26);
    }

    #[test]
    fn auth_debug_redacts_password() {
        let auth = RpcAuth::new("guardian".into(), "hunter2".into());
        let rendered = format!("{auth:?}");
        assert!(rendered.contains("guardian"));
        assert!(!rendered.contains("hunter2"));
        assert!(rendered.contains("<redacted>"));
    }

    /// A minimal block that passes all content checks: one coinbase transaction,
    /// header merkle root set to its txid (the root of a single-leaf tree).
    fn valid_test_block() -> Block {
        use bitcoin::hashes::Hash as _;
        let coinbase = Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: bitcoin::absolute::LockTime::ZERO,
            input: vec![bitcoin::TxIn {
                previous_output: bitcoin::OutPoint::null(),
                script_sig: bitcoin::ScriptBuf::new(),
                sequence: bitcoin::Sequence::MAX,
                witness: bitcoin::Witness::new(),
            }],
            output: vec![],
        };
        let merkle_root = coinbase.compute_txid().to_raw_hash().into();
        Block {
            header: bitcoin::block::Header {
                version: bitcoin::block::Version::TWO,
                prev_blockhash: BlockHash::all_zeros(),
                merkle_root,
                time: 1_700_000_000,
                bits: bitcoin::CompactTarget::from_consensus(0x207f_ffff),
                nonce: 0,
            },
            txdata: vec![coinbase],
        }
    }

    async fn client_serving_block(
        block: &Block,
    ) -> (FlorestaClient, tokio::sync::oneshot::Receiver<Vec<u8>>) {
        let block_hex = serialize_hex(block);
        let body = format!(r#"{{"jsonrpc":"2.0","result":"{block_hex}","id":0}}"#).leak();
        client_with_stub(body).await
    }

    #[tokio::test]
    async fn block_round_trips_through_raw_hex() {
        let block = valid_test_block();
        let (client, request_rx) = client_serving_block(&block).await;
        assert_eq!(client.get_block(&block.block_hash()).await.unwrap(), block);
        let request = sent_request(request_rx).await;
        assert_eq!(request["method"], "getblock");
        assert_eq!(
            request["params"],
            json!([block.block_hash().to_string(), 0])
        );
    }

    #[tokio::test]
    async fn recorded_regtest_block_passes_all_checks() {
        // Non-circular fixture: decoded bytes come from a real node, so this
        // exercises decoding and the witness-commitment check on genuine data.
        let block: Block = deserialize_hex(RECORDED_REGTEST_BLOCK).unwrap();
        assert!(
            !block.txdata[0].input[0].witness.is_empty(),
            "fixture must carry a witness nonce"
        );
        let body =
            format!(r#"{{"jsonrpc":"2.0","result":"{RECORDED_REGTEST_BLOCK}","id":0}}"#).leak();
        let (client, _request) = client_with_stub(body).await;
        assert_eq!(client.get_block(&block.block_hash()).await.unwrap(), block);
    }

    #[tokio::test]
    async fn block_for_wrong_hash_is_rejected() {
        let (client, _request) = client_serving_block(&valid_test_block()).await;
        let other: BlockHash = HASH_1.parse().unwrap();
        let error = client.get_block(&other).await.unwrap_err();
        assert!(error.to_string().contains("different block"));
    }

    #[tokio::test]
    async fn tampered_transaction_list_is_rejected() {
        let mut block = valid_test_block();
        let mut extra = block.txdata[0].clone();
        extra.lock_time = bitcoin::absolute::LockTime::from_consensus(1);
        block.txdata.push(extra);
        // Header untouched: the hash matches the request, the merkle root does not.
        let (client, _request) = client_serving_block(&block).await;
        let error = client.get_block(&block.block_hash()).await.unwrap_err();
        assert!(error.to_string().contains("bad merkle root"));
    }

    #[tokio::test]
    async fn duplicated_transactions_are_rejected() {
        let mut block = valid_test_block();
        let dup = block.txdata[0].clone();
        block.txdata.push(dup);
        // Recompute the root so the duplicate pair passes the merkle check
        // (CVE-2012-2459) and only txid uniqueness can catch it.
        block.header.merkle_root = block.compute_merkle_root().unwrap();
        let (client, _request) = client_serving_block(&block).await;
        let error = client.get_block(&block.block_hash()).await.unwrap_err();
        assert!(error.to_string().contains("duplicate transactions"));
    }

    #[tokio::test]
    async fn transaction_broadcast_returns_ok_on_txid() {
        let body = format!(r#"{{"jsonrpc":"2.0","result":"{TXID_1}","id":0}}"#).leak();
        let (client, request_rx) = client_with_stub(body).await;
        let tx = test_transaction();
        let tx_hex = serialize_hex(&tx);
        assert!(client.submit_transaction(tx).await.is_ok());
        let request = sent_request(request_rx).await;
        assert_eq!(request["method"], "sendrawtransaction");
        assert_eq!(request["params"], json!([tx_hex]));
    }

    #[test]
    fn stored_url_drops_embedded_credentials() {
        let url: SafeUrl = "http://user:secret@127.0.0.1:18442/".parse().unwrap();
        let client = FlorestaClient::new(&url, None).unwrap();
        let stored = client.get_url().to_string();
        assert!(!stored.contains("user"));
        assert!(!stored.contains("secret"));
    }
}
