// Copyright 2019-2021 Parity Technologies (UK) Ltd.
// This file is part of Cumulus.

// Cumulus is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Cumulus is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Cumulus.  If not, see <http://www.gnu.org/licenses/>.

use std::{marker::PhantomData, sync::Arc};

use sp_api::ProvideRuntimeApi;
use sp_block_builder::BlockBuilder as BlockBuilderApi;
use sp_blockchain::Result as ClientResult;
use sp_consensus::{
	error::Error as ConsensusError,
	import_queue::{BasicQueue, CacheKeyId, Verifier as VerifierT},
	BlockImport, BlockImportParams, BlockOrigin, ForkChoiceStrategy,
};
use sp_inherents::{CreateInherentDataProviders, InherentDataProvider};
use sp_runtime::{
	generic::BlockId,
	traits::{Block as BlockT, Header as HeaderT},
	Justifications,
};

/// A verifier that just checks the inherents.
struct Verifier<Client, Block, CIDP> {
	client: Arc<Client>,
	create_inherent_data_providers: CIDP,
	_marker: PhantomData<Block>,
}

#[async_trait::async_trait]
impl<Client, Block, CIDP> VerifierT<Block> for Verifier<Client, Block, CIDP>
where
	Block: BlockT,
	Client: ProvideRuntimeApi<Block> + Send + Sync,
	<Client as ProvideRuntimeApi<Block>>::Api: BlockBuilderApi<Block>,
	CIDP: CreateInherentDataProviders<Block, ()>,
{
	async fn verify(
		&mut self,
		origin: BlockOrigin,
		header: Block::Header,
		justifications: Option<Justifications>,
		mut body: Option<Vec<Block::Extrinsic>>,
	) -> Result<
		(
			BlockImportParams<Block, ()>,
			Option<Vec<(CacheKeyId, Vec<u8>)>>,
		),
		String,
	> {
		if let Some(inner_body) = body.take() {
			let inherent_data_providers = self
				.create_inherent_data_providers
				.create_inherent_data_providers(*header.parent_hash(), ())
				.await
				.map_err(|e| e.to_string())?;

			let inherent_data = inherent_data_providers
				.create_inherent_data()
				.map_err(|e| format!("{:?}", e))?;

			let block = Block::new(header.clone(), inner_body);

			let inherent_res = self
				.client
				.runtime_api()
				.check_inherents(
					&BlockId::Hash(*header.parent_hash()),
					block.clone(),
					inherent_data,
				)
				.map_err(|e| format!("{:?}", e))?;

			if !inherent_res.ok() {
				for (i, e) in inherent_res.into_errors() {
					match inherent_data_providers.try_handle_error(&i, &e).await {
						Some(r) => r.map_err(|e| format!("{:?}", e))?,
						None => Err(format!(
							"Unhandled inherent error from `{}`.",
							String::from_utf8_lossy(&i)
						))?,
					}
				}
			}

			let (_, inner_body) = block.deconstruct();
			body = Some(inner_body);
		}

		let post_hash = Some(header.hash());
		let mut block_import_params = BlockImportParams::new(origin, header);
		block_import_params.body = body;
		block_import_params.justifications = justifications;

		// Best block is determined by the relay chain, or if we are doing the intial sync
		// we import all blocks as new best.
		block_import_params.fork_choice = Some(ForkChoiceStrategy::Custom(
			origin == BlockOrigin::NetworkInitialSync,
		));
		block_import_params.post_hash = post_hash;

		Ok((block_import_params, None))
	}
}

/// Start an import queue for a Cumulus collator that does not uses any special authoring logic.
pub fn import_queue<Client, Block: BlockT, I, CIDP>(
	client: Arc<Client>,
	block_import: I,
	create_inherent_data_providers: CIDP,
	spawner: &impl sp_core::traits::SpawnEssentialNamed,
	registry: Option<&substrate_prometheus_endpoint::Registry>,
) -> ClientResult<BasicQueue<Block, I::Transaction>>
where
	I: BlockImport<Block, Error = ConsensusError> + Send + Sync + 'static,
	I::Transaction: Send,
	Client: ProvideRuntimeApi<Block> + Send + Sync + 'static,
	<Client as ProvideRuntimeApi<Block>>::Api: BlockBuilderApi<Block>,
	CIDP: CreateInherentDataProviders<Block, ()> + 'static,
{
	let verifier = Verifier {
		client,
		create_inherent_data_providers,
		_marker: PhantomData,
	};

	Ok(BasicQueue::new(
		verifier,
		Box::new(block_import),
		None,
		spawner,
		registry,
	))
}
