use std::str::FromStr;
use async_trait::async_trait;
use anyhow::{anyhow, Context};

use futures::{future, TryStreamExt};
use page_turner::PageTurner;
use serde_json::Value;
use fixed::types::U64F64;

use custom_sui_sdk::{
    SuiClient,
    apis::{
        QueryEventsRequest,
        GetDynamicFieldsRequest
    },
    transaction_builder::{TransactionBuilder, ProgrammableObjectArg},
    programmable_transaction_sui_json::ProgrammableTransactionArg
};

use sui_sdk::json::SuiJsonValue;

use sui_sdk::types::base_types::{ObjectID, ObjectIDParseError, SuiAddress};
use sui_sdk::types::dynamic_field::DynamicFieldInfo;
use sui_sdk::rpc_types::{EventFilter, SuiEvent, SuiMoveValue, SuiObjectDataOptions, SuiMoveStruct, SuiObjectResponse, SuiTypeTag};
 
// use sui_sdk::json_types::SuiTypeTag;
use sui_sdk::types::programmable_transaction_builder::ProgrammableTransactionBuilder;

use move_core_types::language_storage::{StructTag, TypeTag};
use move_core_types::value::MoveValue;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::format;

use crate::markets::{Exchange, Market};
use crate::sui_sdk_utils::{self, sui_move_value};
// use crate::{cetus_pool, cetus};
use crate::fast_v3_pool;
use crate::sui_json_utils::move_value_to_json;

// const GLOBAL: &str = "0xdaa46292632c3c4d8f31f23ea0f9b36a28ff3677e9684980e4438403a67a3d8f";
// const POOLS: &str = "0xf699e7f2276f5c9a75944b37a0c5b5d9ddfd2471bf6242483b03ab2887d198d0";
const CLOCK_OBJECT_ID: &str = "0x0000000000000000000000000000000000000000000000000000000000000006";

#[derive(Debug, Clone)]
pub struct Cetus {
    package_id: ObjectID,
    periphery_id: ObjectID,
    global_config_id: ObjectID,
    event_struct_tag_to_pool_field: HashMap<StructTag, String>
}

impl Cetus {
    pub fn new(package_id: ObjectID, periphery_id: ObjectID, global_config_id: ObjectID) -> Self {
        let mut event_struct_tag_to_pool_field = HashMap::new();
        event_struct_tag_to_pool_field.insert(
            StructTag::from_str(
                &format!("{}::pool::SwapEvent", package_id)
            ).expect("Cetus: failed to create event struct tag"),
            "pool".to_string()
        );
        
        Cetus {
            package_id,
            periphery_id,
            global_config_id,
            event_struct_tag_to_pool_field
        }
    }
}

impl Cetus {
    fn package_id(&self) -> &ObjectID {
        &self.package_id
    }

    fn event_package_id(&self) -> &ObjectID {
        &self.package_id
    }

    fn periphery_id(&self) -> &ObjectID {
        &self.periphery_id
    }

    fn global_config_id(&self) -> &ObjectID {
        &self.global_config_id
    }

    fn event_filters(&self) -> Vec<EventFilter> {
        self
            .event_struct_tag_to_pool_field()
            .keys()
            .cloned()
            .map(|event_struct_tag| {
                EventFilter::MoveEventType(
                    event_struct_tag
                )
            })
            .collect::<Vec<_>>()
    }

    fn event_struct_tag_to_pool_field(&self) -> &HashMap<StructTag, String> {
        &self.event_struct_tag_to_pool_field
    }

    // Cetus has us query for events
    async fn get_all_markets_(&self, sui_client: &SuiClient) -> Result<Vec<Box<dyn Market>>, anyhow::Error> {

        let pool_created_events = sui_client
            .event_api()
            .pages(
                QueryEventsRequest {
                    query: EventFilter::MoveEventType(
                        StructTag::from_str(
                            &format!("{}::factory::CreatePoolEvent", self.package_id)
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

        // let mut markets: Vec<Box<dyn Market>> = Vec::new();

        let markets = pool_created_events
            .iter()
            .map(|pool_created_event| {
                let parsed_json = &pool_created_event.parsed_json;
                if let (
                    Value::String(coin_x_value), 
                    Value::String(coin_y_value), 
                    Value::String(pool_id_value)
                ) = 
                    (
                        parsed_json.get("coin_type_a").context("Failed to get coin_type_a for a CetusMarket")?,
                        parsed_json.get("coin_type_b").context("Failed to get coin_type_b for a CetusMarket")?,
                        parsed_json.get("pool_id").context(format!("Failed to get pool_id for a CetusMarket: {:#?}", parsed_json))?
                    ) {
                        let coin_x = TypeTag::from_str(&format!("0x{}", coin_x_value))?;
                        let coin_y = TypeTag::from_str(&format!("0x{}", coin_y_value))?;
                        let pool_id = ObjectID::from_str(&format!("0x{}", pool_id_value))?;

                        Ok(
                            Box::new(
                                CetusMarket {
                                    parent_exchange: self.clone(),
                                    coin_x,
                                    coin_y,
                                    pool_id,
                                    coin_x_sqrt_price: None,
                                    coin_y_sqrt_price: None,
                                    computing_pool: None
                                }
                            ) as Box<dyn Market>
                        )
                    } else {
                        Err(anyhow!("Failed to match pattern."))
                    }
            })
            .collect::<Result<Vec<Box<dyn Market>> ,anyhow::Error>>()?;

        Ok(markets)
    }

    async fn get_pool_id_to_object_response(&self, sui_client: &SuiClient, markets: &[Box<dyn Market>]) -> Result<HashMap<ObjectID, SuiObjectResponse>, anyhow::Error> {
        let pool_ids = markets
            .iter()
            .map(|market| {
                *market.pool_id()
            })
            .collect::<Vec<ObjectID>>();

        sui_sdk_utils::get_object_id_to_object_response(sui_client, &pool_ids).await
    }

    pub async fn computing_pool_from_object_response(&self, sui_client: &SuiClient, response: &SuiObjectResponse) -> Result<fast_v3_pool::Pool, anyhow::Error> {
        // println!("{:#?}", response);

        let id = response.data.as_ref().context("data field from object response is None")?.object_id;
        
        let fields = sui_sdk_utils::read_fields_from_object_response(response).context("missing fields")?;

        // println!("cetus pool fields: {:#?}", fields);

        let tick_spacing = sui_move_value::get_number(&fields, "tick_spacing")?;

        let fee_rate = u64::from_str(
            &sui_move_value::get_string(&fields, "fee_rate")?
        )?;

        let liquidity = u128::from_str(
            &sui_move_value::get_string(&fields, "liquidity")?
        )?;

        let current_sqrt_price = u128::from_str(
            &sui_move_value::get_string(&fields, "current_sqrt_price")?
        )?;

        let current_tick_index = sui_move_value::get_number(
            &sui_move_value::get_struct(&fields, "current_tick_index")?,
            "bits"
        )? as i32;


        let tick_manager_struct = sui_move_value::get_struct(&fields, "tick_manager")?;

        let tick_manager_tick_spacing = sui_move_value::get_number(&tick_manager_struct, "tick_spacing")?;

        let tick_manager_ticks_skip_list_struct = sui_move_value::get_struct(&tick_manager_struct, "ticks")?;

        let tick_manager_ticks_skip_list_id = sui_move_value::get_uid(
            &tick_manager_ticks_skip_list_struct,
            "id"
        )?;

        let ticks = self.get_ticks(sui_client, &tick_manager_ticks_skip_list_id).await?;

        let is_pause = sui_move_value::get_bool(&fields, "is_pause")?;

        Ok(
            fast_v3_pool::Pool {
                id,
                tick_spacing,
                fee: fee_rate,
                liquidity,
                sqrt_price: current_sqrt_price,
                tick_current_index: current_tick_index,
                ticks,
                unlocked: !is_pause
                // fee_growth_global_a,
                // fee_growth_global_b,
                // fee_protocol_coin_a,
                // fee_protocol_coin_b,
                // tick_manager: cetus_pool::tick::TickManager {
                //     tick_spacing: tick_manager_tick_spacing,
                //     ticks
                // },
                // is_pause,
            }
        )
    }

    async fn get_ticks(
        &self,
        sui_client: &SuiClient, 
        ticks_skip_list_id: &ObjectID
    ) -> Result<BTreeMap<i32, fast_v3_pool::Tick>, anyhow::Error> {

        // let aa = sui_client
        //     .read_api()
        //     .get_object_with_options(
        //         ticks_skip_list_id.clone(),
        //         SuiObjectDataOptions::new().with_type()
        //     )
        //     .await?;

        // println!("ticks skip_list: {:#?}", aa);

        let skip_list_dynamic_field_infos = sui_client
            .read_api()
            .pages(
                GetDynamicFieldsRequest {
                    object_id: ticks_skip_list_id.clone(), // We can make this consuming if it saves time
                    cursor: None,
                    limit: None,
                }
            )
            .items()
            .try_collect::<Vec<DynamicFieldInfo>>()
            .await?;

        // println!("skip_list_dynamic_field_infos len: {}", skip_list_dynamic_field_infos.len());
        // println!("skip_list_dynamic_field_infos: {:#?}", skip_list_dynamic_field_infos);

        let node_object_type = format!("0xbe21a06129308e0495431d12286127897aff07a8ade3970495a4404d97f9eaaa::skip_list::Node<{}::tick::Tick>", self.package_id);

        let node_object_ids = skip_list_dynamic_field_infos
            .into_iter()
            .filter(|dynamic_field_info| {
                node_object_type == dynamic_field_info.object_type
            })
            .map(|tick_dynamic_field_info| {
                tick_dynamic_field_info.object_id
            })
            .collect::<Vec<ObjectID>>();

        let node_object_responses = sui_sdk_utils::get_object_responses(sui_client, &node_object_ids).await?;

        // println!("{:#?}", node_object_responses);

        let tick_index_to_tick = node_object_responses
            .into_iter()
            .map(|node_object_response| {
                let fields = sui_sdk_utils::read_fields_from_object_response(&node_object_response).context("Missing fields.")?;

                let node_fields = sui_move_value::get_struct(&fields, "value").context("cetus")?;

                let tick_fields = sui_move_value::get_struct(&node_fields, "value").context("cetus")?;

                // println!("tick_fields: {:#?}", tick_fields);

                // println!("1");

                let index = sui_move_value::get_number(
                    &sui_move_value::get_struct(
                        &tick_fields, 
                        "index"
                    )?, 
                    "bits"
                )? as i32;

                // println!("2");

                let sqrt_price = u128::from_str(
                    &sui_move_value::get_string(&tick_fields,"sqrt_price")?
                )?;

                // println!("3");

                let liquidity_net = u128::from_str(
                    &sui_move_value::get_string(
                        &sui_move_value::get_struct(
                            &tick_fields, 
                            "liquidity_net"
                        )?, 
                        "bits"
                    )?
                )? as i128;

                // println!("4");

                let liquidity_gross = u128::from_str(
                    &sui_move_value::get_string(&tick_fields, "liquidity_gross")?
                )?;

                // // println!("5");

                // let fee_growth_outside_a = u128::from_str(
                //     &sui_move_value::get_string(&tick_fields,"fee_growth_outside_a")?
                // )?;

                // // println!("6");

                // let fee_growth_outside_b = u128::from_str(
                //     &sui_move_value::get_string(&tick_fields,"fee_growth_outside_b")?
                // )?;

                // println!("7");

                // println!("tick_fields: {:#?}", tick_fields);

                let tick = fast_v3_pool::Tick{
                    index,
                    sqrt_price,
                    liquidity_net,
                    liquidity_gross,
                    // fee_growth_outside_a,
                    // fee_growth_outside_b,
                };

                Ok((index, tick))
            })
            .collect::<Result<BTreeMap<i32, fast_v3_pool::Tick>, anyhow::Error>>()?;

        Ok(tick_index_to_tick)

    }

}

#[async_trait]
impl Exchange for Cetus {
    fn package_id(&self) -> &ObjectID {
        self.package_id()
    }

    fn event_package_id(&self) -> &ObjectID {
        &self.event_package_id()
    }


    fn event_filters(&self) -> Vec<EventFilter> {
        self.event_filters()
    }

    fn event_struct_tag_to_pool_field(&self) -> &HashMap<StructTag, String> {
        self.event_struct_tag_to_pool_field()
    }

    // Cetus has us query for events
    async fn get_all_markets(&mut self, sui_client: &SuiClient) -> Result<Vec<Box<dyn Market>>, anyhow::Error> {
        self.get_all_markets_(sui_client).await
    }

    async fn get_pool_id_to_object_response(&self, sui_client: &SuiClient, markets: &[Box<dyn Market>]) -> Result<HashMap<ObjectID, SuiObjectResponse>, anyhow::Error> {
        self.get_pool_id_to_object_response(sui_client, markets).await
    }

}

#[derive(Debug, Clone)]
pub struct CetusMarket {
    pub parent_exchange: Cetus,
    pub coin_x: TypeTag,
    pub coin_y: TypeTag,
    pub pool_id: ObjectID,
    pub coin_x_sqrt_price: Option<U64F64>, // In terms of y. x / y
    pub coin_y_sqrt_price: Option<U64F64>, // In terms of x. y / x
    pub computing_pool: Option<fast_v3_pool::Pool>
}

impl CetusMarket {
    fn coin_x(&self) -> &TypeTag {
        &self.coin_x
    }

    fn coin_y(&self) -> &TypeTag {
        &self.coin_y
    }

    fn coin_x_price(&self) -> Option<U64F64> {
        if let Some(coin_x_sqrt_price) = self.coin_x_sqrt_price {
            Some(coin_x_sqrt_price * coin_x_sqrt_price)
        } else {
            self.coin_x_sqrt_price
        }
    }

    fn coin_y_price(&self) -> Option<U64F64> {
        if let Some(coin_y_sqrt_price) = self.coin_y_sqrt_price {
            Some(coin_y_sqrt_price * coin_y_sqrt_price)
        } else {
            self.coin_y_sqrt_price
        }
    }

    async fn update_with_object_response(&mut self, sui_client: &SuiClient, object_response: &SuiObjectResponse) -> Result<(), anyhow::Error> {
        let fields = sui_sdk_utils::read_fields_from_object_response(object_response).context("Missing fields for object_response.")?;
        // println!("cetus fields: {:#?}", fields);
        let coin_x_sqrt_price = U64F64::from_bits(
            u128::from_str(
                &sui_move_value::get_string(&fields, "current_sqrt_price")?
            )?
        );

        let coin_y_sqrt_price = U64F64::from_num(1) / coin_x_sqrt_price;
        
        self.coin_x_sqrt_price = Some(coin_x_sqrt_price);
        self.coin_y_sqrt_price = Some(coin_y_sqrt_price);

        // println!("coin_x<{}>: {}", self.coin_x, self.coin_x_price.unwrap());
        // println!("coin_y<{}>: {}\n", self.coin_y, self.coin_y_price.unwrap());

        self.computing_pool = Some(self.parent_exchange.computing_pool_from_object_response(sui_client, object_response).await?);

        // println!("finised updating cetus pool");

        Ok(())
    }

    fn update_with_event(&mut self, event: &SuiEvent) -> Result<(), anyhow::Error> {
        let type_ = &event.type_;
        let event_parsed_json = &event.parsed_json;
        let computing_pool = self
            .computing_pool
            .as_mut()
            .context("computing_pool is None")?;

        // Amortize this so we only allocate these once. Cant be computed at compile time.
        let swap_event_type = StructTag::from_str(
                &format!("{}::pool::SwapEvent", &self.parent_exchange.package_id)
            ).context("Cetus: failed to create event struct tag")?;

        let add_liq_event_type = StructTag::from_str(
                &format!("{}::pool::AddLiquidityEvent", &self.parent_exchange.package_id)
            ).context("Cetus: failed to create event struct tag")?;

        let remove_liq_event_type = StructTag::from_str(
                &format!("{}::pool::RemoveLiquidityEvent", &self.parent_exchange.package_id)
            ).context("Cetus: failed to create event struct tag")?;

        let update_fee_rate_event_type = StructTag::from_str(
                &format!("{}::pool::UpdateFeeRateEvent", &self.parent_exchange.package_id)
            ).context("Cetus: failed to create event struct tag")?;

        match type_ {
            swap_event_type => {
                let amount_in = u64::from_str(
                    if let serde_json::Value::String(str) = event_parsed_json.get("amount_in").context("")? {
                        str
                    } else {
                        return Err(anyhow!("SwapEvent amount_in is not Value::String."))
                    }
                )?;
                let amount_out = u64::from_str(
                    if let serde_json::Value::String(str) = event_parsed_json.get("amount_out").context("")? {
                        str
                    } else {
                        return Err(anyhow!("SwapEvent amount_out is not Value::String."))
                    }
                )?;
                let fee_amount = u64::from_str(
                    if let serde_json::Value::String(str) = event_parsed_json.get("fee_amount").context("")? {
                        str
                    } else {
                        return Err(anyhow!("SwapEvent fee_amount is not Value::String."))
                    }
                )?;
                let atob = *if let serde_json::Value::Bool(bool_inner) = event_parsed_json.get("atob").context("")? {
                    bool_inner
                } else {
                    return Err(anyhow!("SwapEvent atob is not Value::Bool."))
                };

                let amount_specified = amount_in + fee_amount;

                let sqrt_price_limit = if atob {
                    fast_v3_pool::tick_math::MIN_SQRT_PRICE_X64 + 1
                } else {
                    fast_v3_pool::tick_math::MAX_SQRT_PRICE_X64 - 1
                };

                computing_pool.apply_swap(
                    atob,
                    amount_specified, 
                    true, 
                    sqrt_price_limit
                );

            },
            add_liq_event_type => {
                let tick_lower = u32::from_str(
                    if let serde_json::Value::String(str) = event_parsed_json.get("tick_lower").context("")? {
                        str
                    } else {
                        return Err(anyhow!("SwapEvent tick_lower is not Value::String."))
                    }
                )? as i32;
                let tick_upper = u32::from_str(
                    if let serde_json::Value::String(str) = event_parsed_json.get("tick_upper").context("")? {
                        str
                    } else {
                        return Err(anyhow!("SwapEvent tick_upper_index is not Value::String."))
                    }
                )? as i32;
                let liquidity_delta = u128::from_str(
                    if let serde_json::Value::String(str) = event_parsed_json.get("liquidity").context("")? {
                        str
                    } else {
                        return Err(anyhow!("SwapEvent liquidity is not Value::String."))
                    }
                )?;

                computing_pool.apply_add_liquidity(
                    tick_lower, 
                    tick_upper, 
                    liquidity_delta
                );
            },
            remove_liq_event_type => {
                let tick_lower_index = u32::from_str(
                    if let serde_json::Value::String(str) = event_parsed_json.get("tick_lower").context("")? {
                        str
                    } else {
                        return Err(anyhow!("SwapEvent tick_lower is not Value::String."))
                    }
                )? as i32;
                let tick_upper_index = u32::from_str(
                    if let serde_json::Value::String(str) = event_parsed_json.get("tick_upper").context("")? {
                        str
                    } else {
                        return Err(anyhow!("SwapEvent tick_upper is not Value::String."))
                    }
                )? as i32;
                let liquidity_delta = u128::from_str(
                    if let serde_json::Value::String(str) = event_parsed_json.get("liquidity").context("")? {
                        str
                    } else {
                        return Err(anyhow!("SwapEvent liquidity is not Value::String."))
                    }
                )?;

                computing_pool.apply_remove_liquidity(
                    tick_lower_index, 
                    tick_upper_index, 
                    liquidity_delta
                );
            },
            update_fee_rate_event_type => {
                let new_fee_rate = u64::from_str(
                    if let serde_json::Value::String(str) = event_parsed_json.get("new_fee_rate").context("")? {
                        str
                    } else {
                        return Err(anyhow!("SwapEvent new_fee_rate is not Value::String."))
                    }
                )?;

                computing_pool.apply_update_fee(new_fee_rate);
            },
            _ => {
                // do nothing
            }
        }

        Ok(())

    }

    fn pool_id(&self) -> &ObjectID {
        &self.pool_id
    }

    fn package_id(&self) -> &ObjectID {
        &self.parent_exchange.package_id
    }

    fn periphery_id(&self) -> &ObjectID {
        &self.parent_exchange.periphery_id
    }

    // Better handling of computing pool being None
    fn compute_swap_x_to_y(&self, amount_specified: u128) -> (u128, u128) {
        let swap_state = self.computing_pool.as_ref().unwrap().compute_swap_result(
            true, 
            amount_specified as u64, 
            true, 
            fast_v3_pool::tick_math::MIN_SQRT_PRICE_X64 + 1,
        );

        (swap_state.amount_a as u128, swap_state.amount_b as u128)
    }

    fn compute_swap_y_to_x(&self, amount_specified: u128) -> (u128, u128) {
        let swap_state = self.computing_pool.as_ref().unwrap().compute_swap_result(
            false, 
            amount_specified as u64, 
            true, 
            fast_v3_pool::tick_math::MAX_SQRT_PRICE_X64 - 1,
        );

        (swap_state.amount_a as u128, swap_state.amount_b as u128)
    }

    fn viable(&self) -> bool {
        if let Some(cp) = &self.computing_pool {
            // println!("liquidity: {}", cp.liquidity);
            if cp.liquidity > 0 && cp.unlocked && cp.liquidity_sanity_check() {
                true
            } else {
                false
            }
        } else {
            false
        }
    }

    async fn add_swap_to_programmable_trasaction(
        &self,
        transaction_builder: &TransactionBuilder,
        pt_builder: &mut ProgrammableTransactionBuilder,
        orig_coins: Option<Vec<ObjectID>>, // the actual coin object in (that you own and has money)
        // orig_coin_is_gas_coin: bool,
        x_to_y: bool,
        amount_in: u128,
        amount_out: u128,
    ) -> Result<(), anyhow::Error> {
        // Very rough but lets do thisss
        // We can't add to a result unless theres a function that exists..

        // println!("x_to_y: {}", x_to_y);

        // Arg0: &GlobalConfig
        let global_config = ProgrammableTransactionArg::SuiJsonValue(
            SuiJsonValue::from_object_id(self.parent_exchange.global_config_id.clone())
        );

        // swap_a_b and swap_b_c arguments
        // Arg1: &mut Pool<Ty0, Ty1, Ty2>
        let pool = ProgrammableTransactionArg::SuiJsonValue(
            SuiJsonValue::from_object_id(self.pool_id.clone())
        );

        // Arg2: vector<Coin<Ty0 or Ty1>>

        let orig_coins_args_vec = if let Some(oc) = orig_coins {
            oc
                .into_iter()
                .map(|orig_coin| {
                    ProgrammableObjectArg::ObjectID(orig_coin)
                })
                .collect::<Vec<ProgrammableObjectArg>>()
        } else {
            vec![
                ProgrammableObjectArg::Argument(
                    transaction_builder.programmable_split_gas_coin(pt_builder, amount_in as u64).await
                )
            ]
        };

        let orig_coins_arg = ProgrammableTransactionArg::Argument(
            transaction_builder
                .programmable_make_object_vec(
                    pt_builder,
                    orig_coins_args_vec
                ).await?
        );

        // Arg3: bool
        let amount_specified_is_input = ProgrammableTransactionArg::SuiJsonValue(
            SuiJsonValue::new(
                move_value_to_json(
                    &MoveValue::Bool(true)
                )
                .context("failed to convert MoveValue for amount_specified_is_input to JSON")?
            )?
        );

        // Arg4: u64
        let amount_specified = ProgrammableTransactionArg::SuiJsonValue(
            SuiJsonValue::new(
                move_value_to_json(
                    &MoveValue::U64((amount_in) as u64)
                )
                .context("failed to convert MoveValue for amount_specified to JSON")?
            )?
        );

        // Arg5: u65
        let amount_limit = ProgrammableTransactionArg::SuiJsonValue(
            SuiJsonValue::new(
                move_value_to_json(
                    &MoveValue::U64((amount_out) as u64)
                )
                .context("failed to convert MoveValue for amount_limit to JSON")?
            )?
        );

        // Arg6: u64
        let sqrt_price_limit = ProgrammableTransactionArg::SuiJsonValue(
            SuiJsonValue::new(
                move_value_to_json(
                    &MoveValue::U128(
                        if x_to_y {
                            fast_v3_pool::tick_math::MIN_SQRT_PRICE_X64 + 1
                        } else {
                            fast_v3_pool::tick_math::MAX_SQRT_PRICE_X64 - 1
                        }
                    )
                )
                .context("failed to convert MoveValue for sqrt_price_limit to JSON")?
            )?
        );

        // Arg7: &Clock
        let clock = ProgrammableTransactionArg::SuiJsonValue(
            SuiJsonValue::from_object_id(
                ObjectID::from_str(CLOCK_OBJECT_ID)?
            )
        );

        let function = if x_to_y {
            "swap_a2b"
        } else {
            "swap_b2a"
        };

        let call_args = vec![
            global_config, 
            pool,
            orig_coins_arg,
            amount_specified_is_input,
            amount_specified,
            amount_limit,
            sqrt_price_limit,
            clock
        ];

        let type_args = vec![
            SuiTypeTag::new(format!("{}", self.coin_x)), 
            SuiTypeTag::new(format!("{}", self.coin_y)), 
        ];

        // println!("{:#?}", type_args);

        transaction_builder.programmable_move_call(
            pt_builder,
            self.parent_exchange.periphery_id.clone(),
            "pool_script",
            function,
            type_args,
            call_args
        ).await?;
        
        Ok(())
    }

}

#[async_trait]
impl Market for CetusMarket {
    fn coin_x(&self) -> &TypeTag {
        self.coin_x()
    }

    fn coin_y(&self) -> &TypeTag {
        self.coin_y()
    }

    fn coin_x_price(&self) -> Option<U64F64> {
        self.coin_x_price()
    }

    fn coin_y_price(&self) -> Option<U64F64> {
        self.coin_y_price()
    }

    async fn update_with_object_response(&mut self, sui_client: &SuiClient, object_response: &SuiObjectResponse) -> Result<(), anyhow::Error> {
        self.update_with_object_response(sui_client, object_response).await
    }

    fn pool_id(&self) -> &ObjectID {
        self.pool_id()
    }

    fn package_id(&self) -> &ObjectID {
        self.package_id()
    }

    // fn router_id(&self) -> &ObjectID {
    //     self.router_id()
    // }

    // fn compute_swap_x_to_y_mut(&mut self, amount_specified: u128) -> (u128, u128) {
    //     let result = self.compute_swap_x_to_y_mut(amount_specified as u64);

    //     (result.0 as u128, result.1 as u128)
    // }

    // fn compute_swap_y_to_x_mut(&mut self, amount_specified: u128) -> (u128, u128) {
    //     let result = self.compute_swap_y_to_x_mut(amount_specified as u64);

    //     (result.0 as u128, result.1 as u128)
    // }

    fn compute_swap_x_to_y(&self, amount_specified: u128) -> (u128, u128) {
        self.compute_swap_x_to_y(amount_specified)
    }

    fn compute_swap_y_to_x(&self, amount_specified: u128) -> (u128, u128) {
        self.compute_swap_y_to_x(amount_specified)
    }

    fn viable(&self) -> bool {
        self.viable()
    }

    async fn add_swap_to_programmable_transaction(
        &self,
        transaction_builder: &TransactionBuilder,
        pt_builder: &mut ProgrammableTransactionBuilder,
        orig_coins: Option<Vec<ObjectID>>, // the actual coin object in (that you own and has money)
        x_to_y: bool,
        amount_in: u128,
        amount_out: u128,
        recipient: SuiAddress
    ) -> Result<(), anyhow::Error> {

        // if !recipient.is_none() {
        //     return Err(anyhow!("recipient should be none"));
        // }

        self.add_swap_to_programmable_trasaction(
            transaction_builder,
            pt_builder,
            orig_coins,
            x_to_y,
            amount_in,
            amount_out
        ).await
    }
}
