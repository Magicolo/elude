use std::{any::type_name, cmp::Ordering};

pub fn short_type_name<T: ?Sized>() -> String {
    let name = type_name::<T>();
    let mut buffer = String::with_capacity(name.len());
    let mut checkpoint = 0;
    let mut characters = name.chars();

    while let Some(character) = characters.next() {
        if character == ':' {
            match characters.next() {
                Some(':') => buffer.truncate(checkpoint),
                Some(character) => {
                    buffer.push(':');
                    buffer.push(character);
                    checkpoint = buffer.len();
                }
                None => {
                    buffer.push(':');
                    checkpoint = buffer.len();
                }
            }
        } else if character == '_' || character.is_alphanumeric() {
            buffer.push(character);
        } else {
            buffer.push(character);
            checkpoint = buffer.len();
        }
    }
    buffer
}

pub fn is_sorted_set<T: PartialOrd>(slice: &[T]) -> bool {
    slice.windows(2).all(|values| values[0] < values[1])
}

// pub fn sorted_difference<T: Ord + 'static>(left: &mut Vec<T>, right: &[T]) {
//     let mut index = 0;
//     left.retain(|left| {
//         while let Some(right) = right.get(index) {
//             match left.cmp(right) {
//                 Ordering::Less => return true,
//                 Ordering::Equal => {
//                     index += 1;
//                     return false;
//                 }
//                 Ordering::Greater => index += 1,
//             }
//         }
//         true
//     });
// }

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
