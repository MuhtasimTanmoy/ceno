use frontend::structs::WireId;
use gkr::structs::Circuit;
use goldilocks::SmallField;

use crate::{constants::OpcodeType, error::ZKVMError};

#[macro_use]
mod macros;

// arithmetic
pub mod add;

// bitwise
pub mod gt;

// control
pub mod jump;
pub mod jumpdest;
pub mod jumpi;
pub mod ret;

// stack
pub mod dup;
pub mod pop;
pub mod push;
pub mod swap;

// memory
pub mod mstore;

// system
pub mod calldataload;

pub mod utils;

#[derive(Clone, Copy, Debug, Default)]
pub struct ChipChallenges {
    // Challenges for multiple-tuple chip records
    record_rlc: usize,
    // Challenges for multiple-cell values
    record_item_rlc: usize,
}

impl ChipChallenges {
    pub fn new() -> Self {
        Self {
            record_rlc: 2,
            record_item_rlc: 1,
        }
    }
    pub fn bytecode(&self) -> usize {
        self.record_rlc
    }
    pub fn stack(&self) -> usize {
        self.record_rlc
    }
    pub fn global_state(&self) -> usize {
        self.record_rlc
    }
    pub fn mem(&self) -> usize {
        self.record_rlc
    }
    pub fn range(&self) -> usize {
        self.record_rlc
    }
    pub fn calldata(&self) -> usize {
        self.record_rlc
    }
    pub fn record_item_rlc(&self) -> usize {
        self.record_item_rlc
    }
}

#[derive(Clone, Debug)]
pub struct InstCircuit<F: SmallField> {
    circuit: Circuit<F>,

    // Wires out index
    state_in_wire_id: WireId,
    state_out_wire_id: WireId,
    bytecode_chip_wire_id: WireId,
    stack_pop_wire_id: Option<WireId>,
    stack_push_wire_id: Option<WireId>,
    range_chip_wire_id: Option<WireId>,
    memory_load_wire_id: Option<WireId>,
    memory_store_wire_id: Option<WireId>,
    calldata_chip_wire_id: Option<WireId>,

    // Wires in index
    phases_wire_id: [Option<WireId>; 2],
}

pub(crate) trait Instruction {
    const OPCODE: OpcodeType;

    fn witness_size(phase: usize) -> usize;

    fn construct_circuit<F: SmallField>(
        challenges: &ChipChallenges,
    ) -> Result<InstCircuit<F>, ZKVMError>;
}
