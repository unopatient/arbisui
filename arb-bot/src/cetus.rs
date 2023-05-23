use std::str::FromStr;
use std::cmp;
use async_trait::async_trait;
use anyhow::{anyhow, Context};

use futures::TryStreamExt;
use page_turner::PageTurner;
use serde_json::Value;
use fixed::{types::{U64F64}, ParseFixedError};

use custom_sui_sdk::{
    SuiClient,
    apis::QueryEventsRequest
};

use sui_sdk::types::base_types::{ObjectID, ObjectType};
use sui_sdk::rpc_types::{SuiObjectDataOptions, SuiObjectResponseQuery, SuiObjectResponse, EventFilter, SuiEvent, SuiParsedData, SuiMoveStruct, SuiMoveValue, SuiParsedMoveObject};
use sui_sdk::types::dynamic_field::DynamicFieldInfo;
 
use move_core_types::language_storage::{StructTag, TypeTag};
use std::fmt::format;
use std::collections::BTreeMap;

use crate::markets::Exchange;

const EXCHANGE_ADDRESS: &str = "0x1eabed72c53feb3805120a081dc15963c204dc8d091542592abaf7a35689b2fb";
const GLOBAL: &str = "0xdaa46292632c3c4d8f31f23ea0f9b36a28ff3677e9684980e4438403a67a3d8f";
const POOLS: &str = "0xf699e7f2276f5c9a75944b37a0c5b5d9ddfd2471bf6242483b03ab2887d198d0";

pub struct Cetus;

#[async_trait]
impl Exchange for Cetus {
    fn package_id(&self) -> Result<ObjectID, anyhow::Error> {
        ObjectID::from_str(EXCHANGE_ADDRESS).map_err(|err| err.into())
    }

    // Cetus has us query for events
    async fn get_all_markets(&self, sui_client: &SuiClient) -> Result<(), anyhow::Error> {

        // TODO: Write page turner
        let pool_created_events = sui_client
            .event_api()
            .pages(
                QueryEventsRequest {
                    query: EventFilter::MoveEventType(
                        StructTag::from_str(
                            &format!("{}::factory::CreatePoolEvent", EXCHANGE_ADDRESS)
                        )?
                    ),
                    cursor: None,
                    limit: None,
                    descending_order: true,
                }
            )
            .items()
            .try_collect::<Vec<SuiEvent>>()
            .await?;

        // let markets = pool_created_events
        //     .into_iter()
        //     .map(|pool_created_event| {
        //         let parsed_json = &pool_created_event.parsed_json;
        //         if let (
        //             Value::String(coin_x_value), 
        //             Value::String(coin_y_value), 
        //             Value::String(pool_id_value)
        //         ) = 
        //             (
        //                 parsed_json.get("coin_type_a").context("Failed to get coin_type_a for a CetusMarket")?,
        //                 parsed_json.get("coin_type_b").context("Failed to get coin_type_b for a CetusMarket")?,
        //                 parsed_json.get("pool_id").context("Failed to get pool_id for a CetusMarket")?
        //             ) {
        //                 let coin_x = StructTag::from_str(&format!("0x{}", coin_x_value))?;
        //                 let coin_y = StructTag::from_str(&format!("0x{}", coin_y_value))?;
        //                 let pool_id = ObjectID::from_str(&format!("0x{}", pool_id_value))?;

        //                 Ok(
        //                     CetusMarket {
        //                         coin_x,
        //                         coin_y,
        //                         pool_id,
        //                     }
        //                 )
        //             } else {
        //                 Err(anyhow!("Failed to match pattern."))
        //             }
        //     })
        //     .collect::<Result<Vec<CetusMarket>, anyhow::Error>>()?;

        // let example_pool = sui_client
        //     .read_api()
        //     .get_object_with_options(
        //         markets[9].pool_id,
        //         SuiObjectDataOptions::full_content()
        //     )
        //     .await?;

        // let example_fields = get_fields_from_object_response(example_pool)?;
        // println!("current_sqrt_price: {:#?}", example_fields.get("current_sqrt_price").context("Could not get current_sqrt_price from fields")?);

        // let example_coin_x = sui_client
        //     .coin_read_api()
        //     .get_coin_metadata(
        //         format!("0x{}", markets[9].coin_x.to_canonical_string())
        //     )
        //     .await?;

        // println!("coin_x: {:#?}", example_coin_x);

        // let example_coin_y = sui_client
        //     .coin_read_api()
        //     .get_coin_metadata(
        //         format!("0x{}", markets[9].coin_y.to_canonical_string())
        //     )
        //     .await?;

        // println!("coin_y: {:#?}", example_coin_y);

        let mut markets = Vec::new();

        for pool_created_event in pool_created_events {
            let parsed_json = &pool_created_event.parsed_json;
            if let (
                Value::String(coin_x_value), 
                Value::String(coin_y_value), 
                Value::String(pool_id_value)
            ) = 
                (
                    parsed_json.get("coin_type_a").context("Failed to get coin_type_a for a CetusMarket")?,
                    parsed_json.get("coin_type_b").context("Failed to get coin_type_b for a CetusMarket")?,
                    parsed_json.get("pool_id").context("Failed to get pool_id for a CetusMarket")?
                ) {
                    let coin_x = StructTag::from_str(&format!("0x{}", coin_x_value))?;
                    let coin_y = StructTag::from_str(&format!("0x{}", coin_y_value))?;
                    let pool_id = ObjectID::from_str(&format!("0x{}", pool_id_value))?;
                    let coin_x_price = U64F64::from_bits(
                        u128::from_str(
                            if let SuiMoveValue::String(str_value) = 
                                get_fields_from_object_response(
                                    sui_client
                                        .read_api()
                                        .get_object_with_options(
                                            pool_id, 
                                            SuiObjectDataOptions::full_content()
                                        )
                                        .await?
                                )?
                                .get("current_sqrt_price")
                                .context("Missing field current_sqrt_price.")? {
                                    str_value
                                } else {
                                    return Err(anyhow!("current_sqrt_price field does not match SuiMoveValue::String value."));
                                }
                        )?
                    );

                    let (coin_y_price, overflowed) = U64F64::from_num(1).overflowing_div(coin_x_price);

                    markets.push(
                        CetusMarket {
                            coin_x,
                            coin_y,
                            pool_id,
                            coin_x_price,
                            coin_y_price,
                        }
                    );
                } else {
                    return Err(anyhow!("Failed to match pattern."));
                }
        }

        println!("{:#?}", markets);


        Ok(())
    }
}

#[derive(Debug)]
struct CetusMarket {
    coin_x: StructTag,
    coin_y: StructTag,
    pool_id: ObjectID,
    coin_x_price: U64F64, // In terms of y
    coin_y_price: U64F64, // In terms of x
}

// We'll need to deal with the math on this side
// Price is simple matter of ((current_sqrt_price / (2^64))^2) * (10^(a - b))
fn get_fields_from_object_response(response: SuiObjectResponse) -> Result<BTreeMap<String, SuiMoveValue>, anyhow::Error> {
    if let Some(object_data) = response.data {
        if let Some(parsed_data) = object_data.content {
            if let SuiParsedData::MoveObject(parsed_move_object) = parsed_data {
                if let SuiMoveStruct::WithFields(field_map) = parsed_move_object.fields {
                    // println!("{:#?}", field_map.get("current_sqrt_price").context("Could not get current_sqrt_price from fields")?);
                    Ok(field_map)
                } else {
                    Err(anyhow!("Does not match the SuiMoveStruct::WithFields variant"))
                }
            } else {
                Err(anyhow!("Does not match the SuiParsedData::MoveObject variant"))
            }
        } else {
            Err(anyhow!("Expected Some"))
        }
    } else {
        Err(anyhow!("Expected Some"))
    }
}