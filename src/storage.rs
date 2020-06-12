use crate::common::traits::aux_store::AuxStore;
use crate::common::traits::header_backend::HeaderBackend;
use crate::common::traits::header_metadata::HeaderMetadata;
use crate::common::traits::storage::Storage as StorageT;
use crate::common::types::block_status::BlockStatus;
use crate::common::types::blockchain_error::BlockchainError;
use crate::common::types::blockchain_info::BlockchainInfo;
use crate::common::types::blockchain_result::BlockchainResult;
use crate::common::types::cached_header_metadata::CachedHeaderMetadata;
use crate::common::types::new_block_state::NewBlockState;
use crate::db::Data;
use kvdb::{DBTransaction, KeyValueDB};
use parity_scale_codec::{Decode, Encode};
use sp_runtime::generic::BlockId;
use sp_runtime::traits::{Block as BlockT, Header as HeaderT, NumberFor, One, Zero};
use std::io;

const META_COLUMN: u32 = 0;
const HEADER_COLUMN: u32 = 1;
const AUX_COLUMN: u32 = 2;
const LOOKUP_COLUMN: u32 = 3;

const META_KEY: &[u8] = b"meta";

/// Database metadata.
#[derive(Debug, Encode, Decode)]
struct StorageMeta<N, H>
where
    N: Encode + Decode,
    H: Encode + Decode,
{
    /// Hash of the best known block.
    pub best_hash: H,
    /// Number of the best known block.
    pub best_number: N,
    /// Hash of the best finalized block.
    pub finalized_hash: H,
    /// Number of the best finalized block.
    pub finalized_number: N,
    /// Hash of the genesis block.
    pub genesis_hash: H,
    /// Non finalized blocks at the moment
    pub non_finalized_blocks: u64,
}

fn db_err(err: io::Error) -> BlockchainError {
    BlockchainError::Backend(format!("{}", err))
}

fn codec_error(err: parity_scale_codec::Error) -> BlockchainError {
    BlockchainError::DataDecode(err.to_string())
}

pub struct Storage {
    data: Data,
    max_non_finalized_blocks_allowed: u64,
}

impl Storage {
    pub fn new(data: Data, max_non_finalized_blocks_allowed: u64) -> Self {
        Self {
            data,
            max_non_finalized_blocks_allowed,
        }
    }

    fn fetch_meta<N, H>(&self) -> BlockchainResult<Option<StorageMeta<N, H>>>
    where
        N: Encode + Decode,
        H: Encode + Decode,
    {
        let possible_encoded_meta = self.data.db.get(META_COLUMN, META_KEY).map_err(db_err)?;
        if possible_encoded_meta.is_none() {
            Ok(None)
        } else {
            let encoded_meta = possible_encoded_meta.unwrap();
            Ok(Some(
                StorageMeta::decode(&mut encoded_meta.as_slice()).map_err(codec_error)?,
            ))
        }
    }

    fn store_meta<N, H>(&self, meta: StorageMeta<N, H>) -> BlockchainResult<()>
    where
        N: Encode + Decode,
        H: Encode + Decode,
    {
        let mut tx = self.data.db.transaction();
        Self::tx_store_meta(&mut tx, &meta);
        self.data.db.write(tx).map_err(db_err)
    }

    fn tx_store_meta<N, H>(tx: &mut DBTransaction, meta: &StorageMeta<N, H>)
    where
        N: Encode + Decode,
        H: Encode + Decode,
    {
        tx.put(META_COLUMN, META_KEY, meta.encode().as_slice());
    }

    fn tx_store_header<Block>(tx: &mut DBTransaction, header: &Block::Header)
    where
        Block: BlockT,
    {
        let id = Self::header_hash_to_id::<Block>(&header.hash());
        tx.put(HEADER_COLUMN, id.as_slice(), header.encode().as_slice());
    }

    fn tx_delete_header<Block>(tx: &mut DBTransaction, hash: &Block::Hash)
    where
        Block: BlockT,
    {
        let id = Self::header_hash_to_id::<Block>(hash);
        tx.delete(HEADER_COLUMN, id.as_slice());
    }

    fn header_hash_to_id<Block>(hash: &Block::Hash) -> Vec<u8>
    where
        Block: BlockT,
    {
        hash.encode()
    }

    fn id<Block>(&self, block_id: BlockId<Block>) -> BlockchainResult<Option<Vec<u8>>>
    where
        Block: BlockT,
    {
        match block_id {
            BlockId::Hash(h) => Ok(Some(Self::header_hash_to_id::<Block>(&h))),
            BlockId::Number(n) => {
                let data = self
                    .data
                    .db
                    .get(LOOKUP_COLUMN, n.encode().as_slice())
                    .map_err(db_err)?;
                if data.is_none() {
                    Ok(None)
                } else {
                    Ok(Some(data.unwrap().to_vec()))
                }
            }
        }
    }

    fn header_hash<Block>(&self, number: NumberFor<Block>) -> BlockchainResult<Option<Block::Hash>>
    where
        Block: BlockT,
    {
        let data = self
            .data
            .db
            .get(LOOKUP_COLUMN, number.encode().as_slice())
            .map_err(db_err)?;
        if data.is_none() {
            Ok(None)
        } else {
            let encoded_header = data.unwrap();
            Ok(Some(
                Block::Hash::decode(&mut encoded_header.as_slice()).map_err(codec_error)?,
            ))
        }
    }
}

impl AuxStore for Storage {
    fn insert_aux<
        'a,
        'b: 'a,
        'c: 'a,
        I: IntoIterator<Item = &'a (&'c [u8], &'c [u8])>,
        D: IntoIterator<Item = &'a &'b [u8]>,
    >(
        &self,
        insert: I,
        delete: D,
    ) -> BlockchainResult<()> {
        let mut tx = self.data.db.transaction();
        for (k, v) in insert {
            tx.put(AUX_COLUMN, *k, *v);
        }

        for k in delete {
            tx.delete(AUX_COLUMN, *k)
        }

        self.data.db.write(tx).map_err(db_err)
    }

    fn get_aux(&self, key: &[u8]) -> BlockchainResult<Option<Vec<u8>>> {
        self.data.db.get(AUX_COLUMN, key).map_err(db_err)
    }
}

impl<Block> HeaderBackend<Block> for Storage
where
    Block: BlockT,
{
    fn header(&self, id: BlockId<Block>) -> BlockchainResult<Option<Block::Header>> {
        let possible_header_key = self.id(id)?;
        if possible_header_key.is_none() {
            Ok(None)
        } else {
            let header_key = possible_header_key.unwrap();
            let possible_encoded_header = self
                .data
                .db
                .get(HEADER_COLUMN, header_key.as_slice())
                .map_err(db_err)?;
            if possible_encoded_header.is_none() {
                Ok(None)
            } else {
                let encoded_header = possible_encoded_header.unwrap();
                let header =
                    Block::Header::decode(&mut encoded_header.as_slice()).map_err(codec_error)?;
                Ok(Some(header))
            }
        }
    }

    fn info(&self) -> BlockchainInfo<Block> {
        let meta = self.fetch_meta();
        let default_info = BlockchainInfo {
            best_hash: Default::default(),
            best_number: Zero::zero(),
            genesis_hash: Default::default(),
            finalized_hash: Default::default(),
            finalized_number: Zero::zero(),
            number_leaves: 0,
        };
        if meta.is_ok() {
            let meta = meta.unwrap();
            if meta.is_none() {
                default_info
            } else {
                let meta = meta.unwrap();
                BlockchainInfo {
                    best_hash: meta.best_hash,
                    best_number: meta.best_number,
                    genesis_hash: meta.genesis_hash,
                    finalized_hash: meta.finalized_hash,
                    finalized_number: meta.finalized_number,
                    number_leaves: 0,
                }
            }
        } else {
            default_info
        }
    }

    fn status(&self, id: BlockId<Block>) -> BlockchainResult<BlockStatus> {
        let possible_header = self.header(id)?;
        if possible_header.is_none() {
            Ok(BlockStatus::Unknown)
        } else {
            Ok(BlockStatus::InChain)
        }
    }

    fn number(
        &self,
        hash: Block::Hash,
    ) -> BlockchainResult<Option<<Block::Header as HeaderT>::Number>> {
        let possible_header: Option<Block::Header> = self.header(BlockId::<Block>::Hash(hash))?;
        if possible_header.is_none() {
            Ok(None)
        } else {
            let header = possible_header.unwrap();
            Ok(Some(*header.number()))
        }
    }

    fn hash(&self, number: NumberFor<Block>) -> BlockchainResult<Option<Block::Hash>> {
        self.header_hash::<Block>(number)
    }
}

impl<Block> StorageT<Block> for Storage
where
    Block: BlockT,
{
    /// Store new header. Should refuse to revert any finalized blocks.
    ///
    /// Takes new authorities, the leaf state of the new block, and
    /// any auxiliary storage updates to place in the same operation.
    fn import_header(
        &self,
        header: Block::Header,
        state: NewBlockState,
        aux_ops: Vec<(Vec<u8>, Option<Vec<u8>>)>,
    ) -> BlockchainResult<()> {
        assert!(
            state.is_best(),
            "Since, we are only following one fork block state must need to be best"
        );

        let possible_meta = self.fetch_meta()?;
        let mut meta: StorageMeta<NumberFor<Block>, Block::Hash> = if possible_meta.is_none() {
            StorageMeta {
                best_hash: Default::default(),
                best_number: Zero::zero(),
                finalized_hash: Default::default(),
                finalized_number: Zero::zero(),
                genesis_hash: Default::default(),
                non_finalized_blocks: 0,
            }
        } else {
            possible_meta.unwrap()
        };

        if meta.non_finalized_blocks >= self.max_non_finalized_blocks_allowed {
            return Err(BlockchainError::Backend(format!(
                "Cannot import any more blocks, before finalizing previous blocks"
            )));
        }

        let possible_header = self.header(BlockId::<Block>::Hash(header.hash()))?;
        if possible_header.is_some() {
            // We have already imported this block
            return Ok(());
        }

        let first_imported_header = meta.best_hash == Default::default();

        // We need to check if this is child of last best header
        if !first_imported_header {
            let possible_parent_header = self.header(BlockId::<Block>::Hash(meta.best_hash))?;
            if possible_parent_header.is_none() {
                return Err(BlockchainError::UnknownBlock(format!(
                    "Could not find parent of importing block"
                )));
            }
            let parent_header = possible_parent_header.unwrap();
            if *header.parent_hash() != parent_header.hash()
                || header.number() <= parent_header.number()
            {
                return Err(BlockchainError::NotInFinalizedChain);
            }
            if *header.number() != meta.best_number + One::one() {
                return Err(BlockchainError::NonSequentialFinalization(format!(
                    "tried to import non sequential block. Expected block number: {}. Got: {}",
                    meta.best_number + One::one(),
                    *header.number()
                )));
            }
        } else {
            meta.genesis_hash = header.hash();
        }

        meta.non_finalized_blocks += 1;
        meta.best_hash = header.hash();
        meta.best_number = *header.number();

        let mut tx = self.data.db.transaction();
        Self::tx_store_meta(&mut tx, &meta);
        Self::tx_store_header::<Block>(&mut tx, &header);
        self.data.db.write(tx).map_err(db_err)
    }

    /// Set an existing block as new best block.
    fn set_head(&self, block: BlockId<Block>) -> BlockchainResult<()> {
        unimplemented!()
    }

    /// Mark historic header as finalized.
    fn finalize_header(&self, block: BlockId<Block>) -> BlockchainResult<()> {
        let possible_to_be_finalized_header = self.header(block)?;
        if possible_to_be_finalized_header.is_none() {
            return Err(BlockchainError::UnknownBlock(format!(
                "Error: {}",
                "Could not find block header to finalize"
            )));
        }
        let to_be_finalized_header = possible_to_be_finalized_header.unwrap();
        let possible_meta = self.fetch_meta()?;
        if possible_meta.is_none() {
            return Err(BlockchainError::Backend(format!(
                "Error: {}",
                "Unable to get metadata about blockchain"
            )));
        }
        let mut meta: StorageMeta<NumberFor<Block>, Block::Hash> = possible_meta.unwrap();
        let first_block_to_be_finalized = meta.finalized_hash == Default::default();

        if (!first_block_to_be_finalized
            && *to_be_finalized_header.parent_hash() != meta.finalized_hash)
            || (first_block_to_be_finalized && to_be_finalized_header.hash() != meta.genesis_hash)
        {
            return Err(BlockchainError::NonSequentialFinalization(format!("Error: {}", "to be finalized block need to be child of last finalized block or first block itself")));
        }

        meta.non_finalized_blocks -= 1;
        meta.finalized_hash = to_be_finalized_header.hash();
        meta.finalized_number = *to_be_finalized_header.number();

        let mut tx = self.data.db.transaction();
        Self::tx_store_meta(&mut tx, &meta);
        if !first_block_to_be_finalized {
            Self::tx_delete_header::<Block>(&mut tx, to_be_finalized_header.parent_hash());
        }
        self.data.db.write(tx).map_err(db_err)
    }

    /// Get last finalized header.
    fn last_finalized(&self) -> BlockchainResult<Block::Hash> {
        let possible_meta: Option<StorageMeta<NumberFor<Block>, Block::Hash>> =
            self.fetch_meta()?;
        if possible_meta.is_none() {
            return Err(BlockchainError::Backend(format!(
                "Error: {}",
                "Unable to get metadata about blockchain"
            )));
        }
        Ok(possible_meta.unwrap().finalized_hash)
    }
}

impl<Block> HeaderMetadata<Block> for Storage
where
    Block: BlockT,
{
    type Error = BlockchainError;

    fn header_metadata(
        &self,
        hash: Block::Hash,
    ) -> Result<CachedHeaderMetadata<Block>, Self::Error> {
        let possible_header = self.header(BlockId::<Block>::Hash(hash))?;
        if possible_header.is_none() {
            Err(BlockchainError::UnknownBlock(format!(
                "header not found in db: {}",
                hash
            )))
        } else {
            let header = possible_header.unwrap();
            Ok(CachedHeaderMetadata::from(&header))
        }
    }

    fn insert_header_metadata(
        &self,
        hash: Block::Hash,
        header_metadata: CachedHeaderMetadata<Block>,
    ) {
        unimplemented!()
    }

    fn remove_header_metadata(&self, hash: Block::Hash) {
        unimplemented!()
    }
}
