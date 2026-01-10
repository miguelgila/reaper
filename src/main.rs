fn main() {
    println!("Hello, world!");
    println!("Welcome to reaper!");
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        let result = 2 + 2;
        assert_eq!(result, 4);
    }
}
