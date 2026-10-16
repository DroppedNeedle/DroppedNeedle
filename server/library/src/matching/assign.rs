//! Optimal track assignment (Kuhn-Munkres), as beets and Lidarr do it.
//!
//! Greedy pairing goes wrong as soon as two local tracks both look like
//! the same release track; the Hungarian method finds the pairing with
//! the lowest total cost instead. Any local track may also stay
//! unmatched at a fixed cost, so a bad pair is never forced just because
//! a column was free.

/// Cost of a pair that must never be chosen.
const FORBIDDEN: f64 = 1.0e6;

/// Pair `rows` local tracks with `columns` release tracks. `cost(row,
/// column)` is the pair's distance (`None` forbids the pair) and
/// `unmatched` the cost of leaving a local track out. Returns, per
/// local track, the release track it took, if any.
pub fn assign(
    rows: usize,
    columns: usize,
    unmatched: f64,
    cost: impl Fn(usize, usize) -> Option<f64>,
) -> Vec<Option<usize>> {
    if rows == 0 {
        return Vec::new();
    }
    // Square matrix: real columns, then one "stay out" column per row;
    // the padding rows absorb whatever columns nobody took.
    let size = rows + columns;
    let mut matrix = vec![vec![0.0; size]; size];
    for (row, line) in matrix.iter_mut().enumerate().take(rows) {
        for (column, slot) in line.iter_mut().enumerate() {
            *slot = if column < columns {
                cost(row, column).unwrap_or(FORBIDDEN)
            } else if column - columns == row {
                unmatched
            } else {
                FORBIDDEN
            };
        }
    }
    let assignment = hungarian(&matrix);
    assignment
        .into_iter()
        .take(rows)
        .map(|column| (column < columns).then_some(column))
        .collect()
}

/// Minimum-cost perfect matching on a square matrix; returns the column
/// chosen for each row. Potentials-based O(n^3) form.
fn hungarian(matrix: &[Vec<f64>]) -> Vec<usize> {
    let size = matrix.len();
    let mut row_potential = vec![0.0; size + 1];
    let mut column_potential = vec![0.0; size + 1];
    // owner[column] = row (1-based) holding that column; 0 = free.
    let mut owner = vec![0usize; size + 1];
    let mut way = vec![0usize; size + 1];
    for row in 1..=size {
        owner[0] = row;
        let mut column = 0;
        let mut min_slack = vec![f64::INFINITY; size + 1];
        let mut used = vec![false; size + 1];
        loop {
            used[column] = true;
            let current_row = owner[column];
            let mut delta = f64::INFINITY;
            let mut next = 0;
            for candidate in 1..=size {
                if used[candidate] {
                    continue;
                }
                let slack = matrix[current_row - 1][candidate - 1]
                    - row_potential[current_row]
                    - column_potential[candidate];
                if slack < min_slack[candidate] {
                    min_slack[candidate] = slack;
                    way[candidate] = column;
                }
                if min_slack[candidate] < delta {
                    delta = min_slack[candidate];
                    next = candidate;
                }
            }
            for candidate in 0..=size {
                if used[candidate] {
                    row_potential[owner[candidate]] += delta;
                    column_potential[candidate] -= delta;
                } else {
                    min_slack[candidate] -= delta;
                }
            }
            column = next;
            if owner[column] == 0 {
                break;
            }
        }
        loop {
            let previous = way[column];
            owner[column] = owner[previous];
            column = previous;
            if column == 0 {
                break;
            }
        }
    }
    let mut chosen = vec![0; size];
    for column in 1..=size {
        if owner[column] > 0 {
            chosen[owner[column] - 1] = column - 1;
        }
    }
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beats_greedy_and_leaves_bad_pairs_out() {
        // Greedy takes (0,0) at 0.1 and is then forced into (1,1) at 0.9;
        // the optimum is (0,1) + (1,0) at 0.4.
        let costs = [[0.1, 0.2], [0.2, 0.9]];
        let pairs = assign(2, 2, 0.65, |row, column| Some(costs[row][column]));
        assert_eq!(pairs, vec![Some(1), Some(0)]);
        // A row whose only pair is forbidden stays out.
        let pairs = assign(2, 1, 0.65, |row, _| (row == 0).then_some(0.0));
        assert_eq!(pairs, vec![Some(0), None]);
    }
}
