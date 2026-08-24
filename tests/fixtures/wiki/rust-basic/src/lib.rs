pub mod config;
pub mod service;

pub fn run() -> String {
    service::greet("fixture")
}
