use bon::bon;
use docs_rs_headers::{CacheControl, testing::test_typed_encode};
use docs_rs_types::KrateName;
use http::{StatusCode, header::CACHE_CONTROL};

pub struct RustsecMockServer {
    server: mockito::ServerGuard,
    mocks: Vec<mockito::Mock>,
}

#[bon]
impl RustsecMockServer {
    pub async fn new() -> Self {
        Self {
            server: mockito::Server::new_async().await,
            mocks: Vec::new(),
        }
    }

    #[builder(start_fn(name = mock), finish_fn(name = start))]
    pub async fn create_mock(
        mut self,
        #[builder(start_fn)] krate: KrateName,
        #[builder(default = StatusCode::OK)] status_code: StatusCode,
        #[builder(default = false)] empty: bool,
        cache_control: Option<CacheControl>,
    ) -> Self {
        let mut mock = self
            .server
            .mock("GET", format!("/packages/{}.json", krate).as_str())
            .with_status(status_code.as_u16().into());

        if let Some(cache_control) = cache_control {
            let value = test_typed_encode(cache_control);
            mock = mock.with_header(CACHE_CONTROL, value.to_str().unwrap());
        }

        let empty = empty || (status_code.is_client_error() || status_code.is_server_error());

        self.mocks.push(
            mock.with_body(if empty {
                ""
            } else {
                include_str!("../../tests/fixtures/owned-alloc.json")
            })
            .create_async()
            .await,
        );

        self
    }

    pub fn config(&self) -> crate::ConfigBuilder {
        crate::Config::builder()
            .base_url(self.server.url().parse().unwrap())
            .max_retries(0)
    }

    pub async fn assert_async(self) {
        for mock in self.mocks {
            mock.assert_async().await;
        }
    }
}
