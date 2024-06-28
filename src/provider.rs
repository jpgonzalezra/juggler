use alloy::providers::RootProvider;
use alloy::pubsub::PubSubFrontend;
use alloy::pubsub::Subscription;
use alloy::transports::{RpcError, TransportError, TransportErrorKind};
use serde::de::DeserializeOwned;
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio_stream::StreamExt;
use tracing::{error, info, warn};

#[derive(Clone, Debug)]
pub enum WsOrIpc {
    Ws(RootProvider<PubSubFrontend>),
    Ipc(RootProvider<PubSubFrontend>),
}

pub struct RpcBalancer {
    providers: Vec<Arc<WsOrIpc>>,
    current: AtomicUsize,
}

impl RpcBalancer {
    pub fn new(providers: Vec<Arc<WsOrIpc>>) -> Self {
        RpcBalancer {
            providers,
            current: AtomicUsize::new(0),
        }
    }

    pub fn next_provider(&self) -> Arc<WsOrIpc> {
        let index = self.current.fetch_add(1, Ordering::SeqCst) % self.providers.len();
        self.providers[index].clone()
    }

    pub async fn execute<'a, F, Fut, U>(&'a self, mut func: F) -> Result<U, TransportError>
    where
        F: FnMut(Arc<WsOrIpc>) -> Fut,
        Fut: std::future::Future<Output = Result<U, TransportError>> + 'a,
    {
        for _ in 0..self.providers.len() {
            let provider = self.next_provider();
            match func(provider.clone()).await {
                Ok(result) => {
                    return Ok(result);
                }
                Err(_) => continue,
            }
        }
        Err(TransportError::local_usage_str("All providers failed"))
    }

    pub async fn subscribe<'a, P, R, F, Fut>(
        &'a self,
        params: P,
        mut handle_data: F,
    ) -> Result<(), TransportError>
    where
        P: for<'b> Fn(
                &'b RootProvider<PubSubFrontend>,
            ) -> Pin<
                Box<
                    dyn Future<Output = Result<Subscription<R>, RpcError<TransportErrorKind>>>
                        + Send
                        + 'b,
                >,
            > + 'a,
        R: 'static + DeserializeOwned + std::fmt::Debug,
        F: FnMut(R) -> Fut,
        Fut: std::future::Future<Output = ()> + 'static,
    {
        loop {
            match self.get_subscription(&params).await {
                Ok(subscription) => {
                    let mut stream = subscription.into_stream();
                    while let Some(data) = stream.next().await {
                        info!("New block: {:?}", data);
                        handle_data(data).await;
                    }
                }
                Err(e) => {
                    error!("Error obtaining subscription: {:?}", e);
                }
            }
        }
    }

    async fn get_subscription<'a, P, R>(
        &'a self,
        params: &P,
    ) -> Result<Subscription<R>, TransportError>
    where
        P: for<'b> Fn(
                &'b RootProvider<PubSubFrontend>,
            ) -> Pin<
                Box<
                    dyn Future<Output = Result<Subscription<R>, RpcError<TransportErrorKind>>>
                        + Send
                        + 'b,
                >,
            > + 'a,
        R: 'static + DeserializeOwned + std::fmt::Debug,
    {
        for _ in 0..self.providers.len() {
            let provider = self.next_provider();
            match self.try_subscribe(provider.clone(), params).await {
                Ok(subscription) => return Ok(subscription),
                Err(e) => {
                    warn!("Failed to subscribe with the current provider: {:?}. Switching to a different provider and retrying...", e);
                    continue;
                }
            }
        }
        Err(TransportError::local_usage_str("All providers failed"))
    }

    async fn try_subscribe<'a, P, R>(
        &'a self,
        provider: Arc<WsOrIpc>,
        params: &P,
    ) -> Result<Subscription<R>, TransportError>
    where
        P: for<'b> Fn(
                &'b RootProvider<PubSubFrontend>,
            ) -> Pin<
                Box<
                    dyn Future<Output = Result<Subscription<R>, RpcError<TransportErrorKind>>>
                        + Send
                        + 'b,
                >,
            > + 'a,
        R: 'static + DeserializeOwned + std::fmt::Debug,
    {
        match &*provider {
            WsOrIpc::Ws(ws_provider) => {
                let subscription = params(ws_provider).await?;
                Ok(subscription)
            }
            WsOrIpc::Ipc(ipc_provider) => {
                let subscription = params(ipc_provider).await?;
                Ok(subscription)
            }
        }
    }
}
