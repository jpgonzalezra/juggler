use alloy::primitives::B256;
use alloy::providers::Provider;
use alloy::providers::{ProviderBuilder, WsConnect};
use juggler::provider::{RpcBalancer, WsOrIpc};
use std::error::Error;
use std::sync::Arc;
use tracing::{error, info};
use tracing_subscriber::fmt::format::FmtSpan;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
        .init();

    let ws_provider1 = WsConnect::new("wss://eth-mainnet.g.alchemy.com/v2/<API-KEY>".to_string());
    let ws_provider2 = WsConnect::new("wss://eth-mainnet.g.alchemy.com/v2/<API-KEY>".to_string());

    let alchemy_ws_provider = Arc::new(WsOrIpc::Ws(
        ProviderBuilder::new().on_ws(ws_provider1).await.unwrap(),
    ));
    let infura_ws_provider = Arc::new(WsOrIpc::Ws(
        ProviderBuilder::new().on_ws(ws_provider2).await.unwrap(),
    ));

    let juggler = Arc::new(RpcBalancer::new(vec![
        alchemy_ws_provider.clone(),
        infura_ws_provider.clone(),
    ]));

    let handle_data = Box::new(|tx_hash: B256| {
        Box::pin(async move {
            println!("Transaction hash: {:?}", tx_hash);
        })
    });

    // Use the balancer to subscribe to pending transactions
    let subscribe_handle: tokio::task::JoinHandle<
        Result<(), Box<dyn std::error::Error + Send + Sync>>,
    > = tokio::spawn(async move {
        let result = juggler
            .subscribe(
                |provider| Box::pin(provider.subscribe_pending_transactions()),
                handle_data,
            )
            .await;

        result.map_err(|e| format!("Subscription error: {:?}", e).into())
    });

    match subscribe_handle.await {
        Ok(Ok(())) => info!("Subscription completed successfully."),
        Ok(Err(e)) => error!("Subscription failed: {:?}", e),
        Err(e) => error!("Tokio spawn error: {:?}", e),
    }

    Ok(())
}
