use std::cmp::Ordering;

pub fn is_sorted_set<T: PartialOrd>(slice: &[T]) -> bool {
    slice.windows(2).all(|values| values[0] < values[1])
}

pub fn sorted_difference<T: Ord + 'static>(left: &mut Vec<T>, right: &[T]) -> bool {
    if right.is_empty() {
        return false;
    }

    let count = left.len();
    let mut index = 0;
    left.retain(|left| {
        while let Some(right) = right.get(index) {
            match left.cmp(right) {
                Ordering::Less => break,
                Ordering::Equal => {
                    index += 1;
                    return false;
                }
                Ordering::Greater => index += 1,
            }
        }
        true
    });
    count > left.len()
}

pub fn sorted_intersects<T: Ord + 'static>(left: &[T], right: &[T]) -> bool {
    let mut indices = (0, 0);
    loop {
        match (left.get(indices.0), right.get(indices.1)) {
            (Some(left), Some(right)) => match left.cmp(right) {
                Ordering::Less => indices.0 += 1,
                Ordering::Equal => break true,
                Ordering::Greater => indices.1 += 1,
            },
            _ => break false,
        }
    }
}
