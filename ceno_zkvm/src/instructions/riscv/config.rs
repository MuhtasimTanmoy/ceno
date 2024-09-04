use std::mem::MaybeUninit;

use crate::{expression::WitIn, set_val, utils::i64_to_ext};
use ff_ext::ExtensionField;
use itertools::Itertools;

#[derive(Clone)]
pub struct IsEqualConfig {
    pub is_equal_per_limb: Vec<WitIn>,
    pub diff_inv_per_limb: Vec<WitIn>,
    pub diff_inv: WitIn,
    pub is_equal: WitIn,
}

#[derive(Clone)]
pub struct MsbConfig {
    pub msb: WitIn,
    pub high_limb_no_msb: WitIn,
}

pub struct MsbInput<'a> {
    pub limbs: &'a [u8],
}

impl MsbInput<'_> {
    pub fn assign<E: ExtensionField>(
        &self,
        instance: &mut [MaybeUninit<E>],
        config: &MsbConfig,
    ) -> (u8, u8) {
        let n_limbs = self.limbs.len();
        assert!(n_limbs > 0);
        let mut high_limb = self.limbs[n_limbs - 1];
        let msb = (high_limb >> 7) & 1;
        set_val!(instance, config.msb, { i64_to_ext::<E>(msb as i64) });
        high_limb &= 0b0111_1111;
        set_val!(instance, config.high_limb_no_msb, {
            i64_to_ext::<E>(high_limb as i64)
        });
        (msb, high_limb)
    }
}

#[derive(Clone)]
pub struct LtuConfig {
    pub indexes: Vec<WitIn>,
    pub acc_indexes: Vec<WitIn>,
    pub byte_diff_inv: WitIn,
    pub lhs_ne_byte: WitIn,
    pub rhs_ne_byte: WitIn,
    pub is_ltu: WitIn,
}

pub struct LtuInput<'a> {
    pub lhs_limbs: &'a [u8],
    pub rhs_limbs: &'a [u8],
}

impl LtuInput<'_> {
    pub fn assign<E: ExtensionField>(
        &self,
        instance: &mut [MaybeUninit<E>],
        config: &LtuConfig,
    ) -> bool {
        let mut idx = 0;
        let mut flag: bool = false;
        for (i, (&lhs, &rhs)) in self
            .lhs_limbs
            .iter()
            .zip(self.rhs_limbs.iter())
            .enumerate()
            .rev()
        {
            if lhs != rhs {
                idx = i;
                flag = true;
                break;
            }
        }
        set_val!(instance, config.indexes[idx], {
            i64_to_ext::<E>(flag as i64)
        });
        config.acc_indexes.iter().enumerate().for_each(|(id, wit)| {
            if id <= idx {
                set_val!(instance, wit, { i64_to_ext::<E>(flag as i64) });
            } else {
                set_val!(instance, wit, E::ZERO);
            }
        });
        let lhs_ne_byte = i64_to_ext::<E>(self.lhs_limbs[idx] as i64);
        let rhs_ne_byte = i64_to_ext::<E>(self.rhs_limbs[idx] as i64);
        set_val!(instance, config.lhs_ne_byte, lhs_ne_byte);
        set_val!(instance, config.rhs_ne_byte, rhs_ne_byte);
        set_val!(instance, config.byte_diff_inv, {
            if flag {
                (lhs_ne_byte - rhs_ne_byte).invert().unwrap()
            } else {
                E::ONE
            }
        });
        let is_ltu = self.lhs_limbs[idx] < self.rhs_limbs[idx];
        set_val!(instance, config.is_ltu, { i64_to_ext::<E>(is_ltu as i64) });
        is_ltu
    }
}

#[derive(Clone)]
pub struct LtConfig {
    pub lhs_msb: MsbConfig,
    pub rhs_msb: MsbConfig,
    pub msb_is_equal: WitIn,
    pub msb_diff_inv: WitIn,
    pub is_ltu: LtuConfig,
    pub is_lt: WitIn,
}

pub struct LtInput<'a> {
    pub lhs_limbs: &'a [u8],
    pub rhs_limbs: &'a [u8],
}

impl LtInput<'_> {
    pub fn assign<E: ExtensionField>(
        &self,
        instance: &mut [MaybeUninit<E>],
        config: &LtConfig,
    ) -> bool {
        let n_limbs = self.lhs_limbs.len();
        let lhs_msb_input = MsbInput {
            limbs: self.lhs_limbs,
        };
        let (lhs_msb, lhs_high_limb_no_msb) = lhs_msb_input.assign(instance, &config.lhs_msb);
        let rhs_msb_input = MsbInput {
            limbs: self.rhs_limbs,
        };
        let (rhs_msb, rhs_high_limb_no_msb) = rhs_msb_input.assign(instance, &config.rhs_msb);

        let mut lhs_limbs_no_msb = self.lhs_limbs.iter().copied().collect_vec();
        lhs_limbs_no_msb[n_limbs - 1] = lhs_high_limb_no_msb;

        let mut rhs_limbs_no_msb = self.rhs_limbs.iter().copied().collect_vec();
        rhs_limbs_no_msb[n_limbs - 1] = rhs_high_limb_no_msb;

        let ltu_input = LtuInput {
            lhs_limbs: &lhs_limbs_no_msb,
            rhs_limbs: &rhs_limbs_no_msb,
        };
        let is_ltu = ltu_input.assign(instance, &config.is_ltu);

        let msb_is_equal = lhs_msb == rhs_msb;
        let msb_diff_inv = if msb_is_equal {
            0
        } else {
            lhs_msb as i64 - rhs_msb as i64
        };
        set_val!(instance, config.msb_is_equal, {
            i64_to_ext::<E>(msb_is_equal as i64)
        });
        set_val!(instance, config.msb_diff_inv, {
            i64_to_ext::<E>(msb_diff_inv)
        });

        // is_lt = a_s\cdot (1-b_s)+eq(a_s,b_s)\cdot ltu(a_{<s},b_{<s})$
        let is_lt = lhs_msb * (1 - rhs_msb) + msb_is_equal as u8 * is_ltu as u8;
        set_val!(instance, config.is_lt, { i64_to_ext::<E>(is_lt as i64) });

        assert!(is_lt == 0 || is_lt == 1);
        is_lt > 0
    }
}
