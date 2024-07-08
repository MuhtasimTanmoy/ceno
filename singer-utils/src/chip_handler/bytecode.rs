// TODO: rename and restructure

use crate::chip_handler::rom_handler::ROMHandler;
use crate::chip_handler::util::cell_to_mixed;
use crate::constants::OpcodeType;
use crate::structs::ROMType;
use ark_std::iterable::Iterable;
use ff_ext::ExtensionField;
use itertools::Itertools;
use simple_frontend::structs::{Cell, CellId, CircuitBuilder, MixedCell};
use std::cell::RefCell;
use std::rc::Rc;

pub struct BytecodeChip<Ext: ExtensionField> {
    rom_handler: Rc<RefCell<ROMHandler<Ext>>>,
}

impl<Ext: ExtensionField> BytecodeChip<Ext> {
    // TODO: document
    pub fn new(rom_handler: Rc<RefCell<ROMHandler<Ext>>>) -> Self {
        Self { rom_handler }
    }

    // TODO: rename and document
    pub fn bytecode_with_pc_opcode(
        &self,
        circuit_builder: &mut CircuitBuilder<Ext>,
        pc: &[CellId],
        opcode: OpcodeType,
    ) {
        let key = [
            vec![MixedCell::Constant(Ext::BaseField::from(
                ROMType::Bytecode as u64,
            ))],
            cell_to_mixed(pc),
        ]
        .concat();

        self.rom_handler.borrow_mut().read_mixed(
            circuit_builder,
            &key,
            &[MixedCell::Constant(Ext::BaseField::from(opcode as u64))],
        );
    }

    // TODO: rename and document
    pub fn bytecode_with_pc_byte(
        &self,
        circuit_builder: &mut CircuitBuilder<Ext>,
        pc: &[CellId],
        byte: CellId,
    ) {
        let key = [
            vec![MixedCell::Constant(Ext::BaseField::from(
                ROMType::Bytecode as u64,
            ))],
            cell_to_mixed(pc),
        ]
        .concat();
        self.rom_handler
            .borrow_mut()
            .read_mixed(circuit_builder, &key, &[byte.into()]);
    }
}
