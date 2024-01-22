use frontend::structs::{CircuitBuilder, MixedCell};
use gkr::structs::Circuit;
use goldilocks::SmallField;

use crate::{
    constants::{OpcodeType, VALUE_BIT_WIDTH},
    error::ZKVMError,
};

use super::{
    utils::{uint::UIntAddSub, ChipHandler, PCUInt, TSUInt, UInt},
    ChipChallenges, InstCircuit, Instruction,
};

pub struct PushInstruction<const N: usize>;

register_wires_in!(
    PushInstruction<N>,
    phase0_size {
        phase0_pc => PCUInt::N_OPRAND_CELLS,
        phase0_stack_ts => TSUInt::N_OPRAND_CELLS,
        phase0_stack_top => 1,
        phase0_clk => 1,

        phase0_pc_add_i_plus_1 => N * UIntAddSub::<PCUInt>::N_NO_OVERFLOW_WITNESS_UNSAFE_CELLS,
        phase0_stack_ts_add => UIntAddSub::<TSUInt>::N_NO_OVERFLOW_WITNESS_CELLS,

        phase0_stack_bytes => N
    },
    phase1_size {
        phase1_memory_ts_rlc => 1
    }
);

register_wires_out!(
    PushInstruction<N>,
    global_state_in_size {
        state_in => 1
    },
    global_state_out_size {
        state_out => 1
    },
    stack_push_size {
        value => N
    },
    bytecode_chip_size {
        current => N + 1
    },
    range_chip_size {
        stack_top => 1,
        stack_ts_add => TSUInt::N_RANGE_CHECK_NO_OVERFLOW_CELLS,
        old_stack_ts_lt => TSUInt::N_RANGE_CHECK_CELLS
    }
);

impl<const N: usize> Instruction for PushInstruction<N> {
    const OPCODE: OpcodeType = match N {
        1 => OpcodeType::PUSH1,
        _ => unimplemented!(),
    };

    #[inline]
    fn witness_size(phase: usize) -> usize {
        match phase {
            0 => Self::phase0_size(),
            1 => Self::phase1_size(),
            _ => 0,
        }
    }
    fn construct_circuit<F: SmallField>(
        challenges: &ChipChallenges,
    ) -> Result<InstCircuit<F>, ZKVMError> {
        let mut circuit_builder = CircuitBuilder::new();
        let (phase0_wire_id, phase0) = circuit_builder.create_wire_in(Self::phase0_size());
        let (phase1_wire_id, phase1) = circuit_builder.create_wire_in(Self::phase1_size());
        let mut global_state_in_handler =
            ChipHandler::new(&mut circuit_builder, Self::global_state_in_size());
        let mut global_state_out_handler =
            ChipHandler::new(&mut circuit_builder, Self::global_state_out_size());
        let mut bytecode_chip_handler =
            ChipHandler::new(&mut circuit_builder, Self::bytecode_chip_size());
        let mut stack_push_handler =
            ChipHandler::new(&mut circuit_builder, Self::stack_push_size());
        let mut range_chip_handler =
            ChipHandler::new(&mut circuit_builder, Self::range_chip_size());

        // State update
        let pc = PCUInt::try_from(&phase0[Self::phase0_pc()])?;
        let stack_ts = TSUInt::try_from(&phase0[Self::phase0_stack_ts()])?;
        let memory_ts_rlc = phase1[Self::phase1_memory_ts_rlc().start];
        let stack_top = phase0[Self::phase0_stack_top().start];
        let stack_top_expr = MixedCell::Cell(stack_top);
        let clk = phase0[Self::phase0_clk().start];
        let clk_expr = MixedCell::Cell(clk);
        global_state_in_handler.state_in(
            &mut circuit_builder,
            pc.values(),
            stack_ts.values(),
            &[memory_ts_rlc],
            stack_top,
            clk,
            challenges,
        );

        let next_pc = ChipHandler::add_pc_const(
            &mut circuit_builder,
            &pc,
            N as i64 + 1,
            &phase0[Self::phase0_pc_add_i_plus_1()],
        )?;
        let next_stack_ts = range_chip_handler.add_ts_with_const(
            &mut circuit_builder,
            &stack_ts,
            1,
            &phase0[Self::phase0_stack_ts_add()],
        )?;

        global_state_out_handler.state_out(
            &mut circuit_builder,
            next_pc.values(),
            next_stack_ts.values(),
            &[memory_ts_rlc],
            stack_top_expr.add(F::from(1)),
            clk_expr.add(F::ONE),
            challenges,
        );

        // Check the range of stack_top is within [0, 1 << STACK_TOP_BIT_WIDTH).
        range_chip_handler.range_check_stack_top(&mut circuit_builder, stack_top_expr);

        let stack_bytes = &phase0[Self::phase0_stack_bytes()];
        let stack_values =
            UInt::<N, VALUE_BIT_WIDTH>::from_bytes_big_endien(&mut circuit_builder, stack_bytes)?;
        // Push value to stack
        stack_push_handler.stack_push_values(
            &mut circuit_builder,
            stack_top_expr,
            stack_ts.values(),
            stack_values.values(),
            challenges,
        );

        // Bytecode check for (pc, PUSH{N}), (pc + 1, byte[0]), ..., (pc + N, byte[N - 1])
        bytecode_chip_handler.bytecode_with_pc_opcode(
            &mut circuit_builder,
            pc.values(),
            Self::OPCODE,
            challenges,
        );
        for (i, pc_add_i_plus_1) in phase0[Self::phase0_pc_add_i_plus_1()]
            .chunks(UIntAddSub::<PCUInt>::N_NO_OVERFLOW_WITNESS_UNSAFE_CELLS)
            .enumerate()
        {
            let next_pc = ChipHandler::add_pc_const(
                &mut circuit_builder,
                &pc,
                i as i64 + 1,
                pc_add_i_plus_1,
            )?;
            bytecode_chip_handler.bytecode_with_pc_byte(
                &mut circuit_builder,
                next_pc.values(),
                stack_bytes[i],
                challenges,
            );
        }

        global_state_in_handler.finalize_with_const_pad(&mut circuit_builder, &F::ONE);
        global_state_out_handler.finalize_with_const_pad(&mut circuit_builder, &F::ONE);
        bytecode_chip_handler.finalize_with_repeated_last(&mut circuit_builder);
        stack_push_handler.finalize_with_const_pad(&mut circuit_builder, &F::ONE);
        range_chip_handler.finalize_with_repeated_last(&mut circuit_builder);

        circuit_builder.configure();
        Ok(InstCircuit {
            circuit: Circuit::new(&circuit_builder),
            state_in_wire_id: global_state_in_handler.wire_out_id(),
            state_out_wire_id: global_state_out_handler.wire_out_id(),
            bytecode_chip_wire_id: bytecode_chip_handler.wire_out_id(),
            stack_pop_wire_id: None,
            stack_push_wire_id: Some(stack_push_handler.wire_out_id()),
            range_chip_wire_id: Some(range_chip_handler.wire_out_id()),
            memory_load_wire_id: None,
            memory_store_wire_id: None,
            calldata_chip_wire_id: None,
            phases_wire_id: [Some(phase0_wire_id), Some(phase1_wire_id)],
        })
    }
}
