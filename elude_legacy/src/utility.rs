pub fn is_sorted_set<T: PartialOrd>(slice: &[T]) -> bool {
    slice.windows(2).all(|values| values[0] < values[1])
}
