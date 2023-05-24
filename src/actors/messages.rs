use crate::errors::Result;
use actix::{dev::MessageResponse, dev::OneshotSender, Actor, Message};
use fuels::types::block::Block;

#[derive(Message, Debug)]
#[rtype(result = "()")]
pub struct CheckNewBlock {}

#[derive(Message, Debug)]
#[rtype(result = "()")]
pub struct BlockUpdate {
    pub block: u64, //TODO: add fuels::types::block::Block
}

#[derive(Message, Debug)]
#[rtype(result = "()")]
pub struct CheckTxStatus {}

#[derive(Message, Debug)]
#[rtype(result = "FuelStatus")]
pub struct ReportFuelStatus {}

pub struct FuelStatus {
    pub latest_fuel_block: u64,
}

impl<A, M> MessageResponse<A, M> for FuelStatus
where
    A: Actor,
    M: Message<Result = FuelStatus>,
{
    fn handle(self, ctx: &mut A::Context, tx: Option<OneshotSender<M::Result>>) {
        if let Some(tx) = tx {
            tx.send(self);
        }
    }
}

#[derive(Message, Debug)]
#[rtype(result = "EthStatus")]
pub struct ReportEthStatus {}

pub struct EthStatus {
    pub latest_commited_block: u64,
    pub ethereum_wallet_gas_balance: u64, // use U256 type from eth lib
}
