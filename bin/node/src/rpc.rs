//! A collection of node-specific RPC methods.
//! Substrate provides the `sc-rpc` crate, which defines the core RPC layer
//! used by Substrate nodes. This file extends those RPC definitions with
//! capabilities that are specific to this project's runtime configuration.

#![warn(missing_docs)]

use std::sync::Arc;

use finality_aleph::{Justification, JustificationTranslator, ValidatorAddressCache};
use futures::channel::mpsc;
use jsonrpsee::RpcModule;
use primitives::{AccountId, Balance, Block, Nonce};
use sc_client_api::backend::StorageProvider;
use sc_client_api::backend::Backend;
use sc_rpc::SubscriptionTaskExecutor;
pub use sc_rpc_api::DenyUnsafe;
use sc_transaction_pool::ChainApi;
use sc_transaction_pool_api::TransactionPool;
use sp_api::ProvideRuntimeApi;
use sp_block_builder::BlockBuilder;
use sp_blockchain::{Error as BlockChainError, HeaderBackend, HeaderMetadata};
use sp_consensus::SyncOracle;
use aleph_runtime::Hash;
use crate::service::FrontierPartialComponents;
use sp_core::H256;
use sp_api::CallApiAt;
use fp_rpc::ConvertTransactionRuntimeApi;
use fp_rpc::EthereumRuntimeRPCApi;
use sc_client_api::BlockchainEvents;
use sc_client_api::AuxStore;  
use sc_client_api::UsageProvider;
use fp_rpc::ConvertTransaction;
use fc_rpc::EthConfig;
use sp_api::__private::BlockT;
use sc_transaction_pool::Pool;
use sc_network::NetworkService;
use sc_network_sync::SyncingService;
use fc_rpc::OverrideHandle;
use fc_rpc::EthBlockDataCacheTask;
use fc_rpc_core::types::FilterPool;
use fc_rpc_core::types::FeeHistoryCache;
use fc_rpc_core::types::FeeHistoryCacheLimit;
use parity_scale_codec::alloc::collections::BTreeMap;
use sp_block_builder::BlockBuilder as BlockBuilderApi;
use sp_inherents::CreateInherentDataProviders;
use sc_service::{error::Error as ServiceError, Configuration, TaskManager};
use std::sync::Mutex;
pub struct DefaultEthConfig<C, BE>(std::marker::PhantomData<(C, BE)>);

impl<C, BE> fc_rpc::EthConfig<Block, C> for DefaultEthConfig<C, BE>
where
	C: StorageProvider<Block, BE> + Sync + Send + 'static,
	BE: Backend<Block> + 'static,
{
	type EstimateGasAdapter = ();
	type RuntimeStorageOverride =
		fc_rpc::frontier_backend_client::SystemAccountId20StorageOverride<Block, C, BE>;
}
/// Extra dependencies for Ethereum compatibility.
pub struct EthDeps<B: BlockT, C, P, A: ChainApi, CT, CIDP> {
	/// The client instance to use.
	pub client: Arc<C>,
	/// Transaction pool instance.
	pub pool: Arc<P>,
	/// Graph pool instance.
	pub graph: Arc<Pool<A>>,
	/// Ethereum transaction converter.
	pub converter: Option<CT>,
	/// The Node authority flag
	pub is_authority: bool,
	/// Whether to enable dev signer
	pub enable_dev_signer: bool,
	/// Network service
	pub network: Arc<NetworkService<B, B::Hash>>,
	/// Chain syncing service
	pub sync: Arc<SyncingService<B>>,
	/// Frontier Backend.
	pub frontier_backend: Arc<dyn fc_api::Backend<B>>,
	/// Ethereum data access overrides.
	pub overrides: Arc<OverrideHandle<B>>,
	/// Cache for Ethereum block data.
	pub block_data_cache: Arc<EthBlockDataCacheTask<B>>,
	/// EthFilterApi pool.
	pub filter_pool: Option<FilterPool>,
	/// Maximum number of logs in a query.
	pub max_past_logs: u32,
	/// Fee history cache.
	pub fee_history_cache: FeeHistoryCache,
	/// Maximum fee history cache size.
	pub fee_history_cache_limit: FeeHistoryCacheLimit,
	/// Maximum allowed gas limit will be ` block.gas_limit * execute_gas_limit_multiplier` when
	/// using eth_call/eth_estimateGas.
	pub execute_gas_limit_multiplier: u64,
	/// Mandated parent hashes for a given block hash.
	pub forced_parent_hashes: Option<BTreeMap<H256, H256>>,
	/// Something that can create the inherent data providers for pending state
	pub pending_create_inherent_data_providers: CIDP,
}

/// Full client dependencies.
pub struct FullDeps<C, P, A: ChainApi, CT, SO, CIDP> {
    /// The client instance to use.
    pub client: Arc<C>,
    /// Transaction pool instance.
    pub pool: Arc<P>,
    /// Whether to deny unsafe calls
    pub deny_unsafe: DenyUnsafe,
    pub import_justification_tx: mpsc::UnboundedSender<Justification>,
    pub justification_translator: JustificationTranslator,
	pub eth: EthDeps<Block, C, P, A, CT, CIDP>,

    pub sync_oracle: SO,
    pub validator_address_cache: Option<ValidatorAddressCache>,
}


/// Instantiate Ethereum-compatible RPC extensions.
pub fn create_eth<B, C, BE, P, A, CT, CIDP, EC>(
	mut io: RpcModule<()>,
	deps: EthDeps<B, C, P, A, CT, CIDP>,
	subscription_task_executor: SubscriptionTaskExecutor,
	pubsub_notification_sinks: Arc<
		fc_mapping_sync::EthereumBlockNotificationSinks<
			fc_mapping_sync::EthereumBlockNotification<B>,
		>,
	>,
) -> Result<RpcModule<()>, Box<dyn std::error::Error + Send + Sync>>
where
	B: BlockT<Hash = H256>,
	C: CallApiAt<B> + ProvideRuntimeApi<B>,
	C::Api:
		 BlockBuilderApi<B>
		+ ConvertTransactionRuntimeApi<B>
		+ EthereumRuntimeRPCApi<B>,
	C: HeaderBackend<B> + HeaderMetadata<B, Error = BlockChainError>,
	C: BlockchainEvents<B> + AuxStore + UsageProvider<B> + StorageProvider<B, BE> + 'static,
	BE: Backend<B> + 'static,
	P: TransactionPool<Block = B> + 'static,
	A: ChainApi<Block = B> + 'static,
	CT: ConvertTransaction<<B as BlockT>::Extrinsic> + Send + Sync + 'static,
	CIDP: CreateInherentDataProviders<B, ()> + Send + 'static,
	EC: EthConfig<B, C>,
{
	use fc_rpc::{
		pending::AuraConsensusDataProvider, Debug, DebugApiServer, Eth, EthApiServer, EthDevSigner,
		EthFilter, EthFilterApiServer, EthPubSub, EthPubSubApiServer, EthSigner, Net, NetApiServer,
		Web3, Web3ApiServer,
	};
	#[cfg(feature = "txpool")]
	use fc_rpc::{TxPool, TxPoolApiServer};

	let EthDeps {
		client,
		pool,
		graph,
		converter,
		is_authority,
		enable_dev_signer,
		network,
		sync,
		frontier_backend,
		overrides,
		block_data_cache,
		filter_pool,
		max_past_logs,
		fee_history_cache,
		fee_history_cache_limit,
		execute_gas_limit_multiplier,
		forced_parent_hashes,
		pending_create_inherent_data_providers,
	} = deps;

	let mut signers = Vec::new();
	if enable_dev_signer {
		signers.push(Box::new(EthDevSigner::new()) as Box<dyn EthSigner>);
	}

	io.merge(
		Eth::<B, C, P, CT, BE, A, CIDP, EC>::new(
			client.clone(),
			pool.clone(),
			graph.clone(),
			converter,
			sync.clone(),
			signers,
			overrides.clone(),
			frontier_backend.clone(),
			is_authority,
			block_data_cache.clone(),
			fee_history_cache,
			fee_history_cache_limit,
			execute_gas_limit_multiplier,
			forced_parent_hashes,
			pending_create_inherent_data_providers,
			None,
		)
		.replace_config::<EC>()
		.into_rpc(),
	)?;

	if let Some(filter_pool) = filter_pool {
		io.merge(
			EthFilter::new(
				client.clone(),
				frontier_backend.clone(),
				graph.clone(),
				filter_pool,
				500_usize, // max stored filters
				max_past_logs,
				block_data_cache.clone(),
			)
			.into_rpc(),
		)?;
	}

	io.merge(
		EthPubSub::new(
			pool,
			client.clone(),
			sync,
			subscription_task_executor,
			overrides.clone(),
			pubsub_notification_sinks,
		)
		.into_rpc(),
	)?;

	io.merge(
		Net::new(
			client.clone(),
			network,
			// Whether to format the `peer_count` response as Hex (default) or not.
			true,
		)
		.into_rpc(),
	)?;

	io.merge(Web3::new(client.clone()).into_rpc())?;

	io.merge(
		Debug::new(
			client.clone(),
			frontier_backend,
			overrides,
			block_data_cache,
		)
		.into_rpc(),
	)?;

	#[cfg(feature = "txpool")]
	io.merge(TxPool::new(client, graph).into_rpc())?;

	Ok(io)
}


/// Avalailable frontier backend types.
#[derive(Debug, Copy, Clone, Default, clap::ValueEnum)]
pub enum BackendType {
	/// Either RocksDb or ParityDb as per inherited from the global backend settings.
	#[default]
	KeyValue,
	/// Sql database with custom log indexing.
	Sql,
}


/// The ethereum-compatibility configuration used to run a node.
#[derive(Clone, Debug, clap::Parser)]
pub struct EthConfiguration {
	/// Maximum number of logs in a query.
	#[arg(long, default_value = "10000")]
	pub max_past_logs: u32,

	/// Maximum fee history cache size.
	#[arg(long, default_value = "2048")]
	pub fee_history_limit: u64,

	#[arg(long)]
	pub enable_dev_signer: bool,

	/// The dynamic-fee pallet target gas price set by block author
	#[arg(long, default_value = "1")]
	pub target_gas_price: u64,

	/// Maximum allowed gas limit will be `block.gas_limit * execute_gas_limit_multiplier`
	/// when using eth_call/eth_estimateGas.
	#[arg(long, default_value = "10")]
	pub execute_gas_limit_multiplier: u64,

	/// Size in bytes of the LRU cache for block data.
	#[arg(long, default_value = "50")]
	pub eth_log_block_cache: usize,

	/// Size in bytes of the LRU cache for transactions statuses data.
	#[arg(long, default_value = "50")]
	pub eth_statuses_cache: usize,

	/// Sets the frontier backend type (KeyValue or Sql)
	#[arg(long, value_enum, ignore_case = true, default_value_t = BackendType::default())]
	pub frontier_backend_type: BackendType,

	// Sets the SQL backend's pool size.
	#[arg(long, default_value = "100")]
	pub frontier_sql_backend_pool_size: u32,

	/// Sets the SQL backend's query timeout in number of VM ops.
	#[arg(long, default_value = "10000000")]
	pub frontier_sql_backend_num_ops_timeout: u32,

	/// Sets the SQL backend's auxiliary thread limit.
	#[arg(long, default_value = "4")]
	pub frontier_sql_backend_thread_count: u32,

	/// Sets the SQL backend's query timeout in number of VM ops.
	/// Default value is 200MB.
	#[arg(long, default_value = "209715200")]
	pub frontier_sql_backend_cache_size: u64,
}

// pub struct FrontierPartialComponents {
// 	pub filter_pool: Option<FilterPool>,
// 	pub fee_history_cache: FeeHistoryCache,
// 	pub fee_history_cache_limit: FeeHistoryCacheLimit,
// }

pub fn new_frontier_partial(
	config: &EthConfiguration,
) -> Result<FrontierPartialComponents, ServiceError> {
	Ok(FrontierPartialComponents {
		filter_pool: Some(Arc::new(Mutex::new(BTreeMap::new()))),
		fee_history_cache: Arc::new(Mutex::new(BTreeMap::new())),
		fee_history_cache_limit: config.fee_history_limit,
	})
}

/// Instantiate all full RPC extensions.
pub fn create_full<C, P, BE, A, CT, SO,CIDP>(
    deps: FullDeps<C, P, A, CT, SO, CIDP>,
    subscription_task_executor: SubscriptionTaskExecutor,
	pubsub_notification_sinks: Arc<
		fc_mapping_sync::EthereumBlockNotificationSinks<
			fc_mapping_sync::EthereumBlockNotification<Block>,
		>,
	>,
) -> Result<RpcModule<()>, Box<dyn std::error::Error + Send + Sync>>
where
    C: ProvideRuntimeApi<Block>
        + HeaderBackend<Block>
        + HeaderMetadata<Block, Error = BlockChainError>
        + StorageProvider<Block, BE>
        + Send
        + Sync
        + 'static,
	C: CallApiAt<Block>	,
	C::Api: fp_rpc::ConvertTransactionRuntimeApi<Block>,
    BE: sc_client_api::Backend<Block> + 'static,
	C::Api: fp_rpc::EthereumRuntimeRPCApi<Block>,
	C: BlockchainEvents<Block> + AuxStore + UsageProvider<Block> + StorageProvider<Block, BE>,

	CT: fp_rpc::ConvertTransaction<<Block as BlockT>::Extrinsic> + Send + Sync + 'static,
	CIDP: CreateInherentDataProviders<Block, ()> + Send + 'static,

    C::Api: substrate_frame_rpc_system::AccountNonceApi<Block, AccountId, Nonce>
        + pallet_transaction_payment_rpc::TransactionPaymentRuntimeApi<Block, Balance>
        + BlockBuilder<Block>,
		P: TransactionPool<Block = Block> + 'static,
		A: ChainApi<Block = Block> + 'static,
    SO: SyncOracle + Send + Sync + 'static,
{
    use pallet_transaction_payment_rpc::{TransactionPayment, TransactionPaymentApiServer};
    use substrate_frame_rpc_system::{System, SystemApiServer};

    let mut module = RpcModule::new(());
    let FullDeps {
        client,
        pool,
        deny_unsafe,
        import_justification_tx,
        justification_translator,
        eth,
        sync_oracle,
        validator_address_cache,
        
    } = deps;

    module.merge(System::new(client.clone(), pool, deny_unsafe).into_rpc())?;

    module.merge(TransactionPayment::new(client.clone()).into_rpc())?;



    use crate::aleph_node_rpc::{AlephNode, AlephNodeApiServer};
    module.merge(
        AlephNode::new(
            import_justification_tx,
            justification_translator,
            client,
            sync_oracle,
            validator_address_cache,
        )
        .into_rpc(),
    )?;
		// Ethereum compatibility RPCs
		let module = create_eth::<_, _, _, _, _, _, _, DefaultEthConfig<C, BE>>(
			module,
			eth,
			subscription_task_executor,
			pubsub_notification_sinks,
		)?;
	
	

    Ok(module)
}
