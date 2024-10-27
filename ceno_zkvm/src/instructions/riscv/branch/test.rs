use ceno_emul::{ByteAddr, Change, PC_STEP_SIZE, StepRecord, Word, encode_rv32};
use goldilocks::GoldilocksExt2;
use itertools::Itertools;
use multilinear_extensions::mle::IntoMLEs;

use super::*;
use crate::{
    circuit_builder::{CircuitBuilder, ConstraintSystem},
    instructions::Instruction,
    scheme::mock_prover::{MOCK_PC_START, MockProver},
};

const A: Word = 0xbead1010;
const B: Word = 0xef552020;

fn imm(imm: i32) -> u32 {
    // imm is 13 bits in B-type
    const IMM_MAX: i32 = 2i32.pow(13);
    if imm.is_negative() {
        (IMM_MAX + imm) as u32
    } else {
        imm as u32
    }
}

#[test]
fn test_opcode_beq() {
    impl_opcode_beq(false);
    impl_opcode_beq(true);
}

fn impl_opcode_beq(equal: bool) {
    let mut cs = ConstraintSystem::<GoldilocksExt2>::new(|| "riscv");
    let mut cb = CircuitBuilder::new(&mut cs);
    let config = cb.namespace(|| "beq", BeqInstruction::construct_circuit);

    let insn_code = encode_rv32(InsnKind::BEQ, 2, 3, 0, imm(8));
    let pc_offset = if equal { 8 } else { PC_STEP_SIZE };
    let (raw_witin, lkm) =
        BeqInstruction::assign_instances(&config, cb.cs.num_witin as usize, vec![
            StepRecord::new_b_instruction(
                3,
                Change::new(MOCK_PC_START, MOCK_PC_START + pc_offset),
                insn_code,
                A,
                if equal { A } else { B },
                0,
            ),
        ]);

    MockProver::assert_satisfied(
        &cb,
        &raw_witin
            .de_interleaving()
            .into_mles()
            .into_iter()
            .map(|v| v.into())
            .collect_vec(),
        &[insn_code],
        None,
        Some(lkm),
    );
}

#[test]
fn test_opcode_bne() {
    impl_opcode_bne(false);
    impl_opcode_bne(true);
}

fn impl_opcode_bne(equal: bool) {
    let mut cs = ConstraintSystem::<GoldilocksExt2>::new(|| "riscv");
    let mut cb = CircuitBuilder::new(&mut cs);
    let config = cb.namespace(|| "bne", BneInstruction::construct_circuit);

    let insn_code = encode_rv32(InsnKind::BNE, 2, 3, 0, imm(8));
    let pc_offset = if equal { PC_STEP_SIZE } else { 8 };
    let (raw_witin, lkm) =
        BneInstruction::assign_instances(&config, cb.cs.num_witin as usize, vec![
            StepRecord::new_b_instruction(
                3,
                Change::new(MOCK_PC_START, MOCK_PC_START + pc_offset),
                insn_code,
                A,
                if equal { A } else { B },
                0,
            ),
        ]);

    MockProver::assert_satisfied(
        &cb,
        &raw_witin
            .de_interleaving()
            .into_mles()
            .into_iter()
            .map(|v| v.into())
            .collect_vec(),
        &[insn_code],
        None,
        Some(lkm),
    );
}

#[test]
fn test_bltu_circuit() {
    impl_bltu_circuit(false, 1, 0);
    impl_bltu_circuit(false, 0, 0);
    impl_bltu_circuit(false, 0xFFFF_FFFF, 0xFFFF_FFFF);

    impl_bltu_circuit(true, 0, 1);
    impl_bltu_circuit(true, 0xFFFF_FFFE, 0xFFFF_FFFF);
    impl_bltu_circuit(true, 0xEFFF_FFFF, 0xFFFF_FFFF);
}

fn impl_bltu_circuit(taken: bool, a: u32, b: u32) {
    let mut cs = ConstraintSystem::new(|| "riscv");
    let mut circuit_builder = CircuitBuilder::<GoldilocksExt2>::new(&mut cs);
    let config = BltuInstruction::construct_circuit(&mut circuit_builder);

    let pc_after = if taken {
        ByteAddr(MOCK_PC_START.0 - 8)
    } else {
        MOCK_PC_START + PC_STEP_SIZE
    };

    let insn_code = encode_rv32(InsnKind::BLTU, 2, 3, 0, imm(-8));
    println!("{:#b}", insn_code);
    let (raw_witin, lkm) =
        BltuInstruction::assign_instances(&config, circuit_builder.cs.num_witin as usize, vec![
            StepRecord::new_b_instruction(
                12,
                Change::new(MOCK_PC_START, pc_after),
                insn_code,
                a as Word,
                b as Word,
                10,
            ),
        ]);

    MockProver::assert_satisfied(
        &circuit_builder,
        &raw_witin
            .de_interleaving()
            .into_mles()
            .into_iter()
            .map(|v| v.into())
            .collect_vec(),
        &[insn_code],
        None,
        Some(lkm),
    );
}

#[test]
fn test_bgeu_circuit() {
    impl_bgeu_circuit(true, 1, 0);
    impl_bgeu_circuit(true, 0, 0);
    impl_bgeu_circuit(true, 0xFFFF_FFFF, 0xFFFF_FFFF);

    impl_bgeu_circuit(false, 0, 1);
    impl_bgeu_circuit(false, 0xFFFF_FFFE, 0xFFFF_FFFF);
    impl_bgeu_circuit(false, 0xEFFF_FFFF, 0xFFFF_FFFF);
}

fn impl_bgeu_circuit(taken: bool, a: u32, b: u32) {
    let mut cs = ConstraintSystem::new(|| "riscv");
    let mut circuit_builder = CircuitBuilder::<GoldilocksExt2>::new(&mut cs);
    let config = BgeuInstruction::construct_circuit(&mut circuit_builder);

    let pc_after = if taken {
        ByteAddr(MOCK_PC_START.0 - 8)
    } else {
        MOCK_PC_START + PC_STEP_SIZE
    };

    let insn_code = encode_rv32(InsnKind::BGEU, 2, 3, 0, imm(-8));
    let (raw_witin, lkm) =
        BgeuInstruction::assign_instances(&config, circuit_builder.cs.num_witin as usize, vec![
            StepRecord::new_b_instruction(
                12,
                Change::new(MOCK_PC_START, pc_after),
                insn_code,
                a as Word,
                b as Word,
                10,
            ),
        ]);

    MockProver::assert_satisfied(
        &circuit_builder,
        &raw_witin
            .de_interleaving()
            .into_mles()
            .into_iter()
            .map(|v| v.into())
            .collect_vec(),
        &[insn_code],
        None,
        Some(lkm),
    );
}

#[test]
fn test_blt_circuit() {
    impl_blt_circuit(false, 0, 0);
    impl_blt_circuit(true, 0, 1);

    impl_blt_circuit(false, 1, -10);
    impl_blt_circuit(false, -10, -10);
    impl_blt_circuit(false, -9, -10);
    impl_blt_circuit(true, -9, 1);
    impl_blt_circuit(true, -10, -9);
}

fn impl_blt_circuit(taken: bool, a: i32, b: i32) {
    let mut cs = ConstraintSystem::new(|| "riscv");
    let mut circuit_builder = CircuitBuilder::<GoldilocksExt2>::new(&mut cs);
    let config = BltInstruction::construct_circuit(&mut circuit_builder);

    let pc_after = if taken {
        ByteAddr(MOCK_PC_START.0 - 8)
    } else {
        MOCK_PC_START + PC_STEP_SIZE
    };

    let insn_code = encode_rv32(InsnKind::BLT, 2, 3, 0, imm(-8));
    let (raw_witin, lkm) =
        BltInstruction::assign_instances(&config, circuit_builder.cs.num_witin as usize, vec![
            StepRecord::new_b_instruction(
                12,
                Change::new(MOCK_PC_START, pc_after),
                insn_code,
                a as Word,
                b as Word,
                10,
            ),
        ]);

    MockProver::assert_satisfied(
        &circuit_builder,
        &raw_witin
            .de_interleaving()
            .into_mles()
            .into_iter()
            .map(|v| v.into())
            .collect_vec(),
        &[insn_code],
        None,
        Some(lkm),
    );
}

#[test]
fn test_bge_circuit() {
    impl_bge_circuit(true, 0, 0);
    impl_bge_circuit(false, 0, 1);

    impl_bge_circuit(true, 1, -10);
    impl_bge_circuit(true, -10, -10);
    impl_bge_circuit(true, -9, -10);
    impl_bge_circuit(false, -9, 1);
    impl_bge_circuit(false, -10, -9);
}

fn impl_bge_circuit(taken: bool, a: i32, b: i32) {
    let mut cs = ConstraintSystem::new(|| "riscv");
    let mut circuit_builder = CircuitBuilder::<GoldilocksExt2>::new(&mut cs);
    let config = BgeInstruction::construct_circuit(&mut circuit_builder);

    let pc_after = if taken {
        ByteAddr(MOCK_PC_START.0 - 8)
    } else {
        MOCK_PC_START + PC_STEP_SIZE
    };

    let insn_code = encode_rv32(InsnKind::BGE, 2, 3, 0, imm(-8));
    let (raw_witin, lkm) =
        BgeInstruction::assign_instances(&config, circuit_builder.cs.num_witin as usize, vec![
            StepRecord::new_b_instruction(
                12,
                Change::new(MOCK_PC_START, pc_after),
                insn_code,
                a as Word,
                b as Word,
                10,
            ),
        ]);

    MockProver::assert_satisfied(
        &circuit_builder,
        &raw_witin
            .de_interleaving()
            .into_mles()
            .into_iter()
            .map(|v| v.into())
            .collect_vec(),
        &[insn_code],
        None,
        Some(lkm),
    );
}
