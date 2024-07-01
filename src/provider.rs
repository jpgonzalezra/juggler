use alloy::providers::Provider;
use alloy::providers::RootProvider;
use alloy::pubsub::PubSubFrontend;
use alloy::pubsub::Subscription;
use alloy::transports::{RpcError, TransportError, TransportErrorKind};
use serde::de::DeserializeOwned;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::sync::RwLock;
use tokio::time::interval;
use tokio::time::sleep;
use tokio_stream::StreamExt;
use tracing::{error, info, warn};

#[derive(Clone, Debug)]
pub enum WsOrIpc {
    Ws(RootProvider<PubSubFrontend>),
    Ipc(RootProvider<PubSubFrontend>),
}
#[derive(Clone, Debug)]
pub struct RpcBalancer {
    pub providers: Arc<RwLock<Vec<Arc<WsOrIpc>>>>,
    pub current: Arc<Mutex<usize>>,
    pub unavailable_providers: Arc<RwLock<VecDeque<Arc<WsOrIpc>>>>,
}

type SubscriptionFuture<'a, R> = Pin<
    Box<dyn Future<Output = Result<Subscription<R>, RpcError<TransportErrorKind>>> + Send + 'a>,
>;

type HandleDataFn<R, Fut> = dyn FnMut(R) -> Fut + Send;

impl RpcBalancer {
    pub fn new(providers: Vec<Arc<WsOrIpc>>) -> Self {
        let balancer = RpcBalancer {
            providers: Arc::new(RwLock::new(providers)),
            current: Arc::new(Mutex::new(0)),
            unavailable_providers: Arc::new(RwLock::new(VecDeque::new())),
        };

        let balancer_clone = balancer.clone();
        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(10)); // TODO: param
            loop {
                interval.tick().await;
                balancer_clone.ping_unavailable_providers().await;
            }
        });

        balancer
    }

    pub async fn next_provider(&self) -> Option<Arc<WsOrIpc>> {
        let providers = self.providers.read().await;
        if providers.is_empty() {
            None
        } else {
            let mut current = self.current.lock().await;
            let index = *current % providers.len();
            *current = *current + 1;
            Some(providers[index].clone())
        }
    }

    pub async fn execute<'a, F, Fut, U>(&'a self, mut func: F) -> Result<U, TransportError>
    where
        F: FnMut(Arc<WsOrIpc>) -> Fut,
        Fut: std::future::Future<Output = Result<U, TransportError>> + 'a,
    {
        let count = self.providers.read().await.len();
        if count == 0 {
            warn!("No available providers. Waiting for 1 second before retrying.");
            sleep(Duration::from_secs(1)).await;
        }
        for _ in 0..count {
            if let Some(provider) = self.next_provider().await {
                match func(provider.clone()).await {
                    Ok(result) => {
                        return Ok(result);
                    }
                    Err(e) => {
                        warn!("Provider failed: {:?}. Moving to unavailable providers.", e);
                        self.move_to_unavailable_providers(provider).await;
                    }
                }
            }
        }
        Err(TransportError::local_usage_str("All providers failed"))
    }

    pub async fn subscribe<'a, P, R, Fut>(
        &'a self,
        params: P,
        handle_data: &'a mut HandleDataFn<R, Fut>,
    ) -> Result<(), TransportError>
    where
        P: for<'b> Fn(&'b RootProvider<PubSubFrontend>) -> SubscriptionFuture<'b, R> + 'a,
        R: 'static + DeserializeOwned + std::fmt::Debug,
        Fut: std::future::Future<Output = ()> + 'static,
    {
        loop {
            match self.get_subscription(&params).await {
                Ok(subscription) => {
                    let mut stream = subscription.into_stream();
                    while let Some(data) = stream.next().await {
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
        P: for<'b> Fn(&'b RootProvider<PubSubFrontend>) -> SubscriptionFuture<'b, R> + 'a,
        R: 'static + DeserializeOwned + std::fmt::Debug,
    {
        let count = self.providers.read().await.len();
        if count == 0 {
            warn!("No available providers. Waiting for 1 second before retrying.");
            sleep(Duration::from_secs(1)).await;
        }
        for _ in 0..count {
            if let Some(provider) = self.next_provider().await {
                match self.try_subscribe(provider.clone(), params).await {
                    Ok(subscription) => return Ok(subscription),
                    Err(e) => {
                        warn!("Failed to subscribe with the current provider: {:?}. Switching to a different provider and retrying...", e);
                        self.move_to_unavailable_providers(provider).await;
                    }
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
        P: for<'b> Fn(&'b RootProvider<PubSubFrontend>) -> SubscriptionFuture<'b, R> + 'a,
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

    async fn move_to_unavailable_providers(&self, provider: Arc<WsOrIpc>) {
        let mut providers = self.providers.write().await;
        if let Some(pos) = providers.iter().position(|p| Arc::ptr_eq(p, &provider)) {
            providers.remove(pos);
        }
        let mut unavailable_providers = self.unavailable_providers.write().await;
        unavailable_providers.push_back(provider);
    }

    async fn ping_unavailable_providers(&self) {
        let mut unavailable_providers = self.unavailable_providers.write().await;
        let mut active_providers = vec![];

        while let Some(provider) = unavailable_providers.pop_front() {
            match self.ping(provider.clone()).await {
                Ok(_) => {
                    info!("Provider is back online: {:?}", provider);
                    active_providers.push(provider);
                }
                Err(error) => {
                    info!("ping error: {:?}", error.to_string());
                    unavailable_providers.push_back(provider);
                }
            }
        }

        drop(unavailable_providers);

        if !active_providers.is_empty() {
            let mut providers = self.providers.write().await;
            info!("Adding back {} active providers", active_providers.len());
            providers.extend(active_providers);
        }
    }

    async fn ping(&self, provider: Arc<WsOrIpc>) -> Result<(), TransportError> {
        match &*provider {
            WsOrIpc::Ws(ws_provider) => {
                ws_provider
                    .get_block_number()
                    .await
                    .map_err(|e| TransportError::from(e))?;
                Ok(())
            }
            WsOrIpc::Ipc(ipc_provider) => {
                ipc_provider
                    .get_block_number()
                    .await
                    .map_err(|e| TransportError::from(e))?;
                Ok(())
            }
        }
    }
}
