//! Pitch, note, rhythm and tempo logic.

pub mod bpm;
pub mod note;
pub mod quantize;
pub mod segment;

/// Returns the median of `values`, or `None` when `values` is empty.
///
/// An even count averages the two middle values of the sorted values.
pub(crate) fn median(mut values: Vec<f64>) -> Option<f64> {
    let len = values.len();
    if len == 0 {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let mid = len / 2;
    Some(if len % 2 == 1 {
        values[mid]
    } else {
        (values[mid - 1] + values[mid]) / 2.0
    })
}

/// Asserts that `a` and `b` differ by at most `tol`.
#[cfg(test)]
pub(crate) fn assert_close(a: f64, b: f64, tol: f64) {
    assert!(
        (a - b).abs() <= tol,
        "{a} and {b} differ by more than {tol}"
    );
}
