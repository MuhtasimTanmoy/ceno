use std::{collections::BTreeSet, mem, sync::Arc};

use ff_ext::ExtensionField;
use itertools::Itertools;
use multilinear_extensions::{
    util::ceil_log2,
    virtual_poly_v2::{ArcMultilinearExtension, VirtualPolynomialV2},
};

use crate::{expression::Expression, structs::VirtualPolynomials};

impl<'a, E: ExtensionField> VirtualPolynomials<'a, E> {
    pub fn new(num_threads: usize, num_variables: usize) -> Self {
        VirtualPolynomials {
            num_threads,
            polys: (0..num_threads)
                .map(|_| VirtualPolynomialV2::new(num_variables - ceil_log2(num_threads)))
                .collect_vec(),
        }
    }

    pub fn get_range_polys_by_thread_id(
        &self,
        thread_id: usize,
        polys: Vec<&'a ArcMultilinearExtension<'a, E>>,
    ) -> Vec<ArcMultilinearExtension<'a, E>> {
        polys
            .into_iter()
            .map(|poly| {
                let range_poly: ArcMultilinearExtension<E> =
                    Arc::new(poly.get_ranged_mle(self.num_threads, thread_id));
                range_poly
            })
            .collect_vec()
    }

    pub fn get_all_range_polys(
        &self,
        poly: &'a ArcMultilinearExtension<'a, E>,
    ) -> Vec<ArcMultilinearExtension<'a, E>> {
        (0..self.num_threads)
            .map(|thread_id| {
                self.get_range_polys_by_thread_id(thread_id, vec![poly])
                    .remove(0)
            })
            .collect_vec()
    }

    pub fn add_mle_list(
        &mut self,
        thread_id: usize,
        polys: Vec<ArcMultilinearExtension<'a, E>>,
        coeff: E,
    ) {
        assert!(thread_id < self.polys.len());
        self.polys[thread_id].add_mle_list(polys, coeff);
    }

    pub fn get_batched_polys(self) -> Vec<VirtualPolynomialV2<'a, E>> {
        self.polys
    }

    /// add mle terms into virtual poly by expression
    /// return distinct witin in set
    pub fn add_mle_list_by_expr(
        &mut self,
        // thread_based selector
        selector: Option<Vec<ArcMultilinearExtension<'a, E>>>,
        // witin_id -> thread_id
        wit_ins: &[Vec<ArcMultilinearExtension<'a, E>>],
        expr: &Expression<E>,
        challenges: &[E],
        // sumcheck batch challenge
        alpha: E,
    ) -> BTreeSet<u16> {
        if let Some(sel) = &selector {
            assert_eq!(sel.len(), self.num_threads);
        }
        assert!(expr.is_monomial_form());
        let monomial_terms = expr.evaluate(
            &|witness_id| {
                vec![(E::ONE, {
                    let mut monomial_terms = BTreeSet::new();
                    monomial_terms.insert(witness_id);
                    monomial_terms
                })]
            },
            &|scalar| vec![(E::from(scalar), { BTreeSet::new() })],
            &|challenge_id, pow, scalar, offset| {
                let challenge = challenges[challenge_id as usize];
                vec![(
                    challenge.pow([pow as u64]) * scalar + offset,
                    BTreeSet::new(),
                )]
            },
            &|mut a, b| {
                a.extend(b);
                a
            },
            &|mut a, mut b| {
                assert!(a.len() <= 2);
                assert!(b.len() <= 2);
                // special logic to deal with scaledsum
                // scaledsum second parameter must be 0
                if a.len() == 2 {
                    assert!((a[1].0, a[1].1.is_empty()) == (E::ZERO, true));
                    a.truncate(1);
                }
                if b.len() == 2 {
                    assert!((b[1].0, b[1].1.is_empty()) == (E::ZERO, true));
                    b.truncate(1);
                }

                a[0].1.extend(mem::take(&mut b[0].1));
                // return [ab]
                vec![(a[0].0 * b[0].0, mem::take(&mut a[0].1))]
            },
            &|mut x, a, b| {
                assert!(a.len() == 1 && a[0].1.is_empty()); // for challenge or constant, term should be empty
                assert!(b.len() == 1 && b[0].1.is_empty()); // for challenge or constant, term should be empty
                assert!(x.len() == 1 && (x[0].0, x[0].1.len()) == (E::ONE, 1)); // witin size only 1
                if b[0].0 == E::ZERO {
                    // only include first term if b = 0
                    vec![(a[0].0, mem::take(&mut x[0].1))]
                } else {
                    // return [ax, b]
                    vec![(a[0].0, mem::take(&mut x[0].1)), (b[0].0, BTreeSet::new())]
                }
            },
        );
        for (constant, monomial_term) in monomial_terms.iter() {
            if *constant != E::ZERO && monomial_term.is_empty() {
                todo!("make virtual poly support pure constant")
            }
            for thread_id in 0..self.num_threads {
                let sel = selector
                    .as_ref()
                    .map(|sel| vec![sel[thread_id].clone()])
                    .unwrap_or_default();
                let terms_polys = monomial_term
                    .iter()
                    .map(|wit_id| wit_ins[*wit_id as usize][thread_id].clone())
                    .collect_vec();

                self.add_mle_list(thread_id, [sel, terms_polys].concat(), *constant * alpha);
            }
        }

        monomial_terms
            .into_iter()
            .flat_map(|(_, monomial_term)| monomial_term.into_iter().collect_vec())
            .collect::<BTreeSet<u16>>()
    }
}

#[cfg(test)]
mod tests {

    use goldilocks::{Goldilocks, GoldilocksExt2};
    use multilinear_extensions::{mle::IntoMLE, virtual_poly_v2::ArcMultilinearExtension};

    use crate::{
        circuit_builder::CircuitBuilder,
        expression::{Expression, ToExpr},
        structs::VirtualPolynomials,
    };

    #[test]
    fn test_add_mle_list_by_expr() {
        type E = GoldilocksExt2;
        let mut cb = CircuitBuilder::<E>::new();
        let x = cb.create_witin();
        let y = cb.create_witin();

        let wits_in: Vec<ArcMultilinearExtension<E>> = (0..cb.num_witin as usize)
            .map(|_| vec![Goldilocks::from(1)].into_mle().into())
            .collect();

        let mut virtual_polys = VirtualPolynomials::new(1, 0);
        let wits_threads: Vec<Vec<ArcMultilinearExtension<E>>> = wits_in
            .iter()
            .map(|wit_poly| virtual_polys.get_all_range_polys(wit_poly))
            .collect();

        // 3xy + 2y
        let expr: Expression<E> =
            Expression::from(3) * x.expr() * y.expr() + Expression::from(2) * y.expr();

        let distrinct_zerocheck_terms_set =
            virtual_polys.add_mle_list_by_expr(None, &wits_threads, &expr, &[], 1.into());
        assert!(distrinct_zerocheck_terms_set.len() == 2);
    }
}
