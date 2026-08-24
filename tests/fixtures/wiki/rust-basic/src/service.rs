use crate::config::Config;

pub fn greet(name: &str) -> String {
    let _config = Config { retries: 2 };
    format!("hello {}", name)
}

#[cfg(test)]
mod tests {
    use super::greet;
    #[test]
    fn greets() {
        assert_eq!(greet("test"), "hello test");
    }
}
