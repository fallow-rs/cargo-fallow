//! The suffix array, the LCP array and the repeat intervals of a sequence.
//!
//! The suffix array uses prefix doubling with a counting sort, so it takes
//! `O(n log n)` time. The LCP array uses the Kasai algorithm. The intervals
//! come from one pass over the LCP array with a stack.

/// The start of each suffix of `text`, in sorted order. Each value of `text`
/// must be less than `alphabet`.
pub fn suffix_array(text: &[u32], alphabet: usize) -> Vec<u32> {
    let n = text.len();
    if n == 0 {
        return Vec::new();
    }
    let mut order: Vec<u32> = (0..to_u32(n)).collect();
    counting_sort(&mut order, |i| text[i as usize] as usize, alphabet);
    let mut rank: Vec<u32> = text.to_vec();
    let mut next_rank = vec![0_u32; n];
    let mut second: Vec<u32> = Vec::with_capacity(n);
    let mut step = 1_usize;
    loop {
        // Sort by the second half: a suffix with no second half comes first.
        second.clear();
        second.extend((n.saturating_sub(step)..n).map(to_u32));
        second.extend(
            order
                .iter()
                .filter(|&&start| start as usize >= step)
                .map(|&start| start - to_u32(step)),
        );
        let classes = rank.iter().copied().max().map_or(1, |max| max as usize + 1);
        counting_sort_from(&second, &mut order, |i| rank[i as usize] as usize, classes);
        let key = |i: u32| {
            let i = i as usize;
            let tail = rank.get(i + step).map_or(0, |value| value + 1);
            (rank[i], tail)
        };
        next_rank[order[0] as usize] = 0;
        for pair in order.windows(2) {
            let previous = next_rank[pair[0] as usize];
            next_rank[pair[1] as usize] = previous + u32::from(key(pair[0]) != key(pair[1]));
        }
        std::mem::swap(&mut rank, &mut next_rank);
        if rank[order[n - 1] as usize] as usize == n - 1 || step >= n {
            break;
        }
        step *= 2;
    }
    order
}

fn to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn counting_sort(items: &mut Vec<u32>, key: impl Fn(u32) -> usize, classes: usize) {
    let input = std::mem::take(items);
    *items = vec![0; input.len()];
    counting_sort_from(&input, items, key, classes);
}

/// Sorts `input` by `key` into `output`, stable.
fn counting_sort_from(
    input: &[u32],
    output: &mut [u32],
    key: impl Fn(u32) -> usize,
    classes: usize,
) {
    let mut counts = vec![0_usize; classes + 1];
    for &item in input {
        counts[key(item) + 1] += 1;
    }
    for index in 1..counts.len() {
        counts[index] += counts[index - 1];
    }
    for &item in input {
        let slot = &mut counts[key(item)];
        output[*slot] = item;
        *slot += 1;
    }
}

/// The LCP array: `lcp[i]` is the length of the common prefix of the suffixes
/// at `order[i - 1]` and `order[i]`. `lcp[0]` is 0.
pub fn lcp_array(text: &[u32], order: &[u32]) -> Vec<u32> {
    let n = text.len();
    let mut rank = vec![0_usize; n];
    for (index, &start) in order.iter().enumerate() {
        rank[start as usize] = index;
    }
    let mut lcp = vec![0_u32; n];
    let mut common = 0_usize;
    for start in 0..n {
        if rank[start] == 0 {
            common = 0;
            continue;
        }
        let other = order[rank[start] - 1] as usize;
        while start + common < n
            && other + common < n
            && text[start + common] == text[other + common]
        {
            common += 1;
        }
        lcp[rank[start]] = to_u32(common);
        common = common.saturating_sub(1);
    }
    lcp
}

/// A repeat: the length of the common prefix and the suffix array range of the
/// suffixes that share it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interval {
    /// The length of the repeat.
    pub length: u32,
    /// The first index in the suffix array.
    pub first: usize,
    /// The last index in the suffix array.
    pub last: usize,
}

/// Each LCP interval with a length of at least `min_length`. An interval whose
/// suffixes all follow the same value is left out: the interval one value
/// earlier holds the same repeat with one more value.
pub fn intervals(text: &[u32], order: &[u32], lcp: &[u32], min_length: u32) -> Vec<Interval> {
    let n = order.len();
    let mut found = Vec::new();
    let mut stack: Vec<(u32, usize)> = vec![(0, 0)];
    for index in 1..=n {
        let current = if index < n { lcp[index] } else { 0 };
        let mut first = index - 1;
        while let Some(&(length, start)) = stack.last() {
            if current >= length {
                break;
            }
            stack.pop();
            if length >= min_length && left_maximal(text, &order[start..index]) {
                found.push(Interval {
                    length,
                    first: start,
                    last: index - 1,
                });
            }
            first = start;
        }
        if stack.last().is_none_or(|&(length, _)| current > length) {
            stack.push((current, first));
        }
    }
    found
}

fn left_maximal(text: &[u32], starts: &[u32]) -> bool {
    let mut before = starts
        .iter()
        .map(|&start| (start as usize).checked_sub(1).map(|at| text[at]));
    let Some(Some(first)) = before.next() else {
        return true;
    };
    before.any(|value| value != Some(first))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive(text: &[u32]) -> Vec<u32> {
        let mut order: Vec<u32> = (0..to_u32(text.len())).collect();
        order.sort_by(|&a, &b| text[a as usize..].cmp(&text[b as usize..]));
        order
    }

    #[test]
    fn suffix_array_matches_a_plain_sort() {
        let cases: [&[u32]; 5] = [
            &[1, 0, 2, 1, 0, 2, 1, 0],
            &[0, 0, 0, 0, 0],
            &[3, 2, 1, 0],
            &[1],
            &[2, 1, 2, 1, 2, 1, 3, 2, 1, 2, 1, 0],
        ];
        for text in cases {
            assert_eq!(suffix_array(text, 4), naive(text), "{text:?}");
        }
        let long: Vec<u32> = (0..500).map(|i| (i * 7 + i / 13) % 5).collect();
        assert_eq!(suffix_array(&long, 5), naive(&long));
    }

    #[test]
    fn lcp_is_the_common_prefix_of_neighbors() {
        let text = [1, 0, 2, 1, 0, 2, 3];
        let order = suffix_array(&text, 4);
        let lcp = lcp_array(&text, &order);
        for index in 1..order.len() {
            let a = &text[order[index - 1] as usize..];
            let b = &text[order[index] as usize..];
            let common = a.iter().zip(b).take_while(|(x, y)| x == y).count();
            assert_eq!(lcp[index] as usize, common);
        }
    }

    #[test]
    fn intervals_give_the_maximal_repeats_only() {
        // `1 2 3 4` repeats twice, with unique values around it.
        let text = [5, 1, 2, 3, 4, 6, 1, 2, 3, 4, 7];
        let order = suffix_array(&text, 8);
        let lcp = lcp_array(&text, &order);
        let found = intervals(&text, &order, &lcp, 2);
        assert_eq!(found.len(), 1, "{found:?}");
        let mut starts: Vec<u32> = order[found[0].first..=found[0].last].to_vec();
        starts.sort_unstable();
        assert_eq!((found[0].length, starts), (4, vec![1, 6]));
    }
}
