use actix::Message;
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
