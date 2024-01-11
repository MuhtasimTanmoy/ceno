use crate::util::merkle_tree::{MerklePathWithoutLeafOrRoot, MerkleTree};
use crate::util::{field_to_usize, u32_to_field};
use crate::{
    multilinear::validate_input,
    poly::{multilinear::MultilinearPolynomial, Polynomial},
    util::{
        arithmetic::{horner, inner_product, steps, PrimeField},
        expression::{Expression, Query, Rotation},
        hash::{Hash, Output},
        log2_strict,
        transcript::{TranscriptRead, TranscriptWrite},
        Deserialize, DeserializeOwned, Itertools, Serialize,
    },
    AdditiveCommitment, Error, Evaluation, Point, PolynomialCommitmentScheme,
};
use crate::{
    sum_check::{
        classic::{ClassicSumCheck, CoefficientsProver},
        eq_xy_eval, SumCheck as _, VirtualPolynomial,
    },
    util::num_of_bytes,
};
use aes::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};
use core::fmt::Debug;
use ctr;
use ff::BatchInverter;
use generic_array::GenericArray;
use std::{ops::Deref, time::Instant};

use multilinear_extensions::virtual_poly::build_eq_x_r_vec;

use crate::util::plonky2_util::{reverse_bits, reverse_index_bits_in_place};
use rand_chacha::{
    rand_core::{RngCore, SeedableRng},
    ChaCha8Rng,
};
use rayon::prelude::{
    IndexedParallelIterator, IntoParallelRefIterator, IntoParallelRefMutIterator, ParallelIterator,
    ParallelSlice, ParallelSliceMut,
};
use std::{borrow::Cow, marker::PhantomData, slice};
type SumCheck<F> = ClassicSumCheck<CoefficientsProver<F>>;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BasefoldParams<F: PrimeField> {
    log_rate: usize,
    num_verifier_queries: usize,
    max_num_vars: usize,
    table_w_weights: Vec<Vec<(F, F)>>,
    table: Vec<Vec<F>>,
    rng: ChaCha8Rng,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BasefoldProverParams<F: PrimeField> {
    log_rate: usize,
    table_w_weights: Vec<Vec<(F, F)>>,
    table: Vec<Vec<F>>,
    num_verifier_queries: usize,
    max_num_vars: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BasefoldVerifierParams<F: PrimeField> {
    rng: ChaCha8Rng,
    max_num_vars: usize,
    log_rate: usize,
    num_verifier_queries: usize,
    table_w_weights: Vec<Vec<(F, F)>>,
}

/// A polynomial commitment together with all the data (e.g., the codeword, and Merkle tree)
/// used to generate this commitment and for assistant in opening
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(bound(serialize = "F: Serialize", deserialize = "F: DeserializeOwned"))]
pub struct BasefoldCommitmentWithData<F, H: Hash> {
    codeword_tree: MerkleTree<F, H>,
    bh_evals: Vec<F>,
    num_vars: usize,
}

impl<F: PrimeField, H: Hash> BasefoldCommitmentWithData<F, H> {
    pub fn to_commitment(&self) -> BasefoldCommitment<H> {
        BasefoldCommitment::new(self.codeword_tree.root(), self.num_vars)
    }

    pub fn get_root_ref(&self) -> &Output<H> {
        self.codeword_tree.root_ref()
    }

    pub fn get_codeword(&self) -> &Vec<F> {
        self.codeword_tree.leaves()
    }

    pub fn codeword_size(&self) -> usize {
        self.codeword_tree.size()
    }

    pub fn codeword_size_log(&self) -> usize {
        self.codeword_tree.height()
    }

    pub fn poly_size(&self) -> usize {
        self.bh_evals.len()
    }

    pub fn get_codeword_entry(&self, index: usize) -> &F {
        self.codeword_tree.get_leaf(index)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(bound(serialize = "", deserialize = ""))]
pub struct BasefoldCommitment<H: Hash> {
    root: Output<H>,
    num_vars: Option<usize>,
}

impl<H: Hash> BasefoldCommitment<H> {
    fn new(root: Output<H>, num_vars: usize) -> Self {
        Self {
            root,
            num_vars: Some(num_vars),
        }
    }

    fn root(&self) -> Output<H> {
        self.root.clone()
    }

    fn num_vars(&self) -> Option<usize> {
        self.num_vars
    }
}

impl<F: PrimeField, H: Hash> PartialEq for BasefoldCommitmentWithData<F, H> {
    fn eq(&self, other: &Self) -> bool {
        self.get_codeword().eq(other.get_codeword()) && self.bh_evals.eq(&other.bh_evals)
    }
}

impl<F: PrimeField, H: Hash> Eq for BasefoldCommitmentWithData<F, H> {}

pub trait BasefoldExtParams: Debug {
    fn get_reps() -> usize;

    fn get_rate() -> usize;

    fn get_basecode() -> usize;
}

#[derive(Debug)]
pub struct Basefold<F: PrimeField, H: Hash, V: BasefoldExtParams>(PhantomData<(F, H, V)>);

impl<F: PrimeField, H: Hash, V: BasefoldExtParams> Clone for Basefold<F, H, V> {
    fn clone(&self) -> Self {
        Self(PhantomData)
    }
}

impl<H: Hash> AsRef<[Output<H>]> for BasefoldCommitment<H> {
    fn as_ref(&self) -> &[Output<H>] {
        let root = &self.root;
        slice::from_ref(root)
    }
}

impl<F: PrimeField, H: Hash> AsRef<[Output<H>]> for BasefoldCommitmentWithData<F, H> {
    fn as_ref(&self) -> &[Output<H>] {
        let root = self.get_root_ref();
        slice::from_ref(root)
    }
}

impl<F: PrimeField, H: Hash> AdditiveCommitment<F> for BasefoldCommitmentWithData<F, H> {
    fn sum_with_scalar<'a>(
        scalars: impl IntoIterator<Item = &'a F> + 'a,
        bases: impl IntoIterator<Item = &'a Self> + 'a,
    ) -> Self {
        let bases = bases.into_iter().collect_vec();

        let scalars = scalars.into_iter().collect_vec();
        let bases = bases.into_iter().collect_vec();
        let k = bases[0].bh_evals.len();
        let num_vars = log2_strict(k);

        let mut new_codeword = vec![F::ZERO; bases[0].codeword_size()];
        new_codeword.par_iter_mut().enumerate().for_each(|(i, c)| {
            for j in 0..bases.len() {
                *c += *scalars[j] * bases[j].get_codeword_entry(i);
            }
        });

        let mut new_bh_eval = vec![F::ZERO; k];
        new_bh_eval.par_iter_mut().enumerate().for_each(|(i, c)| {
            for j in 0..bases.len() {
                *c += *scalars[j] * bases[j].bh_evals[i];
            }
        });

        let tree = MerkleTree::<F, H>::from_leaves(new_codeword);

        Self {
            bh_evals: Vec::new(),
            codeword_tree: tree,
            num_vars,
        }
    }
}

impl<F, H, V> PolynomialCommitmentScheme<F> for Basefold<F, H, V>
where
    F: PrimeField + Serialize + DeserializeOwned,
    H: Hash,
    V: BasefoldExtParams,
{
    type Param = BasefoldParams<F>;
    type ProverParam = BasefoldProverParams<F>;
    type VerifierParam = BasefoldVerifierParams<F>;
    type Polynomial = MultilinearPolynomial<F>;
    type CommitmentWithData = BasefoldCommitmentWithData<F, H>;
    type Commitment = BasefoldCommitment<H>;
    type CommitmentChunk = Output<H>;

    fn setup(poly_size: usize, _: usize, _: impl RngCore) -> Result<Self::Param, Error> {
        let log_rate = V::get_rate();
        let mut test_rng = ChaCha8Rng::from_entropy();
        let (table_w_weights, table) = get_table_aes(poly_size, log_rate, &mut test_rng);

        Ok(BasefoldParams {
            log_rate,
            num_verifier_queries: V::get_reps(),
            max_num_vars: log2_strict(poly_size),
            table_w_weights,
            table,
            rng: test_rng.clone(),
        })
    }

    fn trim(param: &Self::Param) -> Result<(Self::ProverParam, Self::VerifierParam), Error> {
        Ok((
            BasefoldProverParams {
                log_rate: param.log_rate,
                table_w_weights: param.table_w_weights.clone(),
                table: param.table.clone(),
                num_verifier_queries: param.num_verifier_queries,
                max_num_vars: param.max_num_vars,
            },
            BasefoldVerifierParams {
                rng: param.rng.clone(),
                max_num_vars: param.max_num_vars,
                log_rate: param.log_rate,
                num_verifier_queries: param.num_verifier_queries,
                // Why not trim the weights using poly_size? And is the verifier really
                // able to hold all these weights?
                table_w_weights: param.table_w_weights.clone(),
            },
        ))
    }

    fn commit(
        pp: &Self::ProverParam,
        poly: &Self::Polynomial,
    ) -> Result<Self::CommitmentWithData, Error> {
        // bh_evals is just a copy of poly.evals().
        // Note that this function implicitly assumes that the size of poly.evals() is a
        // power of two. Otherwise, the function crashes with index out of bound.
        let mut bh_evals = poly.evals().to_vec();
        let coeffs = interpolate_over_boolean_hypercube(&poly.evals().to_vec());

        let num_vars = log2_strict(bh_evals.len());

        // Split the input into chunks of message size, encode each message, and return the codewords
        let basecode = encode_rs_basecode(&coeffs, 1 << pp.log_rate, 1 << V::get_basecode());

        // Apply the recursive definition of the BaseFold code to the list of base codewords,
        // and produce the final codeword
        let mut codeword = evaluate_over_foldable_domain_generic_basecode(
            1 << V::get_basecode(),
            coeffs.len(),
            pp.log_rate,
            basecode,
            &pp.table,
        );

        // If using repetition code as basecode, it may be faster to use the following line of code to create the commitment and comment out the two lines above
        //        let mut codeword = evaluate_over_foldable_domain(pp.log_rate, coeffs, &pp.table);

        // The sum-check protocol starts from the first variable, but the FRI part
        // will eventually produce the evaluation at (alpha_k, ..., alpha_1), so apply
        // the bit-reversion to reverse the variable indices of the polynomial.
        // In short: store the poly and codeword in big endian
        reverse_index_bits_in_place(&mut bh_evals);
        reverse_index_bits_in_place(&mut codeword);

        // Compute and store all the layers of the Merkle tree
        let codeword_tree = MerkleTree::<F, H>::from_leaves(codeword);

        Ok(Self::CommitmentWithData {
            codeword_tree,
            bh_evals,
            num_vars,
        })
    }

    fn batch_commit_and_write<'a>(
        pp: &Self::ProverParam,
        polys: impl IntoIterator<Item = &'a Self::Polynomial>,
        transcript: &mut impl TranscriptWrite<Self::CommitmentChunk, F>,
    ) -> Result<Vec<Self::CommitmentWithData>, Error>
    where
        Self::Polynomial: 'a,
    {
        let comms = Self::batch_commit(pp, polys)?;
        comms.iter().for_each(|comm| {
            transcript.write_commitment(comm.get_root_ref()).unwrap();
            transcript
                .write_field_element(&u32_to_field(comm.num_vars as u32))
                .unwrap();
        });
        Ok(comms)
    }

    fn batch_commit<'a>(
        pp: &Self::ProverParam,
        polys: impl IntoIterator<Item = &'a Self::Polynomial>,
    ) -> Result<Vec<Self::CommitmentWithData>, Error> {
        let polys_vec: Vec<&Self::Polynomial> = polys.into_iter().map(|poly| poly).collect();
        polys_vec
            .par_iter()
            .map(|poly| Self::commit(pp, poly))
            .collect()
    }

    fn open(
        pp: &Self::ProverParam,
        poly: &Self::Polynomial,
        comm: &Self::CommitmentWithData,
        point: &Point<F, Self::Polynomial>,
        _eval: &F, // Opening does not need eval, except for sanity check
        transcript: &mut impl TranscriptWrite<Self::CommitmentChunk, F>,
    ) -> Result<(), Error> {
        assert!(comm.num_vars >= V::get_basecode());
        let (trees, oracles) = commit_phase(
            &point,
            &comm,
            transcript,
            poly.num_vars(),
            poly.num_vars() - V::get_basecode(),
            &pp.table_w_weights,
            pp.log_rate,
        );

        // Each entry in queried_els stores a list of triples (F, F, i) indicating the
        // position opened at each round and the two values at that round
        let queries = query_phase(transcript, &comm, &oracles, pp.num_verifier_queries);

        let queries_with_merkle_path =
            QueriesResultWithMerklePath::from_query_result(queries, &trees, comm);

        queries_with_merkle_path.write_transcript(transcript);

        Ok(())
    }

    fn batch_open<'a>(
        pp: &Self::ProverParam,
        polys: impl IntoIterator<Item = &'a Self::Polynomial>,
        comms: impl IntoIterator<Item = &'a Self::CommitmentWithData>,
        points: &[Point<F, Self::Polynomial>],
        evals: &[Evaluation<F>],
        transcript: &mut impl TranscriptWrite<Self::CommitmentChunk, F>,
    ) -> Result<(), Error> {
        let polys = polys.into_iter().collect_vec();
        let comms = comms.into_iter().collect_vec();
        let min_num_vars = polys.iter().map(|p| p.num_vars()).min().unwrap();
        assert!(min_num_vars >= V::get_basecode());

        if cfg!(feature = "sanity-check") {
            evals.iter().for_each(|eval| {
                assert_eq!(
                    &polys[eval.poly()].evaluate(&points[eval.point()]),
                    eval.value(),
                )
            })
        }

        validate_input("batch open", pp.max_num_vars, polys.clone(), points)?;

        // evals.len() is the batch size, i.e., how many polynomials are being opened together
        let batch_size_log = evals.len().next_power_of_two().ilog2() as usize;
        let t = transcript.squeeze_challenges(batch_size_log);

        // Use eq(X,t) where t is random to batch the different evaluation queries.
        // Note that this is a small polynomial (only batch_size) compared to the polynomials
        // to open.
        let eq_xt = Self::Polynomial::eq_xy(&t);
        // Merge the polynomials for every point. One merged polynomial for each point.
        let merged_polys = evals.iter().zip(eq_xt.evals().iter()).fold(
            // This folding will generate a vector of |points| pairs of (scalar, polynomial)
            // The polynomials are initialized to zero, and the scalars are initialized to one
            vec![(F::ONE, Cow::<Self::Polynomial>::default()); points.len()],
            |mut merged_polys, (eval, eq_xt_i)| {
                // For each polynomial to open, eval.point() specifies which point it is to be opened at.
                if merged_polys[eval.point()].1.is_zero() {
                    // If the accumulator for this point is still the zero polynomial,
                    // directly assign the random coefficient and the polynomial to open to
                    // this accumulator
                    merged_polys[eval.point()] = (*eq_xt_i, Cow::Borrowed(polys[eval.poly()]));
                } else {
                    // If the accumulator is unempty now, first force its scalar to 1, i.e.,
                    // make (scalar, polynomial) to (1, scalar * polynomial)
                    let coeff = merged_polys[eval.point()].0;
                    if coeff != F::ONE {
                        merged_polys[eval.point()].0 = F::ONE;
                        *merged_polys[eval.point()].1.to_mut() *= &coeff;
                    }
                    // Equivalent to merged_poly += poly * batch_coeff. Note that
                    // add_assign_mixed_with_coeff allows adding two polynomials with
                    // different variables, and the result has the same number of vars
                    // with the larger one of the two added polynomials.
                    (*merged_polys[eval.point()].1.to_mut())
                        .add_assign_mixed_with_coeff(polys[eval.poly()], eq_xt_i);

                    // Note that once the scalar in the accumulator becomes ONE, it will remain
                    // to be ONE forever.
                }
                merged_polys
            },
        );

        let mut points = points.to_vec();
        // Note that merged_polys may contain polynomials of different number of variables.
        // Resize the evaluation points so that the size match.
        merged_polys.iter().enumerate().for_each(|(i, (_, poly))| {
            assert!(points[i].len() >= poly.num_vars());
            points[i].resize(poly.num_vars(), F::ZERO)
        });

        let expression = merged_polys
            .iter()
            .enumerate()
            .map(|(idx, (scalar, _))| {
                Expression::<F>::eq_xy(idx)
                    * Expression::Polynomial(Query::new(idx, Rotation::cur()))
                    * scalar
            })
            .sum();
        let sumcheck_polys: Vec<&MultilinearPolynomial<F>> = merged_polys
            .iter()
            .map(|(_, poly)| poly.deref())
            .collect_vec();
        let virtual_poly =
            VirtualPolynomial::new(&expression, sumcheck_polys, &[], points.as_slice());
        // virtual_poly is a polynomial expression that may also involve polynomials with different
        // number of variables. Use the maximal number of variables in the sum-check.
        let num_vars = merged_polys
            .iter()
            .map(|(_, poly)| poly.num_vars())
            .max()
            .unwrap();
        let target_sum = inner_product(evals.iter().map(Evaluation::value), &eq_xt[..evals.len()]);

        if cfg!(feature = "sanity-check") {
            let expected_sum = merged_polys
                .iter()
                .zip(&points)
                .map(|((scalar, poly), point)| {
                    inner_product(poly.evals(), MultilinearPolynomial::eq_xy(&point).evals())
                        * scalar
                        * F::from(1 << (num_vars - poly.num_vars()))
                    // When this polynomial is smaller, it will be repeatedly summed over the cosets of the hypercube
                })
                .sum::<F>();
            assert_eq!(expected_sum, target_sum);
        }

        let (challenges, merged_poly_evals) =
            SumCheck::prove(&(), num_vars, virtual_poly, target_sum, transcript)?;

        // Now the verifier has obtained the new target sum, and is able to compute the random
        // linear coefficients, and is able to evaluate eq_xy(point) for each poly to open.
        // The remaining tasks for the prover is to prove that
        // sum_i coeffs[i] poly_evals[i] is equal to
        // the new target sum, where coeffs is computed as follows
        let eq_xy_evals = points
            .iter()
            .map(|point| eq_xy_eval(&challenges, point))
            .collect_vec();
        let mut coeffs = vec![F::ZERO; comms.len()];
        evals.iter().enumerate().for_each(|(i, eval)| {
            coeffs[eval.poly()] += eq_xy_evals[eval.point()] * eq_xt[i];
        });

        if cfg!(feature = "sanity-check") {
            let poly_evals = polys
                .iter()
                .map(|poly| poly.evaluate(&challenges))
                .collect_vec();
            let new_target_sum = inner_product(&poly_evals, &coeffs);
            let desired_sum = merged_polys
                .iter()
                .zip(points)
                .zip(merged_poly_evals)
                .map(|(((scalar, poly), point), evals_from_sum_check)| {
                    assert_eq!(evals_from_sum_check, poly.evaluate(&challenges));
                    *scalar
                        * evals_from_sum_check
                        * &eq_xy_eval(point.as_slice(), &challenges[0..point.len()])
                })
                .sum::<F>();
            assert_eq!(new_target_sum, desired_sum);
        }
        // Note that the verifier can also compute these coeffs locally, so no need to pass
        // them to the transcript.

        let point = challenges;

        let (trees, oracles) = batch_commit_phase(
            &point,
            comms.as_slice(),
            transcript,
            num_vars,
            num_vars - V::get_basecode(),
            &pp.table_w_weights,
            pp.log_rate,
            coeffs.as_slice(),
        );

        let query_result = batch_query_phase(
            transcript,
            1 << (num_vars + pp.log_rate),
            comms.as_slice(),
            &oracles,
            pp.num_verifier_queries,
        );

        let query_result_with_merkle_path =
            BatchedQueriesResultWithMerklePath::from_batched_query_result(
                query_result,
                &trees,
                &comms,
            );

        query_result_with_merkle_path.write_transcript(transcript);

        Ok(())
    }

    fn read_commitments(
        _: &Self::VerifierParam,
        num_polys: usize,
        transcript: &mut impl TranscriptRead<Self::CommitmentChunk, F>,
    ) -> Result<Vec<Self::Commitment>, Error> {
        let roots = (0..num_polys)
            .map(|_| {
                let commitment = transcript.read_commitment().unwrap();
                let num_vars = field_to_usize(&transcript.read_field_element().unwrap(), None);
                (num_vars, commitment)
            })
            .collect_vec();

        Ok(roots
            .iter()
            .map(|(num_vars, commitment)| BasefoldCommitment::new(commitment.clone(), *num_vars))
            .collect_vec())
    }

    fn commit_and_write(
        pp: &Self::ProverParam,
        poly: &Self::Polynomial,
        transcript: &mut impl TranscriptWrite<Self::CommitmentChunk, F>,
    ) -> Result<Self::CommitmentWithData, Error> {
        let comm = Self::commit(pp, poly)?;

        transcript.write_commitments(comm.as_ref())?;
        transcript.write_field_element(&u32_to_field::<F>(comm.num_vars as u32))?;

        Ok(comm)
    }

    fn verify(
        vp: &Self::VerifierParam,
        comm: &Self::Commitment,
        point: &Point<F, Self::Polynomial>,
        eval: &F,
        transcript: &mut impl TranscriptRead<Self::CommitmentChunk, F>,
    ) -> Result<(), Error> {
        assert!(comm.num_vars().unwrap() >= V::get_basecode());

        let _field_size = 255;
        let num_vars = point.len();
        let num_rounds = num_vars - V::get_basecode();

        let mut fold_challenges: Vec<F> = Vec::with_capacity(vp.max_num_vars);
        let _size = 0;
        let mut roots = Vec::new();
        let mut sumcheck_messages = Vec::with_capacity(num_rounds);
        for i in 0..num_rounds {
            sumcheck_messages.push(transcript.read_field_elements(3).unwrap());
            fold_challenges.push(transcript.squeeze_challenge());
            if i < num_rounds - 1 {
                roots.push(transcript.read_commitment().unwrap());
            }
        }
        let final_message = transcript
            .read_field_elements(1 << V::get_basecode())
            .unwrap();
        let query_challenges = transcript
            .squeeze_challenges(vp.num_verifier_queries)
            .iter()
            .map(|index| field_to_usize(index, Some(1 << (num_vars + vp.log_rate))))
            .collect_vec();
        let query_result_with_merkle_path = QueriesResultWithMerklePath::read_transcript(
            transcript,
            num_rounds,
            vp.log_rate,
            num_vars,
            query_challenges.as_slice(),
        );

        // coeff is the eq polynomial evaluated at the last challenge.len() variables
        // in reverse order.
        let rev_challenges = fold_challenges.clone().into_iter().rev().collect_vec();
        let coeff = eq_xy_eval(
            &point.as_slice()[point.len() - fold_challenges.len()..],
            &rev_challenges,
        );
        // Compute eq as the partially evaluated eq polynomial
        let mut eq = build_eq_x_r_vec(&point.as_slice()[..point.len() - fold_challenges.len()]);
        eq.par_iter_mut().for_each(|e| *e *= coeff);

        verifier_query_phase::<F, H>(
            &query_result_with_merkle_path,
            &sumcheck_messages,
            &fold_challenges,
            num_rounds,
            num_vars,
            vp.log_rate,
            &final_message,
            &roots,
            comm,
            eq.as_slice(),
            vp.rng.clone(),
            &eval,
        );

        Ok(())
    }

    fn batch_verify<'a>(
        vp: &Self::VerifierParam,
        comms: impl IntoIterator<Item = &'a Self::Commitment>,
        points: &[Point<F, Self::Polynomial>],
        evals: &[Evaluation<F>],
        transcript: &mut impl TranscriptRead<Self::CommitmentChunk, F>,
    ) -> Result<(), Error> {
        //	let key = "RAYON_NUM_THREADS";
        //	env::set_var(key, "32");
        let comms = comms.into_iter().collect_vec();
        let num_vars = points.iter().map(|point| point.len()).max().unwrap();
        let num_rounds = num_vars - V::get_basecode();
        validate_input("batch verify", vp.max_num_vars, [], points)?;
        let poly_num_vars = comms.iter().map(|c| c.num_vars().unwrap()).collect_vec();
        if cfg!(feature = "sanity-check") {
            evals.iter().for_each(|eval| {
                assert_eq!(
                    points[eval.point()].len(),
                    comms[eval.poly()].num_vars().unwrap()
                );
            });
        }
        assert!(poly_num_vars.iter().min().unwrap() >= &V::get_basecode());

        let batch_size_log = evals.len().next_power_of_two().ilog2() as usize;
        let t = transcript.squeeze_challenges(batch_size_log);

        let eq_xt = MultilinearPolynomial::eq_xy(&t);
        let tilde_gs_sum =
            inner_product(evals.iter().map(Evaluation::value), &eq_xt[..evals.len()]);

        let (new_target_sum, verify_point) =
            SumCheck::verify(&(), num_vars, 2, tilde_gs_sum, transcript)?;

        // Now the goal is to use the BaseFold to check the new target sum. Note that this time
        // we only have one eq polynomial in the sum-check.
        let eq_xy_evals = points
            .iter()
            .map(|point| eq_xy_eval(&verify_point, point))
            .collect_vec();
        let mut coeffs = vec![F::ZERO; comms.len()];
        evals
            .iter()
            .enumerate()
            .for_each(|(i, eval)| coeffs[eval.poly()] += eq_xy_evals[eval.point()] * eq_xt[i]);

        //start of verify
        //read first $(num_var - 1) commitments
        let mut sumcheck_messages = Vec::with_capacity(num_rounds);
        let mut roots: Vec<Output<H>> = Vec::with_capacity(num_rounds - 1);
        let mut fold_challenges: Vec<F> = Vec::with_capacity(num_rounds);
        for i in 0..num_rounds {
            sumcheck_messages.push(transcript.read_field_elements(3).unwrap());
            fold_challenges.push(transcript.squeeze_challenge());
            if i < num_rounds - 1 {
                roots.push(transcript.read_commitment().unwrap());
            }
        }
        let final_message = transcript
            .read_field_elements(1 << V::get_basecode())
            .unwrap();

        let query_challenges = transcript
            .squeeze_challenges(vp.num_verifier_queries)
            .iter()
            .map(|index| field_to_usize(index, Some(1 << (num_vars + vp.log_rate))))
            .collect_vec();

        let query_result_with_merkle_path = BatchedQueriesResultWithMerklePath::read_transcript(
            transcript,
            num_rounds,
            vp.log_rate,
            poly_num_vars.as_slice(),
            query_challenges.as_slice(),
        );

        batch_verifier_query_phase::<F, H>(
            &query_result_with_merkle_path,
            &sumcheck_messages,
            &fold_challenges,
            num_rounds,
            num_vars,
            vp.log_rate,
            &final_message,
            &roots,
            &comms,
            &coeffs,
            vp.rng.clone(),
            &new_target_sum,
        );
        Ok(())
    }
}

// Split the input into chunks of message size, encode each message, and return the codewords
fn encode_rs_basecode<F: PrimeField>(
    poly: &Vec<F>,
    rate: usize,
    message_size: usize,
) -> Vec<Vec<F>> {
    // The domain is just counting 1, 2, 3, ... , domain_size
    let domain: Vec<F> = steps(F::ONE).take(message_size * rate).collect();
    let res = poly
        .par_chunks_exact(message_size)
        .map(|chunk| {
            let mut target = vec![F::ZERO; message_size * rate];
            // Just Reed-Solomon code, but with the naive domain
            target
                .iter_mut()
                .enumerate()
                .for_each(|(i, target)| *target = horner(&chunk[..], &domain[i]));
            target
        })
        .collect::<Vec<Vec<F>>>();

    res
}

#[allow(unused)]
fn encode_repetition_basecode<F: PrimeField>(poly: &Vec<F>, rate: usize) -> Vec<Vec<F>> {
    let mut base_codewords = Vec::new();
    for c in poly {
        let mut rep_code = Vec::new();
        for i in 0..rate {
            rep_code.push(*c);
        }
        base_codewords.push(rep_code);
    }
    return base_codewords;
}

//this function assumes all codewords in base_codeword has equivalent length
pub fn evaluate_over_foldable_domain_generic_basecode<F: PrimeField>(
    base_message_length: usize,
    num_coeffs: usize,
    log_rate: usize,
    base_codewords: Vec<Vec<F>>,
    table: &Vec<Vec<F>>,
) -> Vec<F> {
    let k = num_coeffs;
    let logk = log2_strict(k);
    let base_log_k = log2_strict(base_message_length);
    //concatenate together all base codewords
    //    let now = Instant::now();
    let mut coeffs_with_bc: Vec<F> = base_codewords.iter().flatten().map(|x| *x).collect();
    //    println!("concatenate base codewords {:?}", now.elapsed());
    //iterate over array, replacing even indices with (evals[i] - evals[(i+1)])
    let mut chunk_size = base_codewords[0].len(); //block length of the base code
    for i in base_log_k..logk {
        // In beginning of each iteration, the current codeword size is 1<<i, after this iteration,
        // every two adjacent codewords are folded into one codeword of size 1<<(i+1).
        // Fetch the table that has the same size of the *current* codeword size.
        let level = &table[i + log_rate];
        // chunk_size is equal to 1 << (i+1), i.e., the codeword size after the current iteration
        // half_chunk is equal to 1 << i, i.e. the current codeword size
        chunk_size = chunk_size << 1;
        assert_eq!(level.len(), chunk_size >> 1);
        <Vec<F> as AsMut<[F]>>::as_mut(&mut coeffs_with_bc)
            .par_chunks_mut(chunk_size)
            .for_each(|chunk| {
                let half_chunk = chunk_size >> 1;
                for j in half_chunk..chunk_size {
                    // Suppose the current codewords are (a, b)
                    // The new codeword is computed by two halves:
                    // left  = a + t * b
                    // right = a - t * b
                    let rhs = chunk[j] * level[j - half_chunk];
                    chunk[j] = chunk[j - half_chunk] - rhs;
                    chunk[j - half_chunk] = chunk[j - half_chunk] + rhs;
                }
            });
    }
    coeffs_with_bc
}

#[allow(unused)]
pub fn evaluate_over_foldable_domain<F: PrimeField>(
    log_rate: usize,
    mut coeffs: Vec<F>,
    table: &Vec<Vec<F>>,
) -> Vec<F> {
    //iterate over array, replacing even indices with (evals[i] - evals[(i+1)])
    let k = coeffs.len();
    let logk = log2_strict(k);
    let cl = 1 << (logk + log_rate);
    let rate = 1 << log_rate;
    let mut coeffs_with_rep = Vec::with_capacity(cl);
    for i in 0..cl {
        coeffs_with_rep.push(F::ZERO);
    }

    //base code - in this case is the repetition code
    let now = Instant::now();
    for i in 0..k {
        for j in 0..rate {
            coeffs_with_rep[i * rate + j] = coeffs[i];
        }
    }

    let mut chunk_size = rate; //block length of the base code
    for i in 0..logk {
        let level = &table[i + log_rate];
        chunk_size = chunk_size << 1;
        assert_eq!(level.len(), chunk_size >> 1);
        <Vec<F> as AsMut<[F]>>::as_mut(&mut coeffs_with_rep)
            .par_chunks_mut(chunk_size)
            .for_each(|chunk| {
                let half_chunk = chunk_size >> 1;
                for j in half_chunk..chunk_size {
                    let rhs = chunk[j] * level[j - half_chunk];
                    chunk[j] = chunk[j - half_chunk] - rhs;
                    chunk[j - half_chunk] = chunk[j - half_chunk] + rhs;
                }
            });
    }
    coeffs_with_rep
}

fn interpolate_over_boolean_hypercube<F: PrimeField>(evals: &Vec<F>) -> Vec<F> {
    //iterate over array, replacing even indices with (evals[i] - evals[(i+1)])
    let n = log2_strict(evals.len());
    let mut coeffs = vec![F::ZERO; evals.len()];

    let mut j = 0;
    while j < coeffs.len() {
        coeffs[j + 1] = evals[j + 1] - evals[j];
        coeffs[j] = evals[j];
        j += 2
    }

    // This code implicitly assumes that coeffs has size at least 1 << n,
    // that means the size of evals should be a power of two
    for i in 2..n + 1 {
        let chunk_size = 1 << i;
        coeffs.par_chunks_mut(chunk_size).for_each(|chunk| {
            let half_chunk = chunk_size >> 1;
            for j in half_chunk..chunk_size {
                chunk[j] = chunk[j] - chunk[j - half_chunk];
            }
        });
    }

    coeffs
}

fn sum_check_first_round<F: PrimeField>(mut eq: &mut Vec<F>, mut bh_values: &mut Vec<F>) -> Vec<F> {
    // The input polynomials are in the form of evaluations. Instead of viewing
    // every one element as the evaluation of the polynomial at a single point,
    // we can view every two elements as partially evaluating the polynomial at
    // a single point, leaving the first variable free, and obtaining a univariate
    // polynomial. The one_level_interp_hc transforms the evaluation forms into
    // the coefficient forms, for every of these partial polynomials.
    one_level_interp_hc(&mut eq);
    one_level_interp_hc(&mut bh_values);
    parallel_pi(&bh_values, &eq)
    //    p_i(&bh_values, &eq)
}

pub fn one_level_interp_hc<F: PrimeField>(evals: &mut Vec<F>) {
    if evals.len() == 1 {
        return;
    }
    evals.par_chunks_mut(2).for_each(|chunk| {
        chunk[1] = chunk[1] - chunk[0];
    });
}

pub fn one_level_eval_hc<F: PrimeField>(evals: &mut Vec<F>, challenge: F) {
    evals.par_chunks_mut(2).for_each(|chunk| {
        chunk[1] = chunk[0] + challenge * chunk[1];
    });

    // Skip every one other element
    let mut index = 0;
    evals.retain(|_| {
        index += 1;
        (index - 1) % 2 == 1
    });
}

fn parallel_pi<F: PrimeField>(evals: &Vec<F>, eq: &Vec<F>) -> Vec<F> {
    if evals.len() == 1 {
        return vec![evals[0], evals[0], evals[0]];
    }
    let mut coeffs = vec![F::ZERO, F::ZERO, F::ZERO];

    // Manually write down the multiplication formular of two linear polynomials
    let mut firsts = vec![F::ZERO; evals.len()];
    firsts.par_iter_mut().enumerate().for_each(|(i, f)| {
        if i % 2 == 0 {
            *f = evals[i] * eq[i];
        }
    });

    let mut seconds = vec![F::ZERO; evals.len()];
    seconds.par_iter_mut().enumerate().for_each(|(i, f)| {
        if i % 2 == 0 {
            *f = evals[i + 1] * eq[i] + evals[i] * eq[i + 1];
        }
    });

    let mut thirds = vec![F::ZERO; evals.len()];
    thirds.par_iter_mut().enumerate().for_each(|(i, f)| {
        if i % 2 == 0 {
            *f = evals[i + 1] * eq[i + 1];
        }
    });

    coeffs[0] = firsts.par_iter().sum();
    coeffs[1] = seconds.par_iter().sum();
    coeffs[2] = thirds.par_iter().sum();

    coeffs
}

fn sum_check_challenge_round<F: PrimeField>(
    mut eq: &mut Vec<F>,
    mut bh_values: &mut Vec<F>,
    challenge: F,
) -> Vec<F> {
    // Note that when the last round ends, every two elements are in
    // the coefficient form. Use the challenge to reduce the two elements
    // into a single value. This is equivalent to substituting the challenge
    // to the first variable of the poly.
    one_level_eval_hc(&mut bh_values, challenge);
    one_level_eval_hc(&mut eq, challenge);

    one_level_interp_hc(&mut eq);
    one_level_interp_hc(&mut bh_values);

    parallel_pi(&bh_values, &eq)
    // p_i(&bh_values,&eq)
}

fn sum_check_last_round<F: PrimeField>(
    mut eq: &mut Vec<F>,
    mut bh_values: &mut Vec<F>,
    challenge: F,
) {
    one_level_eval_hc(&mut bh_values, challenge);
    one_level_eval_hc(&mut eq, challenge);
}

fn basefold_one_round_by_interpolation_weights<F: PrimeField>(
    table: &Vec<Vec<(F, F)>>,
    level_index: usize,
    values: &Vec<F>,
    challenge: F,
) -> Vec<F> {
    let level = &table[level_index];
    values
        .par_chunks_exact(2)
        .enumerate()
        .map(|(i, ys)| {
            interpolate2_weights::<F>(
                [(level[i].0, ys[0]), (-(level[i].0), ys[1])],
                level[i].1,
                challenge,
            )
        })
        .collect::<Vec<_>>()
}

fn basefold_get_query<F: PrimeField>(
    poly_codeword: &Vec<F>,
    oracles: &Vec<Vec<F>>,
    x_index: usize,
) -> SingleQueryResult<F> {
    let mut index = x_index;
    let p1 = index | 1;
    let p0 = p1 - 1;

    let commitment_query = CodewordSingleQueryResult::new(poly_codeword[p0], poly_codeword[p1], p0);
    index >>= 1;

    let mut oracle_queries = Vec::with_capacity(oracles.len() + 1);
    for oracle in oracles {
        let p1 = index | 1;
        let p0 = p1 - 1;

        oracle_queries.push(CodewordSingleQueryResult::new(oracle[p0], oracle[p1], p0));
        index >>= 1;
    }

    let oracle_query = OracleListQueryResult {
        inner: oracle_queries,
    };

    return SingleQueryResult {
        oracle_query,
        commitment_query,
    };
}

fn batch_basefold_get_query<F: PrimeField, H: Hash>(
    comms: &[&BasefoldCommitmentWithData<F, H>],
    oracles: &Vec<Vec<F>>,
    codeword_size: usize,
    x_index: usize,
) -> BatchedSingleQueryResult<F> {
    let mut oracle_list_queries = Vec::with_capacity(oracles.len());

    let mut index = x_index;
    index >>= 1;
    for oracle in oracles {
        let p1 = index | 1;
        let p0 = p1 - 1;
        oracle_list_queries.push(CodewordSingleQueryResult::<F>::new(
            oracle[p0], oracle[p1], p0,
        ));
        index >>= 1;
    }
    let oracle_query = OracleListQueryResult {
        inner: oracle_list_queries,
    };

    let comm_queries = comms
        .iter()
        .map(|comm| {
            let x_index = x_index >> (log2_strict(codeword_size) - comm.codeword_size_log());
            let p1 = x_index | 1;
            let p0 = p1 - 1;
            CodewordSingleQueryResult::<F>::new(
                *comm.get_codeword_entry(p0),
                *comm.get_codeword_entry(p1),
                p0,
            )
        })
        .collect_vec();

    let commitments_query = CommitmentsQueryResult {
        inner: comm_queries,
    };

    BatchedSingleQueryResult {
        oracle_query,
        commitments_query,
    }
}

pub fn interpolate2_weights<F: PrimeField>(points: [(F, F); 2], weight: F, x: F) -> F {
    // a0 -> a1
    // b0 -> b1
    // x  -> a1 + (x-a0)*(b1-a1)/(b0-a0)
    let (a0, a1) = points[0];
    let (b0, b1) = points[1];
    if cfg!(feature = "sanity-check") {
        assert_ne!(a0, b0);
        assert_eq!(weight * (b0 - a0), F::ONE);
    }
    // Here weight = 1/(b0-a0). The reason for precomputing it is that inversion is expensive
    a1 + (x - a0) * (b1 - a1) * weight
}

pub fn query_point<F: PrimeField>(
    block_length: usize,
    eval_index: usize,
    level: usize,
    mut cipher: &mut ctr::Ctr32LE<aes::Aes128>,
) -> F {
    let level_index = eval_index % (block_length);
    let mut el =
        query_root_table_from_rng_aes::<F>(level, level_index % (block_length >> 1), &mut cipher);

    if level_index >= (block_length >> 1) {
        el = -F::ONE * el;
    }

    return el;
}

pub fn query_root_table_from_rng_aes<F: PrimeField>(
    level: usize,
    index: usize,
    cipher: &mut ctr::Ctr32LE<aes::Aes128>,
) -> F {
    let mut level_offset: u128 = 1;
    for lg_m in 1..=level {
        let half_m = 1 << (lg_m - 1);
        level_offset += half_m;
    }

    let pos = ((level_offset + (index as u128))
        * ((F::NUM_BITS as usize).next_power_of_two() as u128))
        .checked_div(8)
        .unwrap();

    cipher.seek(pos);

    let bytes = (F::NUM_BITS as usize).next_power_of_two() / 8;
    let mut dest: Vec<u8> = vec![0u8; bytes];
    cipher.apply_keystream(&mut dest);

    let res = from_raw_bytes::<F>(&dest);

    res
}

pub fn interpolate2<F: PrimeField>(points: [(F, F); 2], x: F) -> F {
    // a0 -> a1
    // b0 -> b1
    // x  -> a1 + (x-a0)*(b1-a1)/(b0-a0)
    let (a0, a1) = points[0];
    let (b0, b1) = points[1];
    assert_ne!(a0, b0);
    a1 + (x - a0) * (b1 - a1) * (b0 - a0).invert().unwrap()
}

fn degree_2_zero_plus_one<F: PrimeField>(poly: &Vec<F>) -> F {
    poly[0] + poly[0] + poly[1] + poly[2]
}

fn degree_2_eval<F: PrimeField>(poly: &Vec<F>, point: F) -> F {
    poly[0] + point * poly[1] + point * point * poly[2]
}

fn from_raw_bytes<F: PrimeField>(bytes: &Vec<u8>) -> F {
    let mut res = F::ZERO;
    bytes.into_iter().for_each(|b| {
        res += F::from(u64::from(*b));
    });
    res
}

//outputs (trees, sumcheck_oracles, oracles, bh_evals, eq, eval)
fn commit_phase<F: PrimeField, H: Hash>(
    point: &Point<F, MultilinearPolynomial<F>>,
    comm: &BasefoldCommitmentWithData<F, H>,
    transcript: &mut impl TranscriptWrite<Output<H>, F>,
    num_vars: usize,
    num_rounds: usize,
    table_w_weights: &Vec<Vec<(F, F)>>,
    log_rate: usize,
) -> (Vec<MerkleTree<F, H>>, Vec<Vec<F>>) {
    assert_eq!(point.len(), num_vars);
    let mut oracles = Vec::with_capacity(num_vars);
    let mut trees = Vec::with_capacity(num_vars);
    let mut running_oracle = comm.get_codeword().clone();
    let mut running_evals = comm.bh_evals.clone();

    // eq is the evaluation representation of the eq(X,r) polynomial over the hypercube
    let mut eq = build_eq_x_r_vec::<F>(&point);
    reverse_index_bits_in_place(&mut eq);
    let mut last_sumcheck_message = sum_check_first_round::<F>(&mut eq, &mut running_evals);

    for i in 0..num_rounds {
        // For the first round, no need to send the running root, because this root is
        // committing to a vector that can be recovered from linearly combining other
        // already-committed vectors.
        transcript
            .write_field_elements(&last_sumcheck_message)
            .unwrap();

        let challenge: F = transcript.squeeze_challenge();

        // Fold the current oracle for FRI
        running_oracle = basefold_one_round_by_interpolation_weights::<F>(
            &table_w_weights,
            log2_strict(running_oracle.len()) - 1,
            &running_oracle,
            challenge,
        );

        if i < num_rounds - 1 {
            last_sumcheck_message =
                sum_check_challenge_round(&mut eq, &mut running_evals, challenge);
            let running_tree = MerkleTree::<F, H>::from_leaves(running_oracle.clone());
            let running_root = running_tree.root();
            transcript.write_commitment(&running_root).unwrap();

            oracles.push(running_oracle.clone());
            trees.push(running_tree);
        } else {
            // The difference of the last round is that we don't need to compute the message,
            // and we don't interpolate the small polynomials. So after the last round,
            // running_evals is exactly the evaluation representation of the
            // folded polynomial so far.
            sum_check_last_round(&mut eq, &mut running_evals, challenge);
            // For the FRI part, we send the current polynomial as the message.
            // Transform it back into little endiean before sending it
            reverse_index_bits_in_place(&mut running_evals);
            transcript.write_field_elements(&running_evals).unwrap();

            if cfg!(feature = "sanity-check") {
                // If the prover is honest, in the last round, the running oracle
                // on the prover side should be exactly the encoding of the folded polynomial.

                let coeffs = interpolate_over_boolean_hypercube(&running_evals);
                let basecode = encode_rs_basecode(&coeffs, 1 << log_rate, coeffs.len());
                assert_eq!(basecode.len(), 1);
                let basecode = basecode[0].clone();

                reverse_index_bits_in_place(&mut running_oracle);
                assert_eq!(basecode, running_oracle);
            }
        }
    }
    return (trees, oracles);
}

//outputs (trees, sumcheck_oracles, oracles, bh_evals, eq, eval)
fn batch_commit_phase<F: PrimeField, H: Hash>(
    point: &Point<F, MultilinearPolynomial<F>>,
    comms: &[&BasefoldCommitmentWithData<F, H>],
    transcript: &mut impl TranscriptWrite<Output<H>, F>,
    num_vars: usize,
    num_rounds: usize,
    table_w_weights: &Vec<Vec<(F, F)>>,
    log_rate: usize,
    coeffs: &[F],
) -> (Vec<MerkleTree<F, H>>, Vec<Vec<F>>) {
    assert_eq!(point.len(), num_vars);
    let mut oracles = Vec::with_capacity(num_vars);
    let mut trees = Vec::with_capacity(num_vars);
    let mut running_oracle = vec![F::ZERO; 1 << (num_vars + log_rate)];

    // Before the interaction, collect all the polynomials whose num variables match the
    // max num variables
    let running_oracle_len = running_oracle.len();
    comms
        .iter()
        .enumerate()
        .filter(|(_, comm)| comm.codeword_size() == running_oracle_len)
        .for_each(|(index, comm)| {
            running_oracle
                .par_iter_mut()
                .zip_eq(comm.get_codeword().par_iter())
                .for_each(|(r, &a)| *r += a * coeffs[index]);
        });

    // Unlike the FRI part, the sum-check part still follows the original procedure,
    // and linearly combine all the polynomials once for all
    let mut sum_of_all_evals_for_sumcheck = vec![F::ZERO; 1 << num_vars];
    comms.iter().enumerate().for_each(|(index, comm)| {
        sum_of_all_evals_for_sumcheck
            .par_iter_mut()
            .enumerate()
            .for_each(|(pos, r)| {
                // Evaluating the multilinear polynomial outside of its interpolation hypercube
                // is equivalent to repeating each element in place.
                // Here is the tricky part: the bh_evals are stored in big endian, but we want
                // to align the polynomials to the variable with index 0 before adding them
                // together. So each element is repeated by
                // sum_of_all_evals_for_sumcheck.len() / bh_evals.len() times
                *r += comm.bh_evals[pos >> (num_vars - log2_strict(comm.bh_evals.len()))]
                    * coeffs[index]
            });
    });

    // eq is the evaluation representation of the eq(X,r) polynomial over the hypercube
    let mut eq = build_eq_x_r_vec::<F>(&point);
    reverse_index_bits_in_place(&mut eq);

    let mut sumcheck_messages = Vec::with_capacity(num_rounds + 1);
    let mut last_sumcheck_message =
        sum_check_first_round::<F>(&mut eq, &mut sum_of_all_evals_for_sumcheck);
    sumcheck_messages.push(last_sumcheck_message.clone());

    for i in 0..num_rounds {
        // For the first round, no need to send the running root, because this root is
        // committing to a vector that can be recovered from linearly combining other
        // already-committed vectors.
        transcript
            .write_field_elements(&last_sumcheck_message)
            .unwrap();

        let challenge: F = transcript.squeeze_challenge();

        // Fold the current oracle for FRI
        running_oracle = basefold_one_round_by_interpolation_weights::<F>(
            &table_w_weights,
            log2_strict(running_oracle.len()) - 1,
            &running_oracle,
            challenge,
        );
        // Then merge the rest polynomials whose sizes match the current running oracle
        let running_oracle_len = running_oracle.len();
        comms
            .iter()
            .enumerate()
            .filter(|(_, comm)| comm.codeword_size() == running_oracle_len)
            .for_each(|(index, comm)| {
                running_oracle
                    .par_iter_mut()
                    .zip_eq(comm.get_codeword().par_iter())
                    .for_each(|(r, &a)| *r += a * coeffs[index]);
            });

        if i < num_rounds - 1 {
            last_sumcheck_message =
                sum_check_challenge_round(&mut eq, &mut sum_of_all_evals_for_sumcheck, challenge);
            sumcheck_messages.push(last_sumcheck_message.clone());
            let running_tree = MerkleTree::<F, H>::from_leaves(running_oracle.clone());
            let running_root = running_tree.root();
            transcript.write_commitment(&running_root).unwrap();

            oracles.push(running_oracle.clone());
            trees.push(running_tree);
        } else {
            // The difference of the last round is that we don't need to compute the message,
            // and we don't interpolate the small polynomials. So after the last round,
            // sum_of_all_evals_for_sumcheck is exactly the evaluation representation of the
            // folded polynomial so far.
            sum_check_last_round(&mut eq, &mut sum_of_all_evals_for_sumcheck, challenge);
            // For the FRI part, we send the current polynomial as the message.
            // Transform it back into little endiean before sending it
            reverse_index_bits_in_place(&mut sum_of_all_evals_for_sumcheck);
            transcript
                .write_field_elements(&sum_of_all_evals_for_sumcheck)
                .unwrap();

            if cfg!(feature = "sanity-check") {
                // If the prover is honest, in the last round, the running oracle
                // on the prover side should be exactly the encoding of the folded polynomial.

                let coeffs = interpolate_over_boolean_hypercube(&sum_of_all_evals_for_sumcheck);
                let basecode = encode_rs_basecode(&coeffs, 1 << log_rate, coeffs.len());
                assert_eq!(basecode.len(), 1);
                let basecode = basecode[0].clone();

                reverse_index_bits_in_place(&mut running_oracle);
                assert_eq!(basecode, running_oracle);
            }
        }
    }
    return (trees, oracles);
}

fn query_phase<F: PrimeField, H: Hash>(
    transcript: &mut impl TranscriptWrite<Output<H>, F>,
    comm: &BasefoldCommitmentWithData<F, H>,
    oracles: &Vec<Vec<F>>,
    num_verifier_queries: usize,
) -> QueriesResult<F> {
    let queries = transcript.squeeze_challenges(num_verifier_queries);

    // Transform the challenge queries from field elements into integers
    let queries_usize: Vec<usize> = queries
        .iter()
        .map(|x_index| {
            let x_rep = (*x_index).to_repr();
            let x: &[u8] = x_rep.as_ref();
            let (int_bytes, _) = x.split_at(std::mem::size_of::<u32>());
            let x_int: u32 = u32::from_be_bytes(int_bytes.try_into().unwrap());
            ((x_int as usize) % comm.codeword_size()).into()
        })
        .collect_vec();

    QueriesResult {
        inner: queries_usize
            .par_iter()
            .map(|x_index| {
                (
                    *x_index,
                    basefold_get_query::<F>(comm.get_codeword(), &oracles, *x_index),
                )
            })
            .collect(),
    }
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
struct CodewordSingleQueryResult<F> {
    left: F,
    right: F,
    index: usize,
}

impl<F> CodewordSingleQueryResult<F> {
    fn new(left: F, right: F, index: usize) -> Self {
        Self { left, right, index }
    }

    pub fn write_transcript<H: Hash>(&self, transcript: &mut impl TranscriptWrite<Output<H>, F>) {
        transcript.write_field_element(&self.left).unwrap();
        transcript.write_field_element(&self.right).unwrap();
    }

    pub fn read_transcript<H: Hash>(
        transcript: &mut impl TranscriptRead<Output<H>, F>,
        full_codeword_size_log: usize,
        codeword_size_log: usize,
        index: usize,
    ) -> Self {
        Self {
            left: transcript.read_field_element().unwrap(),
            right: transcript.read_field_element().unwrap(),
            index: index >> (full_codeword_size_log - codeword_size_log),
        }
    }
}

#[derive(Debug, Clone)]
struct CodewordSingleQueryResultWithMerklePath<F, H: Hash> {
    query: CodewordSingleQueryResult<F>,
    merkle_path: MerklePathWithoutLeafOrRoot<H>,
}

impl<F: PrimeField, H: Hash> CodewordSingleQueryResultWithMerklePath<F, H> {
    pub fn write_transcript(&self, transcript: &mut impl TranscriptWrite<Output<H>, F>) {
        self.query.write_transcript::<H>(transcript);
        self.merkle_path.write_transcript::<F>(transcript);
    }

    pub fn read_transcript(
        transcript: &mut impl TranscriptRead<Output<H>, F>,
        full_codeword_size_log: usize,
        codeword_size_log: usize,
        index: usize,
    ) -> Self {
        Self {
            query: CodewordSingleQueryResult::read_transcript::<H>(
                transcript,
                full_codeword_size_log,
                codeword_size_log,
                index,
            ),
            merkle_path: MerklePathWithoutLeafOrRoot::read_transcript::<F>(
                transcript,
                codeword_size_log,
            ),
        }
    }

    pub fn check_merkle_path(&self, root: &Output<H>) {
        self.merkle_path.authenticate_leaves_root(
            self.query.left,
            self.query.right,
            self.query.index,
            root,
        );
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OracleListQueryResult<F> {
    inner: Vec<CodewordSingleQueryResult<F>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CommitmentsQueryResult<F> {
    inner: Vec<CodewordSingleQueryResult<F>>,
}

#[derive(Debug, Clone)]
struct OracleListQueryResultWithMerklePath<F, H: Hash> {
    inner: Vec<CodewordSingleQueryResultWithMerklePath<F, H>>,
}

impl<F: PrimeField, H: Hash> OracleListQueryResultWithMerklePath<F, H> {
    pub fn read_transcript(
        transcript: &mut impl TranscriptRead<Output<H>, F>,
        num_rounds: usize,
        codeword_size_log: usize,
        index: usize,
    ) -> Self {
        // Remember that the prover doesn't send the commitment in the last round.
        // In the first round, the oracle is sent after folding, so the first oracle
        // has half the size of the full codeword size.
        Self {
            inner: (0..num_rounds - 1)
                .map(|round| {
                    CodewordSingleQueryResultWithMerklePath::read_transcript(
                        transcript,
                        codeword_size_log,
                        codeword_size_log - round - 1,
                        index,
                    )
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone)]
struct CommitmentsQueryResultWithMerklePath<F, H: Hash> {
    inner: Vec<CodewordSingleQueryResultWithMerklePath<F, H>>,
}

impl<F: PrimeField, H: Hash> CommitmentsQueryResultWithMerklePath<F, H> {
    pub fn read_transcript(
        transcript: &mut impl TranscriptRead<Output<H>, F>,
        max_num_vars: usize,
        poly_num_vars: &[usize],
        log_rate: usize,
        index: usize,
    ) -> Self {
        Self {
            inner: poly_num_vars
                .iter()
                .map(|num_vars| {
                    CodewordSingleQueryResultWithMerklePath::read_transcript(
                        transcript,
                        max_num_vars + log_rate,
                        num_vars + log_rate,
                        index,
                    )
                })
                .collect(),
        }
    }
}

impl<F: PrimeField> ListQueryResult<F> for OracleListQueryResult<F> {
    fn get_inner(&self) -> &Vec<CodewordSingleQueryResult<F>> {
        &self.inner
    }

    fn get_inner_into(self) -> Vec<CodewordSingleQueryResult<F>> {
        self.inner
    }
}

impl<F: PrimeField> ListQueryResult<F> for CommitmentsQueryResult<F> {
    fn get_inner(&self) -> &Vec<CodewordSingleQueryResult<F>> {
        &self.inner
    }

    fn get_inner_into(self) -> Vec<CodewordSingleQueryResult<F>> {
        self.inner
    }
}

impl<F: PrimeField, H: Hash> ListQueryResultWithMerklePath<F, H>
    for OracleListQueryResultWithMerklePath<F, H>
{
    fn get_inner(&self) -> &Vec<CodewordSingleQueryResultWithMerklePath<F, H>> {
        &self.inner
    }

    fn new(inner: Vec<CodewordSingleQueryResultWithMerklePath<F, H>>) -> Self {
        Self { inner }
    }
}

impl<F: PrimeField, H: Hash> ListQueryResultWithMerklePath<F, H>
    for CommitmentsQueryResultWithMerklePath<F, H>
{
    fn get_inner(&self) -> &Vec<CodewordSingleQueryResultWithMerklePath<F, H>> {
        &self.inner
    }

    fn new(inner: Vec<CodewordSingleQueryResultWithMerklePath<F, H>>) -> Self {
        Self { inner }
    }
}

trait ListQueryResult<F: PrimeField> {
    fn get_inner(&self) -> &Vec<CodewordSingleQueryResult<F>>;

    fn get_inner_into(self) -> Vec<CodewordSingleQueryResult<F>>;

    fn merkle_path<'a, H: Hash>(
        &self,
        trees: impl Fn(usize) -> &'a MerkleTree<F, H>,
    ) -> Vec<MerklePathWithoutLeafOrRoot<H>> {
        self.get_inner()
            .into_iter()
            .enumerate()
            .map(|(i, query_result)| {
                let path = trees(i).merkle_path_without_leaf_sibling_or_root(query_result.index);
                if cfg!(feature = "sanity-check") {
                    path.authenticate_leaves_root(
                        query_result.left,
                        query_result.right,
                        query_result.index,
                        &trees(i).root(),
                    );
                }
                path
            })
            .collect_vec()
    }
}

trait ListQueryResultWithMerklePath<F: PrimeField, H: Hash>: Sized {
    fn new(inner: Vec<CodewordSingleQueryResultWithMerklePath<F, H>>) -> Self;

    fn get_inner(&self) -> &Vec<CodewordSingleQueryResultWithMerklePath<F, H>>;

    fn from_query_and_trees<'a, LQR: ListQueryResult<F>>(
        query_result: LQR,
        trees: impl Fn(usize) -> &'a MerkleTree<F, H>,
    ) -> Self {
        Self::new(
            query_result
                .merkle_path(trees)
                .into_iter()
                .zip(query_result.get_inner_into().into_iter())
                .map(
                    |(path, codeword_result)| CodewordSingleQueryResultWithMerklePath {
                        query: codeword_result,
                        merkle_path: path,
                    },
                )
                .collect_vec(),
        )
    }

    fn write_transcript(&self, transcript: &mut impl TranscriptWrite<Output<H>, F>) {
        self.get_inner()
            .iter()
            .for_each(|q| q.write_transcript(transcript));
    }

    fn check_merkle_paths(&self, roots: &Vec<Output<H>>) {
        self.get_inner()
            .iter()
            .zip(roots.iter())
            .for_each(|(q, root)| {
                q.check_merkle_path(root);
            });
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SingleQueryResult<F> {
    oracle_query: OracleListQueryResult<F>,
    commitment_query: CodewordSingleQueryResult<F>,
}

#[derive(Debug, Clone)]
struct SingleQueryResultWithMerklePath<F, H: Hash> {
    oracle_query: OracleListQueryResultWithMerklePath<F, H>,
    commitment_query: CodewordSingleQueryResultWithMerklePath<F, H>,
}

impl<F: PrimeField, H: Hash> SingleQueryResultWithMerklePath<F, H> {
    pub fn from_single_query_result(
        single_query_result: SingleQueryResult<F>,
        oracle_trees: &Vec<MerkleTree<F, H>>,
        commitment: &BasefoldCommitmentWithData<F, H>,
    ) -> Self {
        Self {
            oracle_query: OracleListQueryResultWithMerklePath::from_query_and_trees(
                single_query_result.oracle_query,
                |i| &oracle_trees[i],
            ),
            commitment_query: CodewordSingleQueryResultWithMerklePath {
                query: single_query_result.commitment_query,
                merkle_path: commitment
                    .codeword_tree
                    .merkle_path_without_leaf_sibling_or_root(
                        single_query_result.commitment_query.index,
                    ),
            },
        }
    }

    pub fn write_transcript(&self, transcript: &mut impl TranscriptWrite<Output<H>, F>) {
        self.oracle_query.write_transcript(transcript);
        self.commitment_query.write_transcript(transcript);
    }

    pub fn read_transcript(
        transcript: &mut impl TranscriptRead<Output<H>, F>,
        num_rounds: usize,
        log_rate: usize,
        num_vars: usize,
        index: usize,
    ) -> Self {
        Self {
            oracle_query: OracleListQueryResultWithMerklePath::read_transcript(
                transcript,
                num_rounds,
                num_vars + log_rate,
                index,
            ),
            commitment_query: CodewordSingleQueryResultWithMerklePath::read_transcript(
                transcript,
                num_vars + log_rate,
                num_vars + log_rate,
                index,
            ),
        }
    }

    pub fn check(
        &self,
        fold_challenges: &Vec<F>,
        num_rounds: usize,
        num_vars: usize,
        log_rate: usize,
        final_codeword: &Vec<F>,
        roots: &Vec<Output<H>>,
        comm: &BasefoldCommitment<H>,
        mut cipher: ctr::Ctr32LE<aes::Aes128>,
        index: usize,
    ) {
        self.oracle_query.check_merkle_paths(roots);
        self.commitment_query.check_merkle_path(&comm.root());

        let mut curr_left = self.commitment_query.query.left;
        let mut curr_right = self.commitment_query.query.right;

        let mut right_index = index | 1;
        let mut left_index = right_index - 1;

        for i in 0..num_rounds {
            let ri0 = reverse_bits(left_index, num_vars + log_rate - i);

            let x0: F = query_point(
                1 << (num_vars + log_rate - i),
                ri0,
                num_vars + log_rate - i - 1,
                &mut cipher,
            );
            let x1 = -x0;

            let res = interpolate2([(x0, curr_left), (x1, curr_right)], fold_challenges[i]);

            let next_index = right_index >> 1;
            let next_oracle_value = if i < num_rounds - 1 {
                right_index = next_index | 1;
                left_index = right_index - 1;
                let next_oracle_query = self.oracle_query.get_inner()[i].clone();
                curr_left = next_oracle_query.query.left;
                curr_right = next_oracle_query.query.right;
                if next_index & 1 == 0 {
                    curr_left
                } else {
                    curr_right
                }
            } else {
                // Note that final_codeword has been bit-reversed, so no need to bit-reverse
                // next_index here.
                final_codeword[next_index]
            };
            assert_eq!(res, next_oracle_value);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BatchedSingleQueryResult<F> {
    oracle_query: OracleListQueryResult<F>,
    commitments_query: CommitmentsQueryResult<F>,
}

#[derive(Debug, Clone)]
struct BatchedSingleQueryResultWithMerklePath<F, H: Hash> {
    oracle_query: OracleListQueryResultWithMerklePath<F, H>,
    commitments_query: CommitmentsQueryResultWithMerklePath<F, H>,
}

impl<F: PrimeField, H: Hash> BatchedSingleQueryResultWithMerklePath<F, H> {
    pub fn from_batched_single_query_result(
        batched_single_query_result: BatchedSingleQueryResult<F>,
        oracle_trees: &Vec<MerkleTree<F, H>>,
        commitments: &Vec<&BasefoldCommitmentWithData<F, H>>,
    ) -> Self {
        Self {
            oracle_query: OracleListQueryResultWithMerklePath::from_query_and_trees(
                batched_single_query_result.oracle_query,
                |i| &oracle_trees[i],
            ),
            commitments_query: CommitmentsQueryResultWithMerklePath::from_query_and_trees(
                batched_single_query_result.commitments_query,
                |i| &commitments[i].codeword_tree,
            ),
        }
    }

    pub fn write_transcript(&self, transcript: &mut impl TranscriptWrite<Output<H>, F>) {
        self.oracle_query.write_transcript(transcript);
        self.commitments_query.write_transcript(transcript);
    }

    pub fn read_transcript(
        transcript: &mut impl TranscriptRead<Output<H>, F>,
        num_rounds: usize,
        log_rate: usize,
        poly_num_vars: &[usize],
        index: usize,
    ) -> Self {
        let num_vars = poly_num_vars.iter().max().unwrap();
        Self {
            oracle_query: OracleListQueryResultWithMerklePath::read_transcript(
                transcript,
                num_rounds,
                *num_vars + log_rate,
                index,
            ),
            commitments_query: CommitmentsQueryResultWithMerklePath::read_transcript(
                transcript,
                *num_vars,
                poly_num_vars,
                log_rate,
                index,
            ),
        }
    }

    pub fn check(
        &self,
        fold_challenges: &Vec<F>,
        num_rounds: usize,
        num_vars: usize,
        log_rate: usize,
        final_codeword: &Vec<F>,
        roots: &Vec<Output<H>>,
        comms: &Vec<&BasefoldCommitment<H>>,
        coeffs: &[F],
        mut cipher: ctr::Ctr32LE<aes::Aes128>,
        index: usize,
    ) {
        self.oracle_query.check_merkle_paths(roots);
        self.commitments_query
            .check_merkle_paths(&comms.iter().map(|comm| comm.root()).collect());

        let mut curr_left = F::ZERO;
        let mut curr_right = F::ZERO;

        let mut right_index = index | 1;
        let mut left_index = right_index - 1;

        for i in 0..num_rounds {
            let ri0 = reverse_bits(left_index, num_vars + log_rate - i);
            let matching_comms = comms
                .iter()
                .enumerate()
                .filter(|(_, comm)| comm.num_vars().unwrap() == num_vars - i)
                .map(|(index, _)| index)
                .collect_vec();

            matching_comms.iter().for_each(|index| {
                let query = self.commitments_query.get_inner()[*index].query;
                curr_left += query.left * coeffs[*index];
                curr_right += query.right * coeffs[*index];
            });

            let x0: F = query_point(
                1 << (num_vars + log_rate - i),
                ri0,
                num_vars + log_rate - i - 1,
                &mut cipher,
            );
            let x1 = -x0;

            let res = interpolate2([(x0, curr_left), (x1, curr_right)], fold_challenges[i]);

            let next_index = right_index >> 1;
            let next_oracle_value = if i < num_rounds - 1 {
                right_index = next_index | 1;
                left_index = right_index - 1;
                let next_oracle_query = &self.oracle_query.get_inner()[i];
                curr_left = next_oracle_query.query.left;
                curr_right = next_oracle_query.query.right;
                if next_index & 1 == 0 {
                    curr_left
                } else {
                    curr_right
                }
            } else {
                // Note that final_codeword has been bit-reversed, so no need to bit-reverse
                // next_index here.
                final_codeword[next_index]
            };
            assert_eq!(res, next_oracle_value);
        }
    }
}

struct BatchedQueriesResult<F> {
    inner: Vec<(usize, BatchedSingleQueryResult<F>)>,
}

struct BatchedQueriesResultWithMerklePath<F, H: Hash> {
    inner: Vec<(usize, BatchedSingleQueryResultWithMerklePath<F, H>)>,
}

impl<F: PrimeField, H: Hash> BatchedQueriesResultWithMerklePath<F, H> {
    pub fn from_batched_query_result(
        batched_query_result: BatchedQueriesResult<F>,
        oracle_trees: &Vec<MerkleTree<F, H>>,
        commitments: &Vec<&BasefoldCommitmentWithData<F, H>>,
    ) -> Self {
        Self {
            inner: batched_query_result
                .inner
                .into_iter()
                .map(|(i, q)| {
                    (
                        i,
                        BatchedSingleQueryResultWithMerklePath::from_batched_single_query_result(
                            q,
                            oracle_trees,
                            commitments,
                        ),
                    )
                })
                .collect(),
        }
    }

    pub fn write_transcript(&self, transcript: &mut impl TranscriptWrite<Output<H>, F>) {
        self.inner
            .iter()
            .for_each(|(_, q)| q.write_transcript(transcript));
    }

    pub fn read_transcript(
        transcript: &mut impl TranscriptRead<Output<H>, F>,
        num_rounds: usize,
        log_rate: usize,
        poly_num_vars: &[usize],
        indices: &[usize],
    ) -> Self {
        Self {
            inner: indices
                .iter()
                .map(|index| {
                    (
                        *index,
                        BatchedSingleQueryResultWithMerklePath::read_transcript(
                            transcript,
                            num_rounds,
                            log_rate,
                            poly_num_vars,
                            *index,
                        ),
                    )
                })
                .collect(),
        }
    }

    pub fn check(
        &self,
        fold_challenges: &Vec<F>,
        num_rounds: usize,
        num_vars: usize,
        log_rate: usize,
        final_codeword: &Vec<F>,
        roots: &Vec<Output<H>>,
        comms: &Vec<&BasefoldCommitment<H>>,
        coeffs: &[F],
        cipher: ctr::Ctr32LE<aes::Aes128>,
    ) {
        self.inner.par_iter().for_each(|(index, query)| {
            query.check(
                fold_challenges,
                num_rounds,
                num_vars,
                log_rate,
                final_codeword,
                roots,
                comms,
                coeffs,
                cipher.clone(),
                *index,
            );
        });
    }
}

struct QueriesResult<F> {
    inner: Vec<(usize, SingleQueryResult<F>)>,
}

struct QueriesResultWithMerklePath<F, H: Hash> {
    inner: Vec<(usize, SingleQueryResultWithMerklePath<F, H>)>,
}

impl<F: PrimeField, H: Hash> QueriesResultWithMerklePath<F, H> {
    pub fn from_query_result(
        query_result: QueriesResult<F>,
        oracle_trees: &Vec<MerkleTree<F, H>>,
        commitment: &BasefoldCommitmentWithData<F, H>,
    ) -> Self {
        Self {
            inner: query_result
                .inner
                .into_iter()
                .map(|(i, q)| {
                    (
                        i,
                        SingleQueryResultWithMerklePath::from_single_query_result(
                            q,
                            oracle_trees,
                            commitment,
                        ),
                    )
                })
                .collect(),
        }
    }

    pub fn write_transcript(&self, transcript: &mut impl TranscriptWrite<Output<H>, F>) {
        self.inner
            .iter()
            .for_each(|(_, q)| q.write_transcript(transcript));
    }

    pub fn read_transcript(
        transcript: &mut impl TranscriptRead<Output<H>, F>,
        num_rounds: usize,
        log_rate: usize,
        poly_num_vars: usize,
        indices: &[usize],
    ) -> Self {
        Self {
            inner: indices
                .iter()
                .map(|index| {
                    (
                        *index,
                        SingleQueryResultWithMerklePath::read_transcript(
                            transcript,
                            num_rounds,
                            log_rate,
                            poly_num_vars,
                            *index,
                        ),
                    )
                })
                .collect(),
        }
    }

    pub fn check(
        &self,
        fold_challenges: &Vec<F>,
        num_rounds: usize,
        num_vars: usize,
        log_rate: usize,
        final_codeword: &Vec<F>,
        roots: &Vec<Output<H>>,
        comm: &BasefoldCommitment<H>,
        cipher: ctr::Ctr32LE<aes::Aes128>,
    ) {
        self.inner.par_iter().for_each(|(index, query)| {
            query.check(
                fold_challenges,
                num_rounds,
                num_vars,
                log_rate,
                final_codeword,
                roots,
                comm,
                cipher.clone(),
                *index,
            );
        });
    }
}

fn batch_query_phase<F: PrimeField, H: Hash>(
    transcript: &mut impl TranscriptWrite<Output<H>, F>,
    codeword_size: usize,
    comms: &[&BasefoldCommitmentWithData<F, H>],
    oracles: &Vec<Vec<F>>,
    num_verifier_queries: usize,
) -> BatchedQueriesResult<F> {
    let queries = transcript.squeeze_challenges(num_verifier_queries);

    // Transform the challenge queries from field elements into integers
    let queries_usize: Vec<usize> = queries
        .iter()
        .map(|x_index| field_to_usize(x_index, Some(codeword_size)))
        .collect_vec();

    BatchedQueriesResult {
        inner: queries_usize
            .par_iter()
            .map(|x_index| {
                (
                    *x_index,
                    batch_basefold_get_query::<F, H>(comms, &oracles, codeword_size, *x_index),
                )
            })
            .collect(),
    }
}

fn verifier_query_phase<F: PrimeField, H: Hash>(
    queries: &QueriesResultWithMerklePath<F, H>,
    sum_check_messages: &Vec<Vec<F>>,
    fold_challenges: &Vec<F>,
    num_rounds: usize,
    num_vars: usize,
    log_rate: usize,
    final_message: &Vec<F>,
    roots: &Vec<Output<H>>,
    comm: &BasefoldCommitment<H>,
    partial_eq: &[F],
    rng: ChaCha8Rng,
    eval: &F,
) {
    let message = interpolate_over_boolean_hypercube(&final_message);
    let mut final_codeword = encode_rs_basecode(&message, 1 << log_rate, message.len());
    assert_eq!(final_codeword.len(), 1);
    let mut final_codeword = final_codeword.remove(0);
    reverse_index_bits_in_place(&mut final_codeword);

    // For computing the weights on the fly, because the verifier is incapable of storing
    // the weights.
    let mut key: [u8; 16] = [0u8; 16];
    let mut iv: [u8; 16] = [0u8; 16];
    let mut rng = rng.clone();
    rng.set_word_pos(0);
    rng.fill_bytes(&mut key);
    rng.fill_bytes(&mut iv);

    type Aes128Ctr64LE = ctr::Ctr32LE<aes::Aes128>;
    let cipher = Aes128Ctr64LE::new(
        GenericArray::from_slice(&key[..]),
        GenericArray::from_slice(&iv[..]),
    );

    queries.check(
        fold_challenges,
        num_rounds,
        num_vars,
        log_rate,
        &final_codeword,
        roots,
        comm,
        cipher,
    );

    assert_eq!(eval, &degree_2_zero_plus_one(&sum_check_messages[0]));

    // The sum-check part of the protocol
    for i in 0..fold_challenges.len() - 1 {
        assert_eq!(
            degree_2_eval(&sum_check_messages[i], fold_challenges[i]),
            degree_2_zero_plus_one(&sum_check_messages[i + 1])
        );
    }

    // Finally, the last sumcheck poly evaluation should be the same as the sum of the polynomial
    // sent from the prover
    assert_eq!(
        degree_2_eval(
            &sum_check_messages[fold_challenges.len() - 1],
            fold_challenges[fold_challenges.len() - 1]
        ),
        inner_product(final_message, partial_eq)
    );
}

fn batch_verifier_query_phase<F: PrimeField, H: Hash>(
    queries: &BatchedQueriesResultWithMerklePath<F, H>,
    sum_check_messages: &Vec<Vec<F>>,
    fold_challenges: &Vec<F>,
    num_rounds: usize,
    num_vars: usize,
    log_rate: usize,
    final_message: &Vec<F>,
    roots: &Vec<Output<H>>,
    comms: &Vec<&BasefoldCommitment<H>>,
    coeffs: &[F],
    rng: ChaCha8Rng,
    eval: &F,
) {
    let message = interpolate_over_boolean_hypercube(&final_message);
    let mut final_codeword = encode_rs_basecode(&message, 1 << log_rate, message.len());
    assert_eq!(final_codeword.len(), 1);
    let mut final_codeword = final_codeword.remove(0);
    reverse_index_bits_in_place(&mut final_codeword);

    // For computing the weights on the fly, because the verifier is incapable of storing
    // the weights.
    let mut key: [u8; 16] = [0u8; 16];
    let mut iv: [u8; 16] = [0u8; 16];
    let mut rng = rng.clone();
    rng.set_word_pos(0);
    rng.fill_bytes(&mut key);
    rng.fill_bytes(&mut iv);

    type Aes128Ctr64LE = ctr::Ctr32LE<aes::Aes128>;
    let cipher = Aes128Ctr64LE::new(
        GenericArray::from_slice(&key[..]),
        GenericArray::from_slice(&iv[..]),
    );

    queries.check(
        fold_challenges,
        num_rounds,
        num_vars,
        log_rate,
        &final_codeword,
        roots,
        comms,
        coeffs,
        cipher,
    );

    assert_eq!(eval, &degree_2_zero_plus_one(&sum_check_messages[0]));

    // The sum-check part of the protocol
    for i in 0..fold_challenges.len() - 1 {
        assert_eq!(
            degree_2_eval(&sum_check_messages[i], fold_challenges[i]),
            degree_2_zero_plus_one(&sum_check_messages[i + 1])
        );
    }

    // Finally, the last sumcheck poly evaluation should be the same as the sum of the polynomial
    // sent from the prover
    assert_eq!(
        degree_2_eval(
            &sum_check_messages[fold_challenges.len() - 1],
            fold_challenges[fold_challenges.len() - 1]
        ),
        final_message.iter().sum()
    );
}

fn get_table_aes<F: PrimeField>(
    poly_size: usize,
    rate: usize,
    rng: &mut ChaCha8Rng,
) -> (Vec<Vec<(F, F)>>, Vec<Vec<F>>) {
    // The size (logarithmic) of the codeword for the polynomial
    let lg_n: usize = rate + log2_strict(poly_size);

    let mut key: [u8; 16] = [0u8; 16];
    let mut iv: [u8; 16] = [0u8; 16];
    rng.fill_bytes(&mut key);
    rng.fill_bytes(&mut iv);

    type Aes128Ctr64LE = ctr::Ctr32LE<aes::Aes128>;

    let mut cipher = Aes128Ctr64LE::new(
        GenericArray::from_slice(&key[..]),
        GenericArray::from_slice(&iv[..]),
    );

    // Allocate the buffer for storing n field elements (the entire codeword)
    let bytes = num_of_bytes::<F>(1 << lg_n);
    let mut dest: Vec<u8> = vec![0u8; bytes];
    cipher.apply_keystream(&mut dest[..]);

    // Now, dest is a vector filled with random data for a field vector of size n

    // Collect the bytes into field elements
    let flat_table: Vec<F> = dest
        .par_chunks_exact(num_of_bytes::<F>(1))
        .map(|chunk| from_raw_bytes::<F>(&chunk.to_vec()))
        .collect::<Vec<_>>();

    // Now, flat_table is a field vector of size n, filled with random field elements
    assert_eq!(flat_table.len(), 1 << lg_n);

    // Multiply -2 to every element to get the weights. Now weights = { -2x }
    let mut weights: Vec<F> = flat_table
        .par_iter()
        .map(|el| F::ZERO - *el - *el)
        .collect();

    // Then invert all the elements. Now weights = { -1/2x }
    let mut scratch_space = vec![F::ZERO; weights.len()];
    BatchInverter::invert_with_external_scratch(&mut weights, &mut scratch_space);

    // Zip x and -1/2x together. The result is the list { (x, -1/2x) }
    // What is this -1/2x? It is used in linear interpolation over the domain (x, -x), which
    // involves computing 1/(b-a) where b=-x and a=x, and 1/(b-a) here is exactly -1/2x
    let flat_table_w_weights = flat_table
        .iter()
        .zip(weights)
        .map(|(el, w)| (*el, w))
        .collect_vec();

    // Split the positions from 0 to n-1 into slices of sizes:
    // 2, 2, 4, 8, ..., n/2, exactly lg_n number of them
    // The weights are (x, -1/2x), the table elements are just x

    let mut unflattened_table_w_weights = vec![Vec::new(); lg_n];
    let mut unflattened_table = vec![Vec::new(); lg_n];

    let mut level_weights = flat_table_w_weights[0..2].to_vec();
    // Apply the reverse-bits permutation to a vector of size 2, equivalent to just swapping
    reverse_index_bits_in_place(&mut level_weights);
    unflattened_table_w_weights[0] = level_weights;

    unflattened_table[0] = flat_table[0..2].to_vec();
    for i in 1..lg_n {
        unflattened_table[i] = flat_table[(1 << i)..(1 << (i + 1))].to_vec();
        let mut level = flat_table_w_weights[(1 << i)..(1 << (i + 1))].to_vec();
        reverse_index_bits_in_place(&mut level);
        unflattened_table_w_weights[i] = level;
    }

    return (unflattened_table_w_weights, unflattened_table);
}

#[cfg(test)]
mod test {
    use super::*;

    use crate::{
        multilinear::{
            basefold::Basefold,
            test::{run_batch_commit_open_verify, run_commit_open_verify},
        },
        util::{
            hash::{Hash, Keccak256, Output},
            new_fields::{Mersenne127, Mersenne61},
            transcript::Blake2sTranscript,
        },
    };
    use halo2_curves::{ff::Field, secp256k1::Fp};
    use rand_chacha::{
        rand_core::{RngCore, SeedableRng},
        ChaCha12Rng, ChaCha8Rng,
    };

    use crate::multilinear::basefold::Instant;
    use crate::multilinear::BasefoldExtParams;
    use crate::util::arithmetic::PrimeField;
    use blake2::Blake2s256;

    type Pcs = Basefold<Fp, Blake2s256, Five>;

    #[derive(Debug)]
    pub struct Five {}

    pub fn p_i<F: PrimeField>(evals: &Vec<F>, eq: &Vec<F>) -> Vec<F> {
        if evals.len() == 1 {
            return vec![evals[0], evals[0], evals[0]];
        }
        //evals coeffs
        let mut coeffs = vec![F::ZERO, F::ZERO, F::ZERO];
        let mut i = 0;
        while i < evals.len() {
            coeffs[0] += evals[i] * eq[i];
            coeffs[1] += evals[i + 1] * eq[i] + evals[i] * eq[i + 1];
            coeffs[2] += evals[i + 1] * eq[i + 1];
            i += 2;
        }

        coeffs
    }

    fn get_table<F: PrimeField>(
        poly_size: usize,
        rate: usize,
        rng: &mut ChaCha8Rng,
    ) -> (Vec<Vec<(F, F)>>, Vec<Vec<F>>) {
        let lg_n: usize = rate + log2_strict(poly_size);

        let bytes = (F::NUM_BITS as usize).next_power_of_two() * (1 << lg_n) / 8;
        let mut dest: Vec<u8> = vec![0u8; bytes];
        rng.fill_bytes(&mut dest);

        let flat_table: Vec<F> = dest
            .par_chunks_exact((F::NUM_BITS as usize).next_power_of_two() / 8)
            .map(|chunk| from_raw_bytes::<F>(&chunk.to_vec()))
            .collect::<Vec<_>>();

        assert_eq!(flat_table.len(), 1 << lg_n);

        let mut weights: Vec<F> = flat_table
            .par_iter()
            .map(|el| F::ZERO - *el - *el)
            .collect();

        let mut scratch_space = vec![F::ZERO; weights.len()];
        BatchInverter::invert_with_external_scratch(&mut weights, &mut scratch_space);

        let flat_table_w_weights = flat_table
            .iter()
            .zip(weights)
            .map(|(el, w)| (*el, w))
            .collect_vec();

        let mut unflattened_table_w_weights = vec![Vec::new(); lg_n];
        let mut unflattened_table = vec![Vec::new(); lg_n];

        let mut level_weights = flat_table_w_weights[0..2].to_vec();
        reverse_index_bits_in_place(&mut level_weights);
        unflattened_table_w_weights[0] = level_weights;

        unflattened_table[0] = flat_table[0..2].to_vec();
        for i in 1..lg_n {
            unflattened_table[i] = flat_table[(1 << i)..(1 << (i + 1))].to_vec();
            let mut level = flat_table_w_weights[(1 << i)..(1 << (i + 1))].to_vec();
            reverse_index_bits_in_place(&mut level);
            unflattened_table_w_weights[i] = level;
        }

        return (unflattened_table_w_weights, unflattened_table);
    }

    pub fn multilinear_evaluation_ztoa<F: PrimeField>(poly: &mut Vec<F>, point: &Vec<F>) {
        let n = log2_strict(poly.len());
        assert_eq!(point.len(), n);
        for p in point {
            poly.par_chunks_mut(2).for_each(|chunk| {
                chunk[0] = chunk[0] + *p * chunk[1];
                chunk[1] = chunk[0];
            });
            poly.dedup();
        }
    }

    //helper function
    fn rand_vec<F: PrimeField>(size: usize, mut rng: &mut ChaCha8Rng) -> Vec<F> {
        (0..size).map(|_| F::random(&mut rng)).collect()
    }

    impl BasefoldExtParams for Five {
        fn get_reps() -> usize {
            return 260;
        }

        fn get_rate() -> usize {
            return 3;
        }

        fn get_basecode() -> usize {
            return 3;
        }
    }

    #[test]
    fn time_rs_code() {
        use rand::rngs::OsRng;

        let poly = MultilinearPolynomial::rand(20, OsRng);

        encode_rs_basecode::<Mersenne61>(&poly.evals().to_vec(), 2, 64);
    }

    #[test]
    fn test_sumcheck() {
        use crate::util::ff_255::ff255::Ft255;
        let i = 25;
        let mut rng = ChaCha8Rng::from_entropy();
        let evals = rand_vec::<Ft255>(1 << i, &mut rng);
        let eq = rand_vec::<Ft255>(1 << i, &mut rng);
        let coeffs1 = p_i(&evals, &eq);
        let coeffs2 = parallel_pi(&evals, &eq);
        assert_eq!(coeffs1, coeffs2);
    }

    #[test]
    fn commit_open_verify() {
        run_commit_open_verify::<_, Pcs, Blake2sTranscript<_>>();
    }

    #[test]
    fn batch_commit_open_verify() {
        run_batch_commit_open_verify::<_, Pcs, Blake2sTranscript<_>>();
    }

    #[test]
    fn bench_multilinear_eval() {
        use crate::util::ff_255::ff255::Ft255;
        for i in 10..27 {
            let mut rng = ChaCha8Rng::from_entropy();
            let mut poly = rand_vec::<Ft255>(1 << i, &mut rng);
            let point = rand_vec::<Ft255>(i, &mut rng);
            let now = Instant::now();
            multilinear_evaluation_ztoa(&mut poly, &point);
            println!(
                "time for multilinear eval degree i {:?} : {:?}",
                i,
                now.elapsed().as_millis()
            );
        }
    }

    #[test]
    fn test_sha3_hashes() {
        use blake2::digest::FixedOutputReset;

        type H = Keccak256;
        let lots_of_hashes = Instant::now();
        let values = vec![Mersenne127::ONE; 2000];
        let mut hashes = vec![Output::<H>::default(); values.len() >> 1];
        for (i, mut hash) in hashes.iter_mut().enumerate() {
            let mut hasher = H::new();
            hasher.update_field_element(&values[i + i]);
            hasher.update_field_element(&values[i + i + 1]);
            hasher.finalize_into_reset(&mut hash);
        }
        println!("lots of hashes sha3 time {:?}", lots_of_hashes.elapsed());

        let hash_alot = Instant::now();
        let mut hasher = H::new();
        for i in 0..2000 {
            hasher.update_field_element(&values[i]);
        }
        let mut hash = Output::<H>::default();
        hasher.finalize_into_reset(&mut hash);
        println!("hash a lot sha3 time {:?}", hash_alot.elapsed());
    }

    #[test]
    fn test_blake2b_hashes() {
        use blake2::{digest::FixedOutputReset, Blake2s256};

        type H = Blake2s256;
        let lots_of_hashes = Instant::now();
        let values = vec![Mersenne127::ONE; 2000];
        let mut hashes = vec![Output::<H>::default(); values.len() >> 1];
        for (i, mut hash) in hashes.iter_mut().enumerate() {
            let mut hasher = H::new();
            hasher.update_field_element(&values[i + i]);
            hasher.update_field_element(&values[i + i + 1]);
            hasher.finalize_into_reset(&mut hash);
        }
        println!("lots of hashes blake2 time {:?}", lots_of_hashes.elapsed());

        let hash_alot = Instant::now();
        let mut hasher = H::new();
        for i in 0..2000 {
            hasher.update_field_element(&values[i]);
        }
        let mut hash = Output::<H>::default();
        hasher.finalize_into_reset(&mut hash);
        println!("hash alot blake2 time {:?}", hash_alot.elapsed());
    }

    #[test]
    fn test_blake2b_no_finalize() {
        use blake2::{digest::FixedOutputReset, Blake2s256};

        type H = Blake2s256;
        let lots_of_hashes = Instant::now();
        let values = vec![Mersenne127::ONE; 2000];
        let mut hashes = vec![Output::<H>::default(); values.len() >> 1];
        for (i, hash) in hashes.iter_mut().enumerate() {
            let f1 = values[i + 1].to_repr();
            let f2 = values[i + i + 1].to_repr();
            let data = [f1.as_ref(), f2.as_ref()].concat();
            //	    hasher.update_field_element(&values[i + i]);
            //	    hasher.update_field_element(&values[i+ i + 1]);
            *hash = H::digest(&data);
        }
        println!(
            "lots of hashes blake2 time no finalize{:?}",
            lots_of_hashes.elapsed()
        );

        let hash_alot = Instant::now();
        let mut hasher = H::new();
        for i in 0..2000 {
            hasher.update_field_element(&values[i]);
        }
        let mut hash = Output::<H>::default();
        hasher.finalize_into_reset(&mut hash);
        println!("hash alot blake2 time no finalize{:?}", hash_alot.elapsed());
    }

    #[test]
    fn test_cipher() {
        use aes::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};
        use generic_array::GenericArray;
        type Aes128Ctr64LE = ctr::Ctr32LE<aes::Aes128>;
        let mut rng = ChaCha12Rng::from_entropy();

        let mut key: [u8; 16] = [042; 16];
        let mut iv: [u8; 16] = [024; 16];
        rng.fill_bytes(&mut key);
        rng.fill_bytes(&mut iv);
        //	rng.set_word_pos(0);

        let mut key2: [u8; 16] = [042; 16];
        let mut iv2: [u8; 16] = [024; 16];
        rng.fill_bytes(&mut key2);
        rng.fill_bytes(&mut iv2);

        let mut buf1 = [0u8; 100];

        let mut cipher = Aes128Ctr64LE::new(
            GenericArray::from_slice(&key[..]),
            GenericArray::from_slice(&iv[..]),
        );
        let hash_time = Instant::now();
        cipher.apply_keystream(&mut buf1[..]);
        println!("aes hash 34 bytes {:?}", hash_time.elapsed());
        println!("buf1 {:?}", buf1);
        for i in 0..40 {
            let now = Instant::now();
            cipher.seek((1 << i) as u64);
            println!("aes seek {:?} : {:?}", (1 << i), now.elapsed());
        }
        let mut bufnew = [0u8; 1];
        cipher.apply_keystream(&mut bufnew);

        println!("byte1 {:?}", bufnew);

        /*
            let mut cipher2 = Aes128Ctr64LE::new(&key.into(),&iv.into());
            let mut buf2 = [0u8; 34];
            for chunk in buf2.chunks_mut(3){
                cipher2.apply_keystream(chunk);
            }

            assert_eq!(buf1,buf2);
        */
        let mut dest: Vec<u8> = vec![0u8; 34];
        let mut rng = ChaCha8Rng::from_entropy();
        let now = Instant::now();
        rng.fill_bytes(&mut dest);
        println!("chacha20 hash 34 bytes {:?}", now.elapsed());
        println!("des {:?}", dest);
        let now = Instant::now();
        rng.set_word_pos(1);

        println!("chacha8 seek {:?}", now.elapsed());

        let mut cipher = Aes128Ctr64LE::new(
            GenericArray::from_slice(&key[..]),
            GenericArray::from_slice(&iv[..]),
        );

        let now = Instant::now();
        cipher.seek(33u64);
        println!("aes seek {:?}", now.elapsed());
        let mut bufnew = [0u8; 1];
        cipher.apply_keystream(&mut bufnew);

        println!("byte1 {:?}", bufnew);
    }

    #[test]
    fn test_blake2b_simd_hashes() {
        use blake2b_simd::State;
        use ff::PrimeField;
        let lots_of_hashes = Instant::now();
        let values = vec![Mersenne127::ONE; 2000];
        let mut states = vec![State::new(); 1000];

        for (i, hash) in states.iter_mut().enumerate() {
            hash.update(&values[i + i].to_repr().as_ref());
            hash.update(&values[i + i + 1].to_repr().as_ref());
            hash.finalize();
        }
        println!(
            "lots of hashes blake2simd time {:?}",
            lots_of_hashes.elapsed()
        );

        let hash_alot = Instant::now();
        let mut state = State::new();
        for i in 0..2000 {
            state.update(values[i].to_repr().as_ref());
        }
        state.finalize();
        println!("hash alot blake2simd time {:?}", hash_alot.elapsed());
    }

    #[test]
    fn test_evaluate_generic_basecode() {
        use crate::util::new_fields::Mersenne61;
        use rand::rngs::OsRng;

        let poly = MultilinearPolynomial::rand(10, OsRng);
        let mut t_rng = ChaCha8Rng::from_entropy();
        let (_, table) = get_table(poly.evals().len(), 3, &mut t_rng);

        let rate = 8;
        let base_codewords = encode_repetition_basecode(&poly.evals().to_vec(), rate);

        let evals1 = evaluate_over_foldable_domain_generic_basecode::<Mersenne61>(
            1,
            poly.evals().len(),
            3,
            base_codewords,
            &table,
        );
        let evals2 = evaluate_over_foldable_domain::<Mersenne61>(3, poly.evals().to_vec(), &table);
        assert_eq!(evals1, evals2);
    }
}
