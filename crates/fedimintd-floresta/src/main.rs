// SPDX-License-Identifier: MIT OR Apache-2.0

//! `fedimintd-floresta`: stock fedimintd with `florestad` as the Bitcoin backend.
//!
//! Mirrors `fedimintd::run` at the pinned fedimint rev; the only intended
//! difference is the backend selection, which becomes a [`FlorestaClient`].

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use bitcoin::Network;
use clap::{CommandFactory, FromArgMatches, Parser};
use fedimint_core::db::Database;
use fedimint_core::envs::{FM_IROH_DNS_ENV, FM_IROH_RELAY_ENV};
use fedimint_core::module::registry::ModuleRegistry;
use fedimint_core::rustls::install_crypto_provider;
use fedimint_core::task::TaskGroup;
use fedimint_core::timing;
use fedimint_core::util::{FmtCompactAnyhow as _, SafeUrl};
use fedimint_floresta::FlorestaClient;
use fedimint_logging::{LOG_CORE, LOG_SERVER, TracingSetup};
use fedimint_rocksdb::RocksDb;
use fedimint_server::config::ConfigGenSettings;
use fedimint_server::config::io::DB_FILE;
use fedimint_server::net::api::ApiSecrets;
use fedimint_server_bitcoin_rpc::tracked::ServerBitcoinRpcTracked;
use fedimint_server_core::ServerModuleInitRegistryExt;
use fedimint_server_core::bitcoin_rpc::IServerBitcoinRpc;
use fedimintd_envs::{
    FM_API_URL_ENV, FM_BIND_API_ENV, FM_BIND_METRICS_ENV, FM_BIND_P2P_ENV,
    FM_BIND_TOKIO_CONSOLE_ENV, FM_BIND_UI_ENV, FM_BITCOIN_NETWORK_ENV, FM_DATA_DIR_ENV,
    FM_DB_CHECKPOINT_RETENTION_ENV, FM_ENABLE_IROH_ENV, FM_FORCE_API_SECRETS_ENV,
    FM_IROH_API_MAX_CONNECTIONS_ENV, FM_IROH_API_MAX_REQUESTS_PER_CONNECTION_ENV, FM_P2P_URL_ENV,
};
use futures::FutureExt as _;
use tracing::{debug, error, info};

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// Stand-in for fedimint's build-script hash; DKG only needs it identical
/// across guardians.
const CODE_VERSION_HASH: &str = "0000000000000000000000000000000000000000";

const FM_FLORESTA_URL_ENV: &str = "FM_FLORESTA_URL";

/// Pinned fedimintd's `ServerOpts` without the bitcoind/esplora backend
/// options, plus the florestad endpoint.
#[derive(Parser)]
#[command(version)]
struct ServerOpts {
    /// Path to folder containing federation config files
    #[arg(long = "data-dir", env = FM_DATA_DIR_ENV)]
    data_dir: PathBuf,

    /// The bitcoin network of the federation
    #[arg(long, env = FM_BITCOIN_NETWORK_ENV, default_value = "regtest")]
    bitcoin_network: Network,

    /// florestad JSON-RPC URL, e.g. <http://127.0.0.1:18442>
    #[arg(long, env = FM_FLORESTA_URL_ENV)]
    floresta_url: SafeUrl,

    /// Address we bind to for p2p consensus communication
    #[arg(long, env = FM_BIND_P2P_ENV, default_value = "0.0.0.0:8173")]
    bind_p2p: SocketAddr,

    /// Address we bind to for the API
    #[arg(long, env = FM_BIND_API_ENV, default_value = "0.0.0.0:8174")]
    bind_api: SocketAddr,

    /// Address we bind to for exposing the Web UI
    #[arg(long, env = FM_BIND_UI_ENV, default_value = "127.0.0.1:8175")]
    bind_ui: SocketAddr,

    /// Our external address for communicating with our peers
    #[arg(long, env = FM_P2P_URL_ENV)]
    p2p_url: Option<SafeUrl>,

    /// Our API address for clients to connect to us
    #[arg(long, env = FM_API_URL_ENV)]
    api_url: Option<SafeUrl>,

    #[arg(long, env = FM_ENABLE_IROH_ENV)]
    enable_iroh: bool,

    /// Optional URL of the Iroh DNS server
    #[arg(long, env = FM_IROH_DNS_ENV, requires = "enable_iroh")]
    iroh_dns: Option<SafeUrl>,

    /// Optional URLs of the Iroh relays to use for registering
    #[arg(long, env = FM_IROH_RELAY_ENV, requires = "enable_iroh", value_delimiter = ',')]
    iroh_relays: Vec<SafeUrl>,

    /// Number of checkpoints from the current session to retain on disk
    #[arg(long, env = FM_DB_CHECKPOINT_RETENTION_ENV, default_value = "1")]
    db_checkpoint_retention: u64,

    /// Enable tokio console logging
    #[arg(long, env = FM_BIND_TOKIO_CONSOLE_ENV)]
    bind_tokio_console: Option<SocketAddr>,

    /// Enable jaeger for tokio console logging
    #[arg(long, default_value = "false")]
    with_jaeger: bool,

    /// Enable prometheus metrics
    #[arg(long, env = FM_BIND_METRICS_ENV, default_value = "127.0.0.1:8176")]
    bind_metrics: Option<SocketAddr>,

    /// Comma separated list of API secrets
    #[arg(long, env = FM_FORCE_API_SECRETS_ENV, default_value = "")]
    force_api_secrets: ApiSecrets,

    /// Maximum number of concurrent Iroh API connections
    #[arg(long = "iroh-api-max-connections", env = FM_IROH_API_MAX_CONNECTIONS_ENV, default_value = "1000")]
    iroh_api_max_connections: usize,

    /// Maximum number of parallel requests per Iroh API connection
    #[arg(long = "iroh-api-max-requests-per-connection", env = FM_IROH_API_MAX_REQUESTS_PER_CONNECTION_ENV, default_value = "50")]
    iroh_api_max_requests_per_connection: usize,
}

// Mirrors fedimintd::run at the pin, minus its build-script version checks,
// private startup metric, and jemalloc feature warning.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let module_init_registry = fedimintd::default_modules();

    // Parse options, appending each module's env vars to --help.
    let server_opts = {
        let mut module_env_help = String::from("\nModule environment variables:\n");
        for (_kind, module_init) in module_init_registry.iter() {
            for doc in module_init.get_documented_env_vars() {
                let _ = writeln!(module_env_help, "  {:40}  {}", doc.name, doc.description);
            }
        }
        let matches = ServerOpts::command()
            .after_long_help(module_env_help)
            .get_matches();
        ServerOpts::from_arg_matches(&matches)
            .expect("clap arg matches must be valid after parsing")
    };

    // Logging first, so everything after can trace.
    let mut tracing_builder = TracingSetup::default();
    tracing_builder
        .tokio_console_bind(server_opts.bind_tokio_console)
        .with_jaeger(server_opts.with_jaeger);
    tracing_builder.init().unwrap();

    let version = env!("CARGO_PKG_VERSION");
    info!("Starting fedimintd-floresta (version: {version})");

    // Reports total process runtime when dropped at exit.
    let timing_total_runtime = timing::TimeReporter::new("total-runtime").info();

    // Owns every task we spawn; shutdown flows through it.
    let root_task_group = TaskGroup::new();

    // Prometheus endpoint, on by default at 127.0.0.1:8176.
    if let Some(bind_metrics) = server_opts.bind_metrics.as_ref() {
        info!(
            target: LOG_SERVER,
            url = %format!("http://{}/metrics", bind_metrics),
            "Initializing metrics server",
        );
        fedimint_metrics::spawn_api_server(*bind_metrics, root_task_group.clone()).await?;
    }

    // Everything the setup/DKG flow needs to configure this guardian.
    let settings = ConfigGenSettings {
        p2p_bind: server_opts.bind_p2p,
        api_bind: server_opts.bind_api,
        ui_bind: server_opts.bind_ui,
        p2p_url: server_opts.p2p_url.clone(),
        api_url: server_opts.api_url.clone(),
        enable_iroh: server_opts.enable_iroh,
        iroh_dns: server_opts.iroh_dns.clone(),
        iroh_relays: server_opts.iroh_relays.clone(),
        network: server_opts.bitcoin_network,
        available_modules: module_init_registry.kinds(),
        default_modules: module_init_registry.default_modules(),
    };

    // RocksDB at the stock location inside the data dir.
    let db = Database::new(
        RocksDb::build(server_opts.data_dir.join(DB_FILE))
            .open()
            .await
            .unwrap(),
        ModuleRegistry::default(),
    );

    // The backend swap: FlorestaClient where stock fedimintd selects
    // bitcoind/esplora, behind the same metrics wrapper.
    let dyn_server_bitcoin_rpc = FlorestaClient::new(&server_opts.floresta_url, None)
        .expect("floresta url must be usable")
        .into_dyn();
    let dyn_server_bitcoin_rpc =
        ServerBitcoinRpcTracked::new(dyn_server_bitcoin_rpc, "server").into_dyn();

    // Shutdown signals route through the task group.
    root_task_group.install_kill_handler();

    // One-time rustls provider setup required by fedimint's TLS stack.
    install_crypto_provider().await;

    // The server proper, as the cancellable main task.
    let task_group = root_task_group.clone();
    root_task_group.spawn_cancellable("main", async move {
        fedimint_server::run(
            server_opts.data_dir,
            server_opts.force_api_secrets,
            settings,
            db,
            version.to_string(),
            CODE_VERSION_HASH.to_string(),
            module_init_registry,
            task_group,
            dyn_server_bitcoin_rpc,
            Box::new(fedimint_server_ui::setup::router),
            Box::new(fedimint_server_ui::dashboard::router),
            server_opts.db_checkpoint_retention,
            fedimint_server::ConnectionLimits::new(
                server_opts.iroh_api_max_connections,
                server_opts.iroh_api_max_requests_per_connection,
            ),
        )
        .await
        .unwrap_or_else(|err| panic!("Main task returned error: {}", err.fmt_compact_anyhow()));
    });

    // Park until shutdown is signaled, then drain tasks with a timeout.
    let shutdown_future = root_task_group
        .make_handle()
        .make_shutdown_rx()
        .then(|()| async {
            info!(target: LOG_CORE, "Shutdown called");
        });

    shutdown_future.await;

    debug!(target: LOG_CORE, "Terminating main task");

    if let Err(err) = root_task_group.join_all(Some(SHUTDOWN_TIMEOUT)).await {
        error!(target: LOG_CORE, err = %err.fmt_compact_anyhow(), "Error while shutting down task group");
    }

    debug!(target: LOG_CORE, "Shutdown complete");

    fedimint_logging::shutdown();

    drop(timing_total_runtime);

    std::process::exit(-1);
}
