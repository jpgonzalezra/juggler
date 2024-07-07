use alloy::providers::Provider;
use alloy::providers::RootProvider;
use alloy::pubsub::PubSubFrontend;
use alloy::pubsub::Subscription;
use alloy::transports::{RpcError, TransportError, TransportErrorKind};
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
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
    pub providers: Arc<RwLock<HashMap<usize, Arc<WsOrIpc>>>>,
    pub current: Arc<Mutex<usize>>,
    pub unavailable_providers: Arc<RwLock<VecDeque<usize>>>,
    provider_id_counter: Arc<AtomicUsize>,
}

type SubscriptionFuture<'a, R> = Pin<
    Box<dyn Future<Output = Result<Subscription<R>, RpcError<TransportErrorKind>>> + Send + 'a>,
>;

type HandleDataFn<R, Fut> = dyn FnMut(R) -> Fut + Send;

impl RpcBalancer {
    pub fn new(providers: Vec<Arc<WsOrIpc>>) -> Self {
        let mut provider_map = HashMap::new();
        let mut provider_id_counter = 0;
        for provider in providers {
            provider_map.insert(provider_id_counter, provider);
            provider_id_counter += 1;
        }

        let balancer = RpcBalancer {
            providers: Arc::new(RwLock::new(provider_map)),
            current: Arc::new(Mutex::new(0)),
            unavailable_providers: Arc::new(RwLock::new(VecDeque::new())),
            provider_id_counter: Arc::new(AtomicUsize::new(provider_id_counter)),
        };
        info!("RpcBalancer initialized.");

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

    pub async fn add_provider(&self, provider: Arc<WsOrIpc>) {
        let mut providers = self.providers.write().await;
        let new_id = self.provider_id_counter.fetch_add(1, Ordering::SeqCst);
        providers.insert(new_id, provider);
        info!("Added new provider with ID {}", new_id);
    }

    pub async fn next_provider(&self) -> Option<(usize, Arc<WsOrIpc>)> {
        let providers = self.providers.read().await;
        if providers.is_empty() {
            warn!("No available providers.");
            None
        } else {
            let mut current = self.current.lock().await;
            let index = *current % providers.len();
            info!("Selecting provider at index {}", index);
            *current = *current + 1;
            let provider_id = *providers.keys().nth(index).unwrap();
            Some((provider_id, providers[&provider_id].clone()))
        }
    }

    pub async fn execute<'a, F, Fut, U>(&'a self, mut func: F) -> Result<U, TransportError>
    where
        F: FnMut(Arc<WsOrIpc>) -> Fut + Send,
        Fut: std::future::Future<Output = Result<U, TransportError>> + Send,
    {
        let providers = self.providers.read().await;
        let count = providers.len();
        if count == 0 {
            warn!("No available providers. Waiting for 1 second before retrying.");
            sleep(Duration::from_secs(1)).await; // TODO: param
        } else {
            for _ in 0..count {
                if let Some((provider_id, provider)) = self.next_provider().await {
                    match func(provider.clone()).await {
                        Ok(result) => {
                            info!(
                                "Request executed successfully with provider: {:?}",
                                provider
                            );
                            return Ok(result);
                        }
                        Err(e) => {
                            warn!(
                                "Provider {:?} failed: {:?}. Moving to unavailable providers.",
                                provider, e
                            );
                            self.move_to_unavailable_providers(provider_id).await;
                        }
                    }
                }
            }
        }
        let msg = "All providers failed.";
        error!(msg);
        Err(TransportError::local_usage_str(msg))
    }

    pub async fn subscribe<'a, P, R, Fut>(
        &'a self,
        params: P,
        mut handle_data: Box<HandleDataFn<R, Fut>>,
    ) -> Result<(), TransportError>
    where
        P: for<'b> Fn(&'b RootProvider<PubSubFrontend>) -> SubscriptionFuture<'b, R> + 'a,
        R: 'static + DeserializeOwned + std::fmt::Debug,
        Fut: std::future::Future<Output = ()> + 'static,
    {
        loop {
            match self.get_subscription(&params).await {
                Ok(subscription) => {
                    info!("Subscription obtained successfully.");
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
            sleep(Duration::from_secs(1)).await; // TODO: param
        }
        for _ in 0..count {
            if let Some((provider_id, provider)) = self.next_provider().await {
                match self.try_subscribe(provider.clone(), params).await {
                    Ok(subscription) => {
                        info!("Successfully subscribed with provider: {:?}", provider);
                        return Ok(subscription);
                    }
                    Err(e) => {
                        warn!("Failed to subscribe with the current provider: {:?}. Switching to a different provider and retrying...", e);
                        self.move_to_unavailable_providers(provider_id).await;
                    }
                }
            }
        }
        let msg = "All providers failed for subscription.";
        error!(msg);
        Err(TransportError::local_usage_str(msg))
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
                info!(
                    "Trying to subscribe with WebSocket provider: {:?}",
                    provider
                );
                let subscription = params(ws_provider).await?;
                Ok(subscription)
            }
            WsOrIpc::Ipc(ipc_provider) => {
                info!("Trying to subscribe with IPC provider: {:?}", provider);
                let subscription = params(ipc_provider).await?;
                Ok(subscription)
            }
        }
    }

    async fn move_to_unavailable_providers(&self, provider_id: usize) {
        {
            let mut providers = self.providers.write().await;
            providers.remove(&provider_id);
        }
        {
            let mut unavailable_providers = self.unavailable_providers.write().await;
            unavailable_providers.push_back(provider_id);
        }
    }

    async fn ping_unavailable_providers(&self) {
        let mut unavailable_providers = self.unavailable_providers.write().await;
        let mut providers = self.providers.write().await;
        let mut still_unavailable = VecDeque::new();
        while let Some(provider_id) = unavailable_providers.pop_front() {
            if let Some(provider) = providers.get(&provider_id).cloned() {
                match self.ping(provider.clone()).await {
                    Ok(_) => {
                        info!("Provider is back online: {:?}", provider);
                        providers.insert(provider_id, provider.clone());
                    }
                    Err(error) => {
                        info!("ping error: {:?}", error.to_string());
                        still_unavailable.push_back(provider_id);
                    }
                }
            }
        }

        *unavailable_providers = still_unavailable;
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
