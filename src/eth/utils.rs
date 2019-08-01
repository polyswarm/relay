// From ethereum_types but not reexported by web3
pub fn clean_0x(s: &str) -> &str {
    if s.starts_with("0x") {
        &s[2..]
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_0x_from_addr() {
        // arrange
        // act
        let result = clean_0x("0xstring");
        // assert
        assert_eq!(result, "string");
    }

    #[test]
    fn no_mods_if_no_0x() {
        // arrange
        // act
        let result = clean_0x("string");
        // assert
        assert_eq!(result, "string");
    }
}
