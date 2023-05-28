use std::str::FromStr;
use async_trait::async_trait;
use anyhow::{anyhow, Context};

use futures::{future, TryStreamExt};
use page_turner::PageTurner;
use serde_json::Value;
use fixed::types::U64F64;

use custom_sui_sdk::{
    SuiClient,
    apis::QueryEventsRequest
};

use sui_sdk::types::base_types::{ObjectID, ObjectIDParseError};
use sui_sdk::rpc_types::{SuiObjectDataOptions, SuiObjectResponse, EventFilter, SuiEvent, SuiParsedData, SuiMoveStruct, SuiMoveValue};
 
use move_core_types::language_storage::{StructTag, TypeTag};
use std::collections::{BTreeMap, HashMap};

use crate::markets::{Exchange, Market};
use crate::sui_sdk_utils;

pub struct Turbos {
    package_id: ObjectID
}

impl Turbos {
    pub fn new(package_id: ObjectID) -> Self {
        Turbos {
            package_id,
        }
    }
}

impl FromStr for Turbos {
    type Err = anyhow::Error;

    fn from_str(package_id_str: &str) -> Result<Self, anyhow::Error> {
        Ok(
            Turbos {
                package_id: ObjectID::from_str(package_id_str).map_err(<ObjectIDParseError as Into<anyhow::Error>>::into)?,
            }
        )
    }
}

#[async_trait]
impl Exchange for Turbos {
    fn package_id(&self) -> &ObjectID {
        &self.package_id
    }

    async fn get_all_markets(&self, sui_client: &SuiClient) -> Result<Vec<Box<dyn Market>>, anyhow::Error> {
        let pool_created_events = sui_client
            .event_api()
            .pages(
                QueryEventsRequest {
                    query: EventFilter::MoveEventType(
                        StructTag::from_str(
                            &format!("{}::pool_factory::PoolCreatedEvent", self.package_id)
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

        let pool_ids = pool_created_events
            .into_iter()
            .map(|pool_created_event| {
                let parsed_json = pool_created_event.parsed_json;
                if let Value::String(pool_id_value) = parsed_json.get("pool").context("Failed to get pool_id for a CetusMarket")? {
                    Ok(ObjectID::from_str(&format!("0x{}", pool_id_value))?)
                } else {
                    Err(anyhow!("Failed to match pattern."))
                }
            })
            .collect::<Result<Vec<ObjectID>, anyhow::Error>>()?;

        let pool_id_to_object_response = sui_sdk_utils::get_pool_ids_to_object_response(sui_client, &pool_ids).await?;

        let markets = pool_id_to_object_response
            .into_iter()
            .map(|(pool_id, object_response)| {

                let fields = sui_sdk_utils::get_fields_from_object_response(&object_response)?;
                let (coin_x, coin_y) = sui_sdk_utils::get_coin_pair_from_object_response(&object_response)?;

                let coin_x_price = U64F64::from_bits(
                    u128::from_str(
                        if let SuiMoveValue::String(str_value) = fields
                            .get("sqrt_price")
                            .context("Missing field sqrt_price.")? {
                                &str_value
                            } else {
                                return Err(anyhow!("sqrt_price field does not match SuiMoveValue::String value."));
                            }
                    )?
                );
        
                let coin_y_price = U64F64::from_num(1) / coin_x_price;

                Ok(
                    Box::new(
                        TurbosMarket {
                            coin_x,
                            coin_y,
                            pool_id,
                            coin_x_price: Some(coin_x_price),
                            coin_y_price: Some(coin_y_price),
                        }
                    ) as Box<dyn Market>
                )
            })
            .collect::<Result<Vec<Box<dyn Market>>, anyhow::Error>>()?;

        Ok(vec![])
    }

    // async fn get_pool_id_to_fields(&self, sui_client: &SuiClient, markets: &[Box<dyn Market>]) -> Result<HashMap<ObjectID, BTreeMap<String, SuiMoveValue>>, anyhow::Error> {
    //     let pool_ids = markets
    //         .iter()
    //         .map(|market| {
    //             *market.pool_id()
    //         })
    //         .collect::<Vec<ObjectID>>();

    //     sui_sdk_utils::get_pool_id_to_fields(sui_client, &pool_ids).await
    // }

    async fn get_pool_id_to_object_response(&self, sui_client: &SuiClient, markets: &[Box<dyn Market>]) -> Result<HashMap<ObjectID, SuiObjectResponse>, anyhow::Error> {
        let pool_ids = markets
            .iter()
            .map(|market| {
                *market.pool_id()
            })
            .collect::<Vec<ObjectID>>();

        sui_sdk_utils::get_pool_ids_to_object_response(sui_client, &pool_ids).await
    }
}
#[derive(Debug, Clone)]
struct TurbosMarket {
    coin_x: TypeTag,
    coin_y: TypeTag,
    pool_id: ObjectID,
    coin_x_price: Option<U64F64>, // In terms of y. x / y
    coin_y_price: Option<U64F64>, // In terms of x. y / x
}

impl Market for TurbosMarket {
    fn coin_x(&self) -> &TypeTag {
        &self.coin_x
    }

    fn coin_y(&self) -> &TypeTag {
        &self.coin_y
    }

    fn coin_x_price(&self) -> Option<U64F64> {
        self.coin_x_price
    }

    fn coin_y_price(&self) -> Option<U64F64> {
        self.coin_y_price
    }

    fn update_with_fields(&mut self, fields: &BTreeMap<String, SuiMoveValue>) -> Result<(), anyhow::Error> {
        let coin_x_price = U64F64::from_bits(
            u128::from_str(
                if let SuiMoveValue::String(str_value) = fields
                    .get("sqrt_price")
                    .context("Missing field sqrt_price.")? {
                        str_value
                    } else {
                        return Err(anyhow!("sqrt_price field does not match SuiMoveValue::String value."));
                    }
            )?
        );

        let coin_y_price = U64F64::from_num(1) / coin_x_price;
        
        self.coin_x_price = Some(coin_x_price);
        self.coin_y_price = Some(coin_y_price);

        // println!("coin_x<{}>: {}", self.coin_x, self.coin_x_price.unwrap());
        // println!("coin_y<{}>: {}\n", self.coin_y, self.coin_y_price.unwrap());

        Ok(())
    }

    fn pool_id(&self) -> &ObjectID {
        &self.pool_id
    }
}
