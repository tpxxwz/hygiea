pub use reqwest::{Client, ClientBuilder, Method, Response, StatusCode, Url};

pub fn client() -> ClientBuilder {
    Client::builder()
}
