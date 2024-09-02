use ff_ext::ExtensionField;
use multilinear_extensions::mle::FieldType;

mod basecode;
pub use basecode::{Basecode, BasecodeDefaultSpec};

mod rs;
use plonky2::util::log2_strict;
use rayon::{
    iter::{IndexedParallelIterator, IntoParallelIterator, ParallelIterator},
    slice::ParallelSlice,
};
pub use rs::{coset_fft, fft, fft_root_table, RSCode, RSCodeDefaultSpec};

use serde::{de::DeserializeOwned, Serialize};

use crate::{util::arithmetic::interpolate2_weights, Error};

pub trait EncodingProverParameters {
    fn get_max_message_size_log(&self) -> usize;
}

pub trait EncodingScheme<E: ExtensionField>: std::fmt::Debug + Clone {
    type PublicParameters: Clone + std::fmt::Debug + Serialize + DeserializeOwned;
    type ProverParameters: Clone
        + std::fmt::Debug
        + Serialize
        + DeserializeOwned
        + EncodingProverParameters
        + Sync;
    type VerifierParameters: Clone + std::fmt::Debug + Serialize + DeserializeOwned + Sync;

    fn setup(max_msg_size_log: usize, rng_seed: [u8; 32]) -> Self::PublicParameters;

    fn trim(
        pp: &Self::PublicParameters,
        max_msg_size_log: usize,
    ) -> Result<(Self::ProverParameters, Self::VerifierParameters), Error>;

    fn encode(pp: &Self::ProverParameters, coeffs: &FieldType<E>) -> FieldType<E>;

    /// Encodes a message of small length, such that the verifier is also able
    /// to execute the encoding.
    fn encode_small(vp: &Self::VerifierParameters, coeffs: &FieldType<E>) -> FieldType<E>;

    fn get_number_queries() -> usize;

    fn get_rate_log() -> usize;

    fn get_basecode_msg_size_log() -> usize;

    /// Whether the message needs to be bit-reversed to allow even-odd
    /// folding. If the folding is already even-odd style (like RS code),
    /// then set this function to return false. If the folding is originally
    /// left-right, like basefold, then return true.
    fn message_need_bit_reversion() -> bool;

    /// Returns three values: x0, x1 and 1/(x1-x0). Note that although
    /// 1/(x1-x0) can be computed from the other two values, we return it
    /// separately because inversion is expensive.
    /// These three values can be used to interpolate a linear function
    /// that passes through the two points (x0, y0) and (x1, y1), for the
    /// given y0 and y1, then compute the value of the linear function at
    /// any give x.
    /// Params:
    /// - level: which particular code in this family of codes?
    /// - index: position in the codeword (after folded)
    fn prover_folding_coeffs(pp: &Self::ProverParameters, level: usize, index: usize) -> (E, E, E);

    /// The same as `prover_folding_coeffs`, but for the verifier. The two
    /// functions, although provide the same functionality, may use different
    /// implementations. For example, prover can use precomputed values stored
    /// in the parameters, but the verifier may need to recompute them.
    fn verifier_folding_coeffs(
        vp: &Self::VerifierParameters,
        level: usize,
        index: usize,
    ) -> (E, E, E);

    /// Fold the given codeword into a smaller codeword of half size, using
    /// the folding coefficients computed by `prover_folding_coeffs`.
    /// The given codeword is assumed to be bit-reversed on the original
    /// codeword directly produced from the `encode` method.
    fn fold_bitreversed_codeword(
        pp: &Self::ProverParameters,
        codeword: &FieldType<E>,
        challenge: E,
    ) -> Vec<E> {
        let level = log2_strict(codeword.len()) - 1;
        match codeword {
            FieldType::Ext(codeword) => codeword
                .par_chunks_exact(2)
                .enumerate()
                .map(|(i, ys)| {
                    let (x0, x1, w) = Self::prover_folding_coeffs(pp, level, i);
                    interpolate2_weights([(x0, ys[0]), (x1, ys[1])], w, challenge)
                })
                .collect::<Vec<_>>(),
            FieldType::Base(codeword) => codeword
                .par_chunks_exact(2)
                .enumerate()
                .map(|(i, ys)| {
                    let (x0, x1, w) = Self::prover_folding_coeffs(pp, level, i);
                    interpolate2_weights([(x0, E::from(ys[0])), (x1, E::from(ys[1]))], w, challenge)
                })
                .collect::<Vec<_>>(),
            _ => panic!("Unsupported field type"),
        }
    }

    /// Fold the given message into a smaller message of half size using challenge
    /// as the random linear combination coefficient.
    /// Note that this is either even-odd fold or left-right fold,
    /// assuming the message has been bit-reversed (since this is the case
    /// in basefold).
    /// So if the message folding does not need bit reversion at all,
    /// (specified by the `message_need_bit_reversion` function)
    /// then the folding should be left-and-right.
    fn fold_bitreversed_message(msg: &FieldType<E>, challenge: E) -> Vec<E> {
        if Self::message_need_bit_reversion() {
            match msg {
                FieldType::Ext(msg) => msg
                    .par_chunks_exact(2)
                    .map(|ys| ys[0] + ys[1] * challenge)
                    .collect::<Vec<_>>(),
                FieldType::Base(msg) => msg
                    .par_chunks_exact(2)
                    .map(|ys| E::from(ys[0]) + E::from(ys[1]) * challenge)
                    .collect::<Vec<_>>(),
                _ => panic!("Unsupported field type"),
            }
        } else {
            match msg {
                FieldType::Ext(msg) => (0..(msg.len() >> 1))
                    .into_par_iter()
                    .map(|i| challenge * msg[(msg.len() >> 1) + i] + msg[i])
                    .collect::<Vec<_>>(),
                FieldType::Base(msg) => (0..(msg.len() >> 1))
                    .into_par_iter()
                    .map(|i| challenge * msg[(msg.len() >> 1) + i] + msg[i])
                    .collect::<Vec<_>>(),
                _ => panic!("Unsupported field type"),
            }
        }
    }
}

fn concatenate_field_types<E: ExtensionField>(coeffs: &Vec<FieldType<E>>) -> FieldType<E> {
    match coeffs[0] {
        FieldType::Ext(_) => {
            let res = coeffs
                .iter()
                .map(|x| match x {
                    FieldType::Ext(x) => x.iter().map(|x| *x),
                    _ => unreachable!(),
                })
                .flatten()
                .collect::<Vec<_>>();
            FieldType::Ext(res)
        }
        FieldType::Base(_) => {
            let res = coeffs
                .iter()
                .map(|x| match x {
                    FieldType::Base(x) => x.iter().map(|x| *x),
                    _ => unreachable!(),
                })
                .flatten()
                .collect::<Vec<_>>();
            FieldType::Base(res)
        }
        _ => unreachable!(),
    }
}

#[cfg(test)]
pub(crate) mod test_util {
    use ff_ext::ExtensionField;
    use multilinear_extensions::mle::FieldType;
    use rand::rngs::OsRng;

    use crate::util::plonky2_util::reverse_index_bits_in_place_field_type;

    use super::EncodingScheme;

    pub fn test_codeword_folding<E: ExtensionField, Code: EncodingScheme<E>>() {
        let num_vars = 12;

        let poly: Vec<E> = (0..(1 << num_vars)).map(|i| E::from(i)).collect();
        let mut poly = FieldType::Ext(poly);

        let rng_seed = [0; 32];
        let pp: Code::PublicParameters = Code::setup(num_vars, rng_seed);
        let (pp, _) = Code::trim(&pp, num_vars).unwrap();
        let mut codeword = Code::encode(&pp, &poly);
        reverse_index_bits_in_place_field_type(&mut codeword);
        reverse_index_bits_in_place_field_type(&mut poly);
        let challenge = E::random(&mut OsRng);
        let folded_codeword = Code::fold_bitreversed_codeword(&pp, &codeword, challenge);
        let mut folded_message = FieldType::Ext(Code::fold_bitreversed_message(&poly, challenge));
        reverse_index_bits_in_place_field_type(&mut folded_message);

        let mut encoded_folded_message = Code::encode(&pp, &folded_message);
        reverse_index_bits_in_place_field_type(&mut encoded_folded_message);
        let encoded_folded_message = match encoded_folded_message {
            FieldType::Ext(coeffs) => coeffs,
            _ => panic!("Wrong field type"),
        };
        for (i, (a, b)) in folded_codeword
            .iter()
            .zip(encoded_folded_message.iter())
            .enumerate()
        {
            assert_eq!(a, b, "Failed at index {}", i);
        }

        let mut folded_codeword = FieldType::Ext(folded_codeword);
        for round in 0..4 {
            let folded_codeword_vec =
                Code::fold_bitreversed_codeword(&pp, &folded_codeword, challenge);

            reverse_index_bits_in_place_field_type(&mut folded_message);
            folded_message =
                FieldType::Ext(Code::fold_bitreversed_message(&folded_message, challenge));
            reverse_index_bits_in_place_field_type(&mut folded_message);

            let mut encoded_folded_message = Code::encode(&pp, &folded_message);
            reverse_index_bits_in_place_field_type(&mut encoded_folded_message);
            let encoded_folded_message = match encoded_folded_message {
                FieldType::Ext(coeffs) => coeffs,
                _ => panic!("Wrong field type"),
            };
            for (i, (a, b)) in folded_codeword_vec
                .iter()
                .zip(encoded_folded_message.iter())
                .enumerate()
            {
                assert_eq!(a, b, "Failed at index {} in round {}", i, round);
            }
            folded_codeword = FieldType::Ext(folded_codeword_vec);
        }
    }
}
