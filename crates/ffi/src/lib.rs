#![allow(missing_docs)]

use std::sync::Arc;

use futures_util::StreamExt;
use lightwallet_core::tonic::transport::{Channel, ClientTlsConfig, Endpoint};
use lightwallet_core::{CanonicalIndexerClient, IndexerClient, NetworkParams};

uniffi::setup_scaffolding!();

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiError {
    #[error("connect failed: {message}")]
    Connect { message: String },
    #[error("rpc failed: {message}")]
    Rpc { message: String },
}

#[derive(uniffi::Record)]
pub struct BlockSummary {
    pub height: u64,
    pub hash: String,
    pub tx_count: u32,
}

// Foreign-implemented: Kotlin backs this with a callbackFlow, so a server
// stream reaches the app as a Flow without a stream type crossing FFI.
#[uniffi::export(foreign)]
pub trait BlockSink: Send + Sync {
    fn on_block(&self, block: BlockSummary);
    fn on_error(&self, message: String);
    fn on_complete(&self);
}

#[derive(uniffi::Object)]
pub struct IndexerConnection {
    inner: CanonicalIndexerClient<Channel>,
}

#[uniffi::export(async_runtime = "tokio")]
impl IndexerConnection {
    #[uniffi::constructor]
    pub async fn connect(url: String) -> Result<Arc<Self>, FfiError> {
        let channel = endpoint(&url)?
            .connect()
            .await
            .map_err(|e| FfiError::Connect {
                message: e.to_string(),
            })?;
        // Blank until GetLightdInfo discovery lands; the RPCs here don't read it.
        let params = NetworkParams {
            chain_name: String::new(),
            activation_heights: Default::default(),
            consensus_branch_id: 0,
        };
        Ok(Arc::new(Self {
            inner: CanonicalIndexerClient::new(channel, params),
        }))
    }

    pub async fn latest_height(&self) -> Result<u64, FfiError> {
        self.inner.get_latest_height().await.map_err(rpc)
    }

    pub async fn stream_block_range(
        &self,
        start: u64,
        end: u64,
        sink: Arc<dyn BlockSink>,
    ) -> Result<(), FfiError> {
        let mut stream = self.inner.get_block_range(start, end).await.map_err(rpc)?;
        while let Some(item) = stream.next().await {
            match item {
                Ok(block) => sink.on_block(BlockSummary {
                    height: block.height,
                    hash: hex::encode(&block.hash),
                    tx_count: block.vtx.len() as u32,
                }),
                Err(e) => {
                    sink.on_error(rpc(e).to_string());
                    return Ok(());
                }
            }
        }
        sink.on_complete();
        Ok(())
    }
}

fn endpoint(url: &str) -> Result<Endpoint, FfiError> {
    let connect = |e: &dyn std::fmt::Display| FfiError::Connect {
        message: e.to_string(),
    };
    let endpoint = Endpoint::from_shared(url.to_owned()).map_err(|e| connect(&e))?;
    if url.starts_with("https") {
        endpoint
            .tls_config(ClientTlsConfig::new().with_webpki_roots())
            .map_err(|e| connect(&e))
    } else {
        Ok(endpoint)
    }
}

fn rpc(e: lightwallet_core::Error) -> FfiError {
    FfiError::Rpc {
        message: e.to_string(),
    }
}
