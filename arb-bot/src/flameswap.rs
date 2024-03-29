use std::str::FromStr;
use std::cmp;
use async_trait::async_trait;
use anyhow::{anyhow, Context};

use futures::TryStreamExt;
use page_turner::PageTurner;
use custom_sui_sdk::{
    SuiClient,
    apis::GetDynamicFieldsRequest
};

use sui_sdk::types::base_types::{ObjectID, ObjectType};
use sui_sdk::rpc_types::{SuiObjectDataOptions};
use sui_sdk::types::dynamic_field::DynamicFieldInfo;
// use sui_sdk::error::{Error, SuiRpcResult};

use crate::markets::Exchange;


use move_core_types::language_storage::{TypeTag};

const EXCHANGE_ADDRESS: &str = "0x6b84da4f5dc051759382e60352377fea9d59bc6ec92dc60e0b6387e05274415f";
const GLOBAL: &str = "0x3083e3d751360c9084ba33f6d9e1ad38fb2a11cffc151f2ee4a5c03da61fb1e2";
const POOLS: &str = "0x6edec171d3b4c6669ac748f6de77f78635b72aac071732b184677db19eefd9e8";

pub struct FlameSwap;

#[async_trait]
impl Exchange for FlameSwap {
    fn package_id(&self) -> Result<ObjectID, anyhow::Error> {
        ObjectID::from_str(EXCHANGE_ADDRESS).map_err(|err| err.into())
    }

    async fn get_all_markets(&self, sui_client: &SuiClient) -> Result<(), anyhow::Error> {

        let pools_dynamic_fields_data = sui_client
            .read_api()
            .pages(
                GetDynamicFieldsRequest {
                    object_id: ObjectID::from_str(POOLS)?,
                    cursor: None,
                    limit: None,
                }
            )
            .items()
            .try_collect::<Vec<DynamicFieldInfo>>()
            .await?;

        let pool_object_ids = pools_dynamic_fields_data
            .iter()
            .map(|field| {
                field.object_id
            })
            .collect::<Vec<ObjectID>>();

        println!(
            "Number of pools: {:#?}", 
            pool_object_ids.len()
        );

        // Considering the request limit might make sense to do a page at a time hahahha
        let mut pools = Vec::new();

        for i in 0..(pool_object_ids.len() / 50) + 1 {

            // println!("i*50: {}, pool_object_ids length:{}", i*50, pool_object_ids.len());
            pools.extend(
                sui_client
                    .read_api()
                    .multi_get_object_with_options(
                        pool_object_ids[i*50..cmp::min(i*50 + 50, pool_object_ids.len())].to_vec(),
                        SuiObjectDataOptions::full_content()
                    )
                    .await?
            )
        }

        // TODO: Refactor
        let coin_pairs = pools
            .into_iter()
            .map(|pool| {
                if let Some(data) = pool.data {
                    if let Some(type_) = data.type_ {
                        if let ObjectType::Struct(move_object_type) = type_ {
                                if let TypeTag::Struct(box_struct_tag) = move_object_type
                                .type_params()
                                .get(1).context("Missing coin pair type parameter")? 
                            {
                                println!("{:#?}", box_struct_tag);
                                Ok(
                                    FlameswapMarket{
                                        coin_x: box_struct_tag.type_params.get(0).context("Missing coin_x")?.clone(),
                                        coin_y: box_struct_tag.type_params.get(0).context("Missing coin_y")?.clone(),
                                    }
                                )
                            } else {
                                Err(anyhow!("Does not match the TypeTag::Struct variant"))
                            }
                        } else {
                            Err(anyhow!("Does not match the ObjectType::Struct variant"))
                        }
                    } else {
                        Err(anyhow!("Expected Some"))
                    }
                } else {
                    Err(anyhow!("Expected Some"))
                }
            })
            .collect::<Result<Vec<FlameswapMarket>, anyhow::Error>>()?;

        println!("Number of markets: {}", coin_pairs.len());
        // coin_pairs.iter().for_each(|market| println!("{:#?}", market));

        
        Ok(())
    }
}

#[derive(Debug)]
struct FlameswapMarket {
    coin_x: TypeTag,
    coin_y: TypeTag,
}
