use std::borrow::Cow;
use std::future::Future;
use std::marker::PhantomData;

use bincode::{Decode, Encode, config::standard};
use byteorder::LE;
use futures::channel::oneshot::{self, Canceled};
use heed::{BytesDecode, BytesEncode, types::U64, Env};
use tracing::{trace, warn};

use crate::{
    database::Database,
    utils::error::Error,
    utils::hash::hash,
    world::chunk_format::Chunk
};

use super::LMDB_THREADPOOL;

pub struct Zstd<T>(PhantomData<T>);

impl<'a, T: Encode + 'a> BytesEncode<'a> for Zstd<T> {
    type EItem = T;

    fn bytes_encode(item: &'a Self::EItem) -> Result<Cow<'a, [u8]>, heed::BoxedError> {
        
        // Compress
        let mut bytes = Vec::new();
        let mut compressor = zstd::Encoder::new(&mut bytes, 6)?;
        bincode::encode_into_std_write(item, &mut compressor, standard())?;
        
        Ok(Cow::Owned(bytes))
    }
}

impl<'a, T: Decode + 'a> BytesDecode<'a> for Zstd<T> {
    type DItem = T;

    fn bytes_decode(bytes: &'a [u8]) -> Result<Self::DItem, heed::BoxedError> {
        
        let mut decompressor = zstd::Decoder::new(bytes)?;
        let decoded = bincode::decode_from_std_read(&mut decompressor, standard())?;
        Ok(decoded)
    }
}

// Will delegate a database operation to the database threadpool
pub(super) fn spawn_blocking_db<F, R>(f: F) -> impl Future<Output = Result<R,Canceled>> 
where  
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let (tx,res) = oneshot::channel::<R>();
    
    let pool = LMDB_THREADPOOL.get().unwrap();
    pool.spawn(move || {
        if tx.send(f()).is_err() {
            tracing::warn!("A database task has been unable to send its result because the receiver at other end have closed.")
        }
    });
    
    res
}

impl Database {
    
    // Close the database
    pub fn close(self) {
        let token = self.db.prepare_for_closing();
        token.wait();
    }
    
    /// Fetch chunk from database
    fn get_chunk_from_database(db: &Env, key: &u64) -> Result<Option<Chunk>, Error> {
        // Initialize read transaction and open chunks table
        let ro_tx = db.read_txn()?;
        let database = db
            .open_database::<U64<LE>, Zstd<Chunk>>(&ro_tx, Some("chunks"))?
            .expect("No table \"chunks\" found. The database should have been initialized");

        // Attempt to fetch chunk from table
        database.get(&ro_tx, key)
            .map_err(|err| Error::DatabaseError(format!("Failed to get chunk: {err}")))
    }

    /// Insert a single chunk into database
    fn insert_chunk_into_database(db: &Env, chunk: &Chunk) -> Result<(), Error> {
        // Initialize write transaction and open chunks table
        let mut rw_tx = db.write_txn()?;
        let database = db
            .open_database::<U64<LE>, Zstd<Chunk>>(&rw_tx, Some("chunks"))?
            .expect("No table \"chunks\" found. The database should have been initialized");

        // Calculate key
        let key = hash((chunk.dimension.as_ref().unwrap(), chunk.x_pos, chunk.z_pos));

        // Insert chunk
        let res = database.put(&mut rw_tx, &key, chunk);
        rw_tx.commit().map_err(|err| {
            Error::DatabaseError(format!("Unable to commit changes to database: {err}"))
        })?;

        if let Err(err) = res {
            Err(Error::DatabaseError(format!(
                "Failed to insert or update chunk: {err}"
            )))
        } else {
            Ok(())
        }
    }

    /// Insert multiple chunks into database
    /// TODO: Find better name/disambiguation
    fn insert_chunks_into_database(db: &Env, chunks: &[Chunk]) -> Result<(), Error> {
        // Initialize write transaction and open chunks table
        let mut rw_tx = db.write_txn()?;
        let database = db
            .open_database::<U64<LE>, Zstd<Chunk>>(&rw_tx, Some("chunks"))?
            .expect("No table \"chunks\" found. The database should have been initialized");

        // Update page
        for chunk in chunks {
            // Calculate key
            let key = hash((chunk.dimension.as_ref().unwrap(), chunk.x_pos, chunk.z_pos));

            // Insert chunk
            database.put(&mut rw_tx, &key, chunk).map_err(|err| {
                Error::DatabaseError(format!("Failed to insert or update chunk: {err}"))
            })?;
        }

        // Commit changes
        rw_tx.commit().map_err(|err| {
            Error::DatabaseError(format!("Unable to commit changes to database: {err}"))
        })?;
        Ok(())
    }

    async fn load_into_cache(&self, key: u64) -> Result<(), Error> {
        let db = self.db.clone();
        let cache = self.cache.clone();

        tokio::task::spawn(async move {
            
            // Check cache
            if cache.contains_key(&key) {
                trace!("Chunk already exists in cache: {:X}", key);
            }
            // If not in cache then search in database
            else if let Ok(chunk) =
                spawn_blocking_db(move || Self::get_chunk_from_database(&db, &key))
                    .await
                    .unwrap()
            {
                if let Some(chunk) = chunk {
                    cache.insert(key, chunk).await;
                } else {
                    warn!(
                        "Chunk does not exist in db, can't load into cache: {:X}",
                        key,
                    );
                }
            }
            // The chunk don't exist
            else {
                warn!("Error getting chunk: {:X}", key,);
            }
        })
            .await?;
        Ok(())
    }

    /// Insert a chunk into the database <br>
    /// This will also insert the chunk into the cache <br>
    /// If the chunk already exists, it will return an error
    /// # Arguments
    /// * `value` - The chunk to insert
    /// # Returns
    /// * `Result<(), Error>` - Ok if the chunk was inserted, Err if the chunk already exists
    /// # Example
    /// ```no_run
    /// use crate::world::chunkformat::Chunk;
    /// use crate::database::Database;
    /// use crate::utils::error::Error;
    ///
    /// async fn insert_chunk(database: Database, chunk: Chunk) -> Result<(), Error> {
    ///    database.insert_chunk(chunk).await
    /// }
    ///
    /// ```
    pub async fn insert_chunk(&self, value: Chunk) -> Result<(), Error> {
        // Calculate key of this chunk
        // WARNING: This key wasn't supposed to include value.dimension in the tuple, but it was different from the key used in persistent database most likely a bug.
        let key = hash((value.dimension.as_ref().unwrap(), value.x_pos, value.z_pos));

        // Insert chunk into persistent database
        let chunk = value.clone();
        let db = self.db.clone();
        spawn_blocking_db(move || Self::insert_chunk_into_database(&db, &chunk))
            .await
            .unwrap()?;

        // Insert into cache
        self.cache.insert(key, value).await;
        Ok(())
    }

    /// Get a chunk from the database <br>
    /// This will also insert the chunk into the cache <br>
    /// If the chunk does not exist, it will return None
    /// # Arguments
    /// * `x` - The x position of the chunk
    /// * `z` - The z position of the chunk
    /// * `dimension` - The dimension of the chunk
    /// # Returns
    /// * `Result<Option<Chunk>, Error>` - Ok if the chunk was found, Err if the chunk does not exist
    /// # Example
    /// ```no_run
    /// use crate::world::chunkformat::Chunk;
    /// use crate::database::Database;
    /// use crate::utils::error::Error;
    ///
    /// async fn get_chunk(database: Database, x: i32, z: i32, dimension: String) -> Result<Option<Chunk>, Error> {
    ///   database.get_chunk(x, z, dimension).await
    /// }
    ///
    /// ```
    pub async fn get_chunk(
        &self,
        x: i32,
        z: i32,
        dimension: String,
    ) -> Result<Option<Chunk>, Error> {
        // Calculate key of this chunk and clone database pointer
        let key = hash((dimension, x, z));
        let db = self.db.clone();

        // First check cache
        if self.cache.contains_key(&key) {
            Ok(self.cache.get(&key).await)
        }
        // Attempt to get chunk from persistent database
        else if let Some(chunk) = spawn_blocking_db(move || Self::get_chunk_from_database(&db, &key))
            .await
            .unwrap()?
        {
            self.cache.insert(key, chunk.clone()).await;
            Ok(Some(chunk))
        }
        // Chunk do not exist
        else {
            Ok(None)
        }
    }

    /// Check if a chunk exists in the database
    /// # Arguments
    /// * `x` - The x position of the chunk
    /// * `z` - The z position of the chunk
    /// * `dimension` - The dimension of the chunk
    /// # Returns
    ///
    /// * `Result<bool, Error>` - Ok if the chunk exists, Err if the chunk does not exist
    /// # Example
    /// ```no_run
    /// use crate::database::Database;
    /// use crate::utils::error::Error;
    ///
    /// async fn chunk_exists(database: Database, x: i32, z: i32, dimension: String) -> Result<bool, Error> {
    ///  database.chunk_exists(x, z, dimension).await
    /// }
    ///
    /// ```
    pub async fn chunk_exists(&self, x: i32, z: i32, dimension: String) -> Result<bool, Error> {
        // Calculate key and copy database pointer
        let key = hash((dimension, x, z));
        let db = self.db.clone();

        // Check first cache
        if self.cache.contains_key(&key) {
            Ok(true)
        // Else check persistent database and load it into cache
        } else {
            let res = spawn_blocking_db(move || Self::get_chunk_from_database(&db, &key)).await.unwrap();

            // WARNING: The previous logic was to order the chunk to be loaded into cache whether it existed or not.
            // This has been replaced by directly loading the queried chunk into cache
            match res {
                Ok(opt) => {
                    let exist = opt.is_some();
                    if let Some(chunk) = opt {
                        self.cache.insert(key, chunk).await;
                    }
                    Ok(exist)
                }
                Err(err) => Err(err),
            }
        }
    }

    /// Update a chunk in the database <br>
    /// This will also update the chunk in the cache <br>
    /// If the chunk does not exist, it will return an error
    /// # Arguments
    /// * `value` - The chunk to update
    /// # Returns
    /// * `Result<(), Error>` - Ok if the chunk was updated, Err if the chunk does not exist
    /// # Example
    /// ```no_run
    /// use crate::world::chunkformat::Chunk;
    /// use crate::database::Database;
    /// use crate::utils::error::Error;
    ///
    /// async fn update_chunk(database: Database, chunk: Chunk) -> Result<(), Error> {
    ///   database.update_chunk(chunk).await
    /// }
    ///
    /// ```
    pub async fn update_chunk(&self, value: Chunk) -> Result<(), Error> {
        // Calculate key of this chunk
        // WARNING: This key wasn't supposed to include value.dimension in the tuple, but it was different from the key used in persistent database most likely a bug.
        let key = hash((value.dimension.as_ref().unwrap(), value.x_pos, value.z_pos));

        // Insert new chunk state into persistent database
        let chunk = value.clone();
        let db = self.db.clone();
        spawn_blocking_db(move || Self::insert_chunk_into_database(&db, &chunk)).await.unwrap()?;

        // Insert new chunk state into cache
        self.cache.insert(key, value).await;
        Ok(())
    }

    /// Batch insert chunks into the database <br>
    /// This will also insert the chunks into the cache <br>
    /// If any of the chunks already exist, it will return an error
    /// # Arguments
    /// * `values` - The chunks to insert
    /// # Returns
    /// * `Result<(), Error>` - Ok if the chunks were inserted, Err if any of the chunks already exist
    /// # Example
    /// ```no_run
    /// use crate::world::chunkformat::Chunk;
    /// use crate::database::Database;
    /// use crate::utils::error::Error;
    ///
    /// async fn batch_insert_chunks(database: Database, chunks: Vec<Chunk>) -> Result<(), Error> {
    ///  database.batch_insert_chunks(chunks).await
    /// }
    ///
    /// ```
    pub async fn batch_insert(&self, values: Vec<Chunk>) -> Result<(), Error> {
        /*

        trace!("processing chunks (compressing and encoding)");
        // Process chunks in parallel
        let processed_chunks: Vec<(u64, Vec<u8>)> = values
            .par_iter()
            .map(|chunk| {
                let key = hash((
                    chunk.dimension.as_ref().expect(&format!("Invalid chunk @ ({},{})", chunk.x_pos, chunk.z_pos)),
                    chunk.x_pos,
                    chunk.z_pos,
                ));

                let encoded_chunk = encode_to_vec(chunk, standard())
                    .expect("Failed to encode chunk");
                let compressed = zstd_compress(&encoded_chunk, 3)
                    .expect("Failed to compress chunk.")
                    ;

                (key, compressed)
            })
            .collect();
        trace!("processed chunks");*/

        // Insert into cache in parallel
        // TODO: re-enable this?
        /*values.par_iter().for_each(|chunk| {
            let key = hash((
                chunk.dimension.as_ref().expect(&format!("Invalid chunk @ ({},{})", chunk.x_pos, chunk.z_pos)),
                chunk.x_pos,
                chunk.z_pos,
            ));

            // tokio::spawn(self.load_into_cache(key));
            // if let Err(e) = self.cache.insert(key, chunk.clone()) {
            //     warn!("Failed to insert chunk into cache: {:?}", e);
            // }
        });
*/

        /*trace!("Inserting chunks into database");
        // Perform batch insert into LMDB
        spawn_blocking(move || {
            let mut rw_tx = db.write_txn()?;
            let database = db
                .open_database::<U64<LE>, Bytes>(&rw_tx, Some("chunks"))?
                .expect("No table \"chunks\" found. The database should have been initialized");

            for (key, compressed) in processed_chunks {
                database.put(&mut rw_tx, &key, &compressed)?;
            }

            rw_tx.commit()?;
            Ok::<_, Error>(())
        })
            .await??;

        Ok(())*/

        // Clone database pointer
        let db = self.db.clone();

        // Calculate all keys
        let keys = values
            .iter()
            .map(|v| hash((v.dimension.as_ref().expect(format!("Invalid chunk @ ({},{})", v.x_pos, v.z_pos).as_str()), v.x_pos, v.z_pos)))
            .collect::<Vec<u64>>();

        // WARNING: The previous logic was to first insert in database and then insert in cache using load_into_cache fn.
        // This has been modified to avoid having to query database while we already have the data available.
        // First insert into cache
        for (key, chunk) in keys.into_iter().zip(&values) {
            self.cache.insert(key, chunk.clone()).await;
            self.load_into_cache(key).await?;
        }

        // Then insert into persistent database
        spawn_blocking_db(move || Self::insert_chunks_into_database(&db, &values))
            .await
            .unwrap()?;
        Ok(())
    }
}

#[tokio::test]
#[ignore]
async fn dump_chunk() {
    use crate::utils::setup_logger;
    use tokio::net::TcpListener;
    setup_logger().unwrap();
    let state = crate::create_state(TcpListener::bind("0.0.0.0:0").await.unwrap())
        .await
        .unwrap();
    let chunk = state
        .database
        .get_chunk(0, 0, "overworld".to_string())
        .await
        .unwrap()
        .unwrap();
    let outfile = std::fs::File::create("chunk.json").unwrap();
    let mut writer = std::io::BufWriter::new(outfile);
    serde_json::to_writer(&mut writer, &chunk).unwrap();
}
