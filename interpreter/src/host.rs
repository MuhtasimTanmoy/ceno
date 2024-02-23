use crate::{
    primitives::{Address, Bytecode, Bytes, Env, B256, U256},
    SelfDestructResult,
};
use alloc::vec::Vec;

mod dummy;
pub use dummy::DummyHost;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreRecord {
    pub(crate) opcode: u8,
    pub(crate) clock: u64,
    pub(crate) pc: u64,
    pub(crate) stack_timestamp: u64,
    pub(crate) memory_timestamp: u64,
    pub(crate) stack_top: u64,
}

impl PreRecord {
    pub(crate) fn complete(self, operands: Vec<U256>, operands_timestamps: Vec<u64>) -> Record {
        Record {
            opcode: self.opcode,
            clock: self.clock,
            pc: self.pc,
            stack_timestamp: self.stack_timestamp,
            memory_timestamp: self.memory_timestamp,
            operands,
            operands_timestamps,
            stack_top: self.stack_top,
            ret_info: ReturnInfo::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub opcode: u8,
    pub clock: u64,
    pub pc: u64,
    pub stack_timestamp: u64,
    pub memory_timestamp: u64,
    pub operands: Vec<U256>,
    pub operands_timestamps: Vec<u64>,
    pub stack_top: u64,
    pub ret_info: ReturnInfo,
}

/// The information collected specifically for the return instruction
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReturnInfo {
    /// Address, timestamp, and value of the memory content at the ret
    /// instruction, except those output by the ret instruction.
    pub rest_memory_loads: Vec<(u64, u64, u8)>,
    /// Addresses and initial values of the memory that are ever accessed
    pub rest_memory_store: Vec<(u64, u8)>,
}

impl ReturnInfo {
    pub fn new() -> Self {
        Self {
            rest_memory_loads: vec![],
            rest_memory_store: vec![],
        }
    }
}

/// EVM context host.
pub trait Host {
    /// Returns a mutable reference to the environment.
    fn env(&mut self) -> &mut Env;

    /// Load an account.
    ///
    /// Returns (is_cold, is_new_account)
    fn load_account(&mut self, address: Address) -> Option<(bool, bool)>;

    /// Get the block hash of the given block `number`.
    fn block_hash(&mut self, number: U256) -> Option<B256>;

    /// Get balance of `address` and if the account is cold.
    fn balance(&mut self, address: Address) -> Option<(U256, bool)>;

    /// Get code of `address` and if the account is cold.
    fn code(&mut self, address: Address) -> Option<(Bytecode, bool)>;

    /// Get code hash of `address` and if the account is cold.
    fn code_hash(&mut self, address: Address) -> Option<(B256, bool)>;

    /// Get storage value of `address` at `index` and if the account is cold.
    fn sload(&mut self, address: Address, index: U256) -> Option<(U256, bool)>;

    /// Set storage value of account address at index.
    ///
    /// Returns (original, present, new, is_cold).
    fn sstore(
        &mut self,
        address: Address,
        index: U256,
        value: U256,
    ) -> Option<(U256, U256, U256, bool)>;

    /// Get the transient storage value of `address` at `index`.
    fn tload(&mut self, address: Address, index: U256) -> U256;

    /// Set the transient storage value of `address` at `index`.
    fn tstore(&mut self, address: Address, index: U256, value: U256);

    /// Emit a log owned by `address` with given `topics` and `data`.
    fn log(&mut self, address: Address, topics: Vec<B256>, data: Bytes);

    /// Record the instruction being executed
    fn record(&mut self, record: &Record);

    /// Mark `address` to be deleted, with funds transferred to `target`.
    fn selfdestruct(&mut self, address: Address, target: Address) -> Option<SelfDestructResult>;
}
