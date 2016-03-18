//! High level construction of sparse matrices by stacking, by block, ...

use std::default::Default;
use std::cmp;
use sparse::csmat::{CompressedStorage, CsMatOwned, CsMatView};
use errors::SprsError;
use ndarray::{ArrayView, Ix};
use num::traits::{Num, Signed};

/// Stack the given matrices into a new one, using the most efficient stacking
/// direction (ie vertical stack for CSR matrices, horizontal stack for CSC)
pub fn same_storage_fast_stack<'a, N, MatArray>(
    mats: &MatArray) -> Result<CsMatOwned<N>, SprsError>
where N: 'a + Clone,
      MatArray: AsRef<[CsMatView<'a, N>]> {
    let mats = mats.as_ref();
    if mats.len() == 0 {
        return Err(SprsError::EmptyStackingList);
    }
    let inner_dim = mats[0].inner_dims();
    if ! mats.iter().all(|x| x.inner_dims() == inner_dim) {
        return Err(SprsError::IncompatibleDimensions);
    }
    let storage_type = mats[0].storage();
    if ! mats.iter().all(|x| x.storage() == storage_type) {
        return Err(SprsError::IncompatibleStorages);
    }

    let outer_dim = mats.iter().map(|x| x.outer_dims()).fold(0, |x, y| x + y);
    let nnz = mats.iter().map(|x| x.nb_nonzero()).fold(0, |x, y| x + y);

    let mut res = CsMatOwned::empty(storage_type, inner_dim);
    res.reserve_outer_dim_exact(outer_dim);
    res.reserve_nnz_exact(nnz);
    for mat in mats {
        for vec in mat.outer_iterator() {
            res = res.append_outer_csvec(vec.view());
        }
    }

    Ok(res)
}

/// Construct a sparse matrix by vertically stacking other matrices
pub fn vstack<'a, N, MatArray>(mats: &MatArray) -> Result<CsMatOwned<N>, SprsError>
where N: 'a + Clone + Default,
      MatArray: AsRef<[CsMatView<'a, N>]> {
    let mats = mats.as_ref();
    if mats.iter().all(|x| x.is_csr()) {
        return same_storage_fast_stack(&mats);
    }

    let mats_csr: Vec<_> = mats.iter().map(|x| x.to_csr()).collect();
    let mats_csr_views: Vec<_> = mats_csr.iter().map(|x| x.view()).collect();
    same_storage_fast_stack(&mats_csr_views)
}

/// Construct a sparse matrix by horizontally stacking other matrices
pub fn hstack<'a, N, MatArray>(mats: &MatArray) -> Result<CsMatOwned<N>, SprsError>
where N: 'a + Clone + Default,
      MatArray: AsRef<[CsMatView<'a, N>]> {
    let mats = mats.as_ref();
    if mats.iter().all(|x| x.is_csc()) {
        return same_storage_fast_stack(&mats);
    }

    let mats_csc: Vec<_> = mats.iter().map(|x| x.to_csc()).collect();
    let mats_csc_views: Vec<_> = mats_csc.iter().map(|x| x.view()).collect();
    same_storage_fast_stack(&mats_csc_views)
}

/// Specify a sparse matrix by constructing it from blocks of other matrices
///
/// # Examples
/// ```
/// use sprs::sparse::CompressedStorage::CSR;
/// use sprs::CsMatOwned;
/// let a = CsMatOwned::<f64>::eye(CSR, 3);
/// let b = CsMatOwned::<f64>::eye(CSR, 4);
/// let c = sprs::bmat(&[[Some(a.view()), None],
///                      [None, Some(b.view())]]).unwrap();
/// assert_eq!(c.rows(), 7);
/// ```
pub fn bmat<'a, N, OuterArray, InnerArray>(mats: &OuterArray)
-> Result<CsMatOwned<N>, SprsError>
where N: 'a + Clone + Default,
      OuterArray: 'a + AsRef<[InnerArray]>,
      InnerArray: 'a + AsRef<[Option<CsMatView<'a, N>>]> {
    let mats = mats.as_ref();
    let super_rows = mats.len();
    if super_rows == 0 {
        return Err(SprsError::EmptyStackingList);
    }
    let super_cols = mats[0].as_ref().len();
    if super_cols == 0 {
        return Err(SprsError::EmptyStackingList);
    }

    // check input has matrix shape
    if ! mats.iter().all(|x| x.as_ref().len() == super_cols) {
        return Err(SprsError::IncompatibleDimensions);
    }

    if mats.iter().any(|x| x.as_ref().iter().all(|y| y.is_none())) {
        return Err(SprsError::EmptyBmatRow);
    }
    if (0..super_cols).any(|j| mats.iter().all(|x| x.as_ref()[j].is_none())) {
        return Err(SprsError::EmptyBmatCol);
    }

    // find out the shapes of the None elements
    let rows_per_row: Vec<_> = mats.iter().map(|row| {
        row.as_ref().iter().fold(0, |nrows, mopt| {
            mopt.as_ref().map_or(nrows, |m| cmp::max(nrows, m.rows()))
        })
    }).collect();
    let cols_per_col: Vec<_> = (0..super_cols).map(|j| {
        mats.iter().fold(0, |ncols, row| {
            row.as_ref()[j].as_ref()
                           .map_or(ncols, |m| cmp::max(ncols, m.cols()))
        })
    }).collect();
    let mut to_vstack = Vec::new();
    to_vstack.reserve(super_rows);
    for (i, row) in mats.iter().enumerate() {
        let with_zeros: Vec<_> = row.as_ref().iter().enumerate().map(|(j, m)| {
            m.as_ref().map_or(CsMatOwned::zero(rows_per_row[i], cols_per_col[j]),
                              |x| x.to_owned())
        }).collect();
        let borrows: Vec<_> = with_zeros.iter().map(|x| x.view()).collect();
        let stacked = try!(hstack(&borrows));
        to_vstack.push(stacked);
    }
    let borrows: Vec<_> = to_vstack.iter().map(|x| x.view()).collect();
    vstack(&borrows)
}

/// Create a CSR matrix from a dense matrix, ignoring elements
/// lower than `epsilon`.
///
/// If epsilon is negative, it will be clamped to zero.
pub fn csr_from_dense<N>(m: ArrayView<N, (Ix, Ix)>, epsilon: N) -> CsMatOwned<N>
where N: Num + Clone + cmp::PartialOrd + Signed
{
    let epsilon = if epsilon > N::zero() { epsilon } else { N::zero() };
    let rows = m.shape()[0];
    let cols = m.shape()[1];

    let mut indptr = vec![0; rows + 1];
    let mut nnz = 0;
    for (row, row_count) in m.outer_iter().zip(&mut indptr[1..]) {
        nnz += row.iter().filter(|&x| x.abs() > epsilon).count();
        *row_count = nnz;
    }

    let mut indices = Vec::with_capacity(nnz);
    let mut data = Vec::with_capacity(nnz);
    for row in m.outer_iter() {
        for (col_ind, x) in row.iter().enumerate() {
            if x.abs() > epsilon {
                indices.push(col_ind);
                data.push(x.clone());
            }
        }
    }
    // TODO: the structure check are not necessary here
    CsMatOwned::new_owned(CompressedStorage::CSR,
                          rows,
                          cols,
                          indptr,
                          indices,
                          data).unwrap()
}

/// Create a CSC matrix from a dense matrix, ignoring elements
/// lower than `epsilon`.
///
/// If epsilon is negative, it will be clamped to zero.
pub fn csc_from_dense<N>(m: ArrayView<N, (Ix, Ix)>,
                         epsilon: N
                        ) -> CsMatOwned<N>
where N: Num + Clone + cmp::PartialOrd + Signed
{
    csr_from_dense(m.reversed_axes(), epsilon).transpose_into()
}

#[cfg(test)]
mod test {
    use sparse::csmat::CsMatOwned;
    use sparse::CompressedStorage::{CSR, CSC};
    use test_data::{mat1, mat2, mat3, mat4};
    use errors::SprsError::*;
    use ndarray::{arr2, OwnedArray};

    fn mat1_vstack_mat2() -> CsMatOwned<f64> {
        let indptr = vec![0, 2, 4, 5, 6, 7, 11, 13, 13, 15, 17];
        let indices = vec![2, 3, 3, 4, 2, 1, 3, 0, 1, 2, 4, 0, 3, 2, 3, 1, 2];
        let data = vec![3., 4., 2., 5., 5., 8., 7., 6., 7., 3., 3.,
                        8., 9., 2., 4., 4., 4.];
        CsMatOwned::new_owned(CSR, 10, 5, indptr, indices, data).unwrap()
    }

    #[test]
    fn same_storage_fast_stack_failures() {
        let res: Result<CsMatOwned<f64>, _> =
            super::same_storage_fast_stack(&[]);
        assert_eq!(res, Err(EmptyStackingList));
        let a = mat1();
        let c = mat3();
        let d = mat4();
        let _: Result<CsMatOwned<f64>, _> = super::same_storage_fast_stack(&[]);
        let res = super::same_storage_fast_stack(&[a.view(), c.view()]);
        assert_eq!(res, Err(IncompatibleDimensions));
        let res = super::same_storage_fast_stack(&[a.view(), d.view()]);
        assert_eq!(res, Err(IncompatibleStorages));
    }

    #[test]
    fn same_storage_fast_stack_ok() {
        let a = mat1();
        let b = mat2();
        let res = super::same_storage_fast_stack(&[a.view(), b.view()]);
        let expected = mat1_vstack_mat2();
        assert_eq!(res, Ok(expected));
    }

    #[test]
    fn vstack_trivial() {
        let a = mat1();
        let b = mat2();
        let res = super::vstack(&[a.view(), b.view()]);
        let expected = mat1_vstack_mat2();
        assert_eq!(res, Ok(expected));
    }

    #[test]
    fn hstack_trivial() {
        let a = mat1().transpose_into();
        let b = mat2().transpose_into();
        let res = super::hstack(&[a.view(), b.view()]);
        let expected = mat1_vstack_mat2().transpose_into();
        assert_eq!(res, Ok(expected));
    }

    #[test]
    fn vstack_with_conversion() {
        let a = mat1().to_csc();
        let b = mat2();
        let res = super::vstack(&[a.view(), b.view()]);
        let expected = mat1_vstack_mat2();
        assert_eq!(res, Ok(expected));
    }

    #[test]
    fn bmat_failures() {
        let res: Result<CsMatOwned<f64>, _> =
            super::bmat(&[[]]);
        assert_eq!(res, Err(EmptyStackingList));
        let a = mat1();
        let c = mat3();
        let res: Result<CsMatOwned<f64>,_> = super::bmat(
            &vec![vec![None, None], vec![None]]);
        assert_eq!(res, Err(IncompatibleDimensions));
        let res: Result<CsMatOwned<f64>, _> =
            super::bmat(&[[None, None],
                          [Some(a.view()), Some(c.view())]]);
        assert_eq!(res, Err(EmptyBmatRow));
        let res: Result<CsMatOwned<f64>, _> =
            super::bmat(&[[Some(c.view()), None],
                          [Some(a.view()), None]]);
        assert_eq!(res, Err(EmptyBmatCol));
    }

    #[test]
    fn bmat_simple() {
        let a = CsMatOwned::<f64>::eye(CSR, 5);
        let b = CsMatOwned::<f64>::eye(CSR, 4);
        let c = super::bmat(&[[Some(a.view()), None],
                              [None, Some(b.view())]]).unwrap();
        let expected = CsMatOwned::new_owned(
            CSR, 9, 9,
            vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
            vec![0, 1, 2, 3, 4, 5, 6, 7, 8],
            vec![1.; 9]).unwrap();
        assert_eq!(c, expected);
    }

    #[test]
    fn bmat_complex() {
        let a = mat1();
        let b = mat2();
        let c = super::bmat(&[[Some(a.view()), Some(b.view())],
                              [Some(b.view()), None]]).unwrap();
        let expected = CsMatOwned::new_owned(
            CSR, 10, 10,
            vec![0,  6, 10, 11, 14, 17, 21, 23, 23, 25, 27],
            vec![2, 3, 5, 6, 7, 9, 3, 4, 5, 8, 2, 1, 7, 8, 3,
                 6, 7, 0, 1, 2, 4, 0, 3, 2, 3, 1, 2],
            vec![3., 4., 6., 7., 3., 3., 2., 5., 8., 9., 5., 8., 2., 4.,
                 7., 4., 4., 6., 7., 3., 3., 8., 9., 2., 4., 4., 4.]).unwrap();
        assert_eq!(c, expected);

        let d = mat3();
        let e = mat4();
        let f = super::bmat(&[[Some(d.view()), Some(a.view())],
                              [None, Some(e.view())]]
                           ).unwrap();
        let expected = CsMatOwned::new_owned(
            CSR, 10, 9,
            vec![0, 4, 8, 10, 12, 14, 16, 18, 21, 23, 24],
            vec![2, 3, 6, 7, 2, 3, 7, 8, 2, 6, 1, 5, 3, 7, 4,
                 5, 4, 8, 4, 7, 8, 5, 7, 4],
            vec![3., 4., 3., 4., 2., 5., 2., 5., 5., 5., 8., 8.,
                 7., 7., 6., 8., 7., 4., 3., 2., 4., 9., 4., 3.]).unwrap();
        assert_eq!(f, expected);
    }

    #[test]
    fn csr_from_dense() {
        let m = OwnedArray::eye(3);
        let m_sparse = super::csr_from_dense(m.view(), 0.);

        assert_eq!(m_sparse, CsMatOwned::eye(CSR, 3));

        let m = arr2(&[[1., 0., 2., 1e-7, 1.],
                       [0., 0., 0., 1.,   0.],
                       [3., 0., 1., 0.,   0.]]);
        let m_sparse = super::csr_from_dense(m.view(), 1e-5);

        let expected_output = CsMatOwned::new_owned(CSR,
                                                    3,
                                                    5,
                                                    vec![0, 3, 4, 6],
                                                    vec![0, 2, 4, 3, 0, 2],
                                                    vec![1., 2., 1., 1., 3., 1.]
                                                   ).unwrap();

        assert_eq!(m_sparse, expected_output);
    }

    #[test]
    fn csc_from_dense() {
        let m = OwnedArray::eye(3);
        let m_sparse = super::csc_from_dense(m.view(), 0.);

        assert_eq!(m_sparse, CsMatOwned::eye(CSC, 3));

        let m = arr2(&[[1., 0., 2., 1e-7, 1.],
                       [0., 0., 0., 1.,   0.],
                       [3., 0., 1., 0.,   0.]]);
        let m_sparse = super::csc_from_dense(m.view(), 1e-5);

        let expected_output = CsMatOwned::new_owned(CSC,
                                                    3,
                                                    5,
                                                    vec![0, 2, 2, 4, 5, 6],
                                                    vec![0, 2, 0, 2, 1, 0],
                                                    vec![1., 3., 2., 1., 1., 1.]
                                                   ).unwrap();

        assert_eq!(m_sparse, expected_output);
    }
}