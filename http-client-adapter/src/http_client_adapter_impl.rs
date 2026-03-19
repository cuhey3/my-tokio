use bytes::Bytes;
use http::{Method, StatusCode};
use http_client_if::http_client_adapter::HttpClientAdapter;
use reqwest::{Client, ClientBuilder};
use serde::{Deserialize, Serialize};

pub struct HttpClientAdapterImpl {
    status_code: Option<StatusCode>,
    bytes: Option<Bytes>,
}

impl HttpClientAdapterImpl {
    fn get_https_client() -> reqwest::Result<Client> {
        ClientBuilder::new().build()
    }
}

impl HttpClientAdapter for HttpClientAdapterImpl {
    fn new() -> Self {
        Self {
            status_code: None,
            bytes: None,
        }
    }

    async fn send_json<T: Serialize + Send + Sync>(
        &mut self,
        full_url: &str,
        data: &T,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let client = Self::get_https_client()?;

        let response = client
            .request(Method::POST, full_url)
            .json(data)
            .send()
            .await?;

        self.status_code = Some(response.status());

        self.bytes = Some(response.bytes().await?);

        Ok(())
    }

    fn status_is_ok(&self) -> Result<bool, String> {
        if self.status_code.is_none() {
            return Err("status code has not yet been set".to_owned());
        };

        Ok(self.status_code == Some(StatusCode::OK))
    }

    fn parse_response_json<'a, T: Deserialize<'a> + Send>(&'a self) -> Result<T, String> {
        serde_json::from_slice(self.bytes.as_ref().unwrap())
            .map_err(|err| format!("response to json error: {}", err))
    }

    fn get_status(&self) -> StatusCode {
        self.status_code.unwrap()
    }
}
