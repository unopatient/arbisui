// #![feature(async_fn_in_trait)]

use anyhow::Result;
use sui_sdk::rpc_types::EventFilter;
use custom_sui_sdk::SuiClient;
// use sui_sdk::wallet_context::WalletContext;
use futures::StreamExt;

pub mod markets;
pub mod market_graph;
pub mod cetus;
pub mod turbos;
pub mod constants;
pub mod sui_sdk_utils;
pub mod turbos_pool;
pub mod cetus_pool; 
pub use crate::markets::*;
pub use crate::market_graph::*;
pub use crate::cetus::*;
pub use crate::turbos::*;

pub struct RunData {
    pub sui_client: SuiClient,
    // pub wallet_context: WalletContext,
}

pub async fn loop_blocks(run_data: RunData, exchanges: Vec<&impl Exchange>) -> Result<()> {

    let exchange_package_event_filters = exchanges
        .iter()
        .map(|exchange| {
            Ok(
                EventFilter::Package(
                    *exchange.package_id()
                )
            )
        })
        .collect::<Result<Vec<EventFilter>>>()?;
    
    let mut subscribe_any_exchange_package_event = run_data
        .sui_client
        .event_api()
        .subscribe_event(
            EventFilter::Any(
                exchange_package_event_filters
            )
        )
        .await?;

    // Equivalent to .is_some() except we can print events
    while let Some(event) = subscribe_any_exchange_package_event.next().await {
        println!("New event: {:#?}", event);
    }
    
    Ok(())
}

// pub struct Config {
//     pub rpc: ,
//     pub ws: ,
// }

// pub async fn run() -> Result<()> {
//     let mut run_data = RunData
// }