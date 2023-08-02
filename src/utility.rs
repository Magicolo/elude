use std::any::type_name;

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
