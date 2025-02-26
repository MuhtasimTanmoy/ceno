use std::ops::Range;

use crate::addr::{Addr, RegIdx};

/// The Platform struct holds the parameters of the VM.
/// It defines:
/// - the layout of virtual memory,
/// - special addresses, such as the initial PC,
/// - codes of environment calls.
#[derive(Clone, Debug)]
pub struct Platform {
    pub rom: Range<Addr>,
    pub ram: Range<Addr>,
    pub public_io: Range<Addr>,
    pub stack_top: Addr,
    /// If true, ecall instructions are no-op instead of trap. Testing only.
    pub unsafe_ecall_nop: bool,
}

pub const CENO_PLATFORM: Platform = Platform {
    rom: 0x2000_0000..0x3000_0000,
    ram: 0x8000_0000..0xFFFF_0000,
    public_io: 0x3000_1000..0x3000_2000,
    stack_top: 0xC0000000,
    unsafe_ecall_nop: false,
};

impl Platform {
    // Virtual memory layout.

    pub fn is_rom(&self, addr: Addr) -> bool {
        self.rom.contains(&addr)
    }

    pub fn is_ram(&self, addr: Addr) -> bool {
        self.ram.contains(&addr)
    }

    pub fn is_pub_io(&self, addr: Addr) -> bool {
        self.public_io.contains(&addr)
    }

    /// Virtual address of a register.
    pub const fn register_vma(index: RegIdx) -> Addr {
        // Register VMAs are aligned, cannot be confused with indices, and readable in hex.
        (index << 8) as Addr
    }

    /// Register index from a virtual address (unchecked).
    pub const fn register_index(vma: Addr) -> RegIdx {
        (vma >> 8) as RegIdx
    }

    // Startup.

    pub const fn pc_base(&self) -> Addr {
        self.rom.start
    }

    // Permissions.

    pub fn can_read(&self, addr: Addr) -> bool {
        self.is_rom(addr) || self.is_ram(addr) || self.is_pub_io(addr)
    }

    pub fn can_write(&self, addr: Addr) -> bool {
        self.is_ram(addr)
    }

    pub fn can_execute(&self, addr: Addr) -> bool {
        self.is_rom(addr)
    }

    // Environment calls.

    /// Register containing the ecall function code. (x5, t0)
    pub const fn reg_ecall() -> RegIdx {
        5
    }

    /// Register containing the first function argument. (x10, a0)
    pub const fn reg_arg0() -> RegIdx {
        10
    }

    /// Register containing the 2nd function argument. (x11, a1)
    pub const fn reg_arg1() -> RegIdx {
        11
    }

    /// The code of ecall HALT.
    pub const fn ecall_halt() -> u32 {
        0
    }

    /// The code of success.
    pub const fn code_success() -> u32 {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{VMState, WORD_SIZE};

    #[test]
    fn test_no_overlap() {
        let p = CENO_PLATFORM;
        assert!(p.can_execute(p.pc_base()));
        // ROM and RAM do not overlap.
        assert!(!p.is_rom(p.ram.start));
        assert!(!p.is_rom(p.ram.end - WORD_SIZE as Addr));
        assert!(!p.is_ram(p.rom.start));
        assert!(!p.is_ram(p.rom.end - WORD_SIZE as Addr));
        // Registers do not overlap with ROM or RAM.
        for reg in [
            Platform::register_vma(0),
            Platform::register_vma(VMState::REG_COUNT - 1),
        ] {
            assert!(!p.is_rom(reg));
            assert!(!p.is_ram(reg));
        }
    }
}
