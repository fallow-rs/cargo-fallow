pub fn is_two(a: u32) -> bool {
    a == 2
}

pub fn add(a: u32, b: u32) -> u32 {
    a + b
}

#[cfg(test)]
mod tests {
    #[test]
    fn two() {
        assert!(super::is_two(2));
    }
}
