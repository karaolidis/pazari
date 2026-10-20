use std::{fmt, io, net::SocketAddr, sync::Arc, time::Duration};

use bytes::Bytes;
use http_body_util::Full;
use hyper::{
    Method, Request, Response, StatusCode,
    body::Incoming,
    header::{CONTENT_TYPE, HeaderValue},
    server::conn::http1,
    service::service_fn,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use pazari_types::Backoff;
use prometheus_client::registry::Registry;
use tokio::{net::TcpListener, time::sleep};
use tracing::{debug, warn};

use crate::{Encoded, error::MetricsError};

const CONTENT: &str = "application/openmetrics-text; version=1.0.0; charset=utf-8";

const HEAD_LIMIT: usize = 8 << 10;

/// The first wait after a failed accept, which doubles while accepts keep failing.
const ACCEPT_RETRY_MIN: Duration = Duration::from_millis(10);

const ACCEPT_RETRY_MAX: Duration = Duration::from_secs(1);

#[derive(Debug)]
pub struct Exposition {
    listener: TcpListener,
    registry: Arc<Registry>,
}

impl Exposition {
    pub async fn bind(address: SocketAddr, registry: Registry) -> Result<Self, MetricsError> {
        let listener = TcpListener::bind(address)
            .await
            .map_err(|source| MetricsError::Bind { address, source })?;

        Ok(Self {
            listener,
            registry: Arc::new(registry),
        })
    }

    pub fn local(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    pub async fn serve(self) {
        let mut backoff = Backoff::new(ACCEPT_RETRY_MIN, ACCEPT_RETRY_MAX);

        loop {
            match self.listener.accept().await {
                Ok((connection, _)) => {
                    backoff.reset();

                    let registry = Arc::clone(&self.registry);

                    tokio::spawn(async move {
                        let service = service_fn(move |request| {
                            let registry = Arc::clone(&registry);

                            async move { Self::answer(&request, &registry) }
                        });

                        if let Err(error) = http1::Builder::new()
                            .timer(TokioTimer::new())
                            .max_buf_size(HEAD_LIMIT)
                            .serve_connection(TokioIo::new(connection), service)
                            .await
                        {
                            debug!(%error, "scrape failed");
                        }
                    });
                }
                Err(error) => {
                    warn!(%error, "failed to accept a scrape connection");

                    sleep(backoff.delay()).await;
                }
            }
        }
    }

    fn answer(
        request: &Request<Incoming>,
        registry: &Registry,
    ) -> Result<Response<Full<Bytes>>, fmt::Error> {
        if request.method() != Method::GET {
            let mut refused = Response::new(Full::default());

            *refused.status_mut() = StatusCode::METHOD_NOT_ALLOWED;

            return Ok(refused);
        }

        let mut response = Response::new(Full::new(Bytes::from(registry.encoded()?)));

        response
            .headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static(CONTENT));

        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use std::{net::Ipv4Addr, time::Duration};

    use prometheus_client::metrics::counter::Counter;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
        task::JoinHandle,
        time::timeout,
    };

    use super::*;

    const PATIENCE: Duration = Duration::from_secs(5);

    const SCRAPES: u64 = 7;

    const GARBAGE: usize = 64 << 10;

    struct Served {
        address: SocketAddr,
        task: JoinHandle<()>,
    }

    impl Drop for Served {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    fn registry() -> Registry {
        let mut registry = Registry::with_prefix("pazari_test");

        let scrapes = Counter::<u64>::default();
        scrapes.inc_by(SCRAPES);

        registry.register("scrapes", "Scrapes the fixture stands in for", scrapes);

        registry
    }

    async fn served() -> Served {
        let exposition = Exposition::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), registry())
            .await
            .expect("a bound endpoint");

        let address = exposition.local().expect("a bound address");

        Served {
            address,
            task: tokio::spawn(exposition.serve()),
        }
    }

    async fn ask(address: SocketAddr, request: &[u8]) -> String {
        let mut connection = TcpStream::connect(address)
            .await
            .expect("the endpoint answers");

        let exchange = async {
            drop(connection.write_all(request).await);

            let mut answer = Vec::new();
            drop(connection.read_to_end(&mut answer).await);

            assert_eq!(
                connection.read(&mut [0_u8; 1]).await.unwrap_or(0),
                0,
                "the endpoint held the connection open after answering",
            );

            answer
        };

        let answer = timeout(PATIENCE, exchange)
            .await
            .expect("the endpoint answered in time");

        String::from_utf8_lossy(&answer).into_owned()
    }

    async fn scrape(address: SocketAddr) -> String {
        ask(address, b"GET /metrics HTTP/1.0\r\n\r\n").await
    }

    #[tokio::test]
    async fn a_scrape_carries_the_registry_in_the_exposition_format() {
        let served = served().await;

        let answer = scrape(served.address).await;

        assert!(
            answer.starts_with("HTTP/1.0 200 OK\r\n"),
            "unexpected answer: {answer}"
        );
        assert!(
            answer.contains(
                "content-type: application/openmetrics-text; version=1.0.0; charset=utf-8\r\n"
            ),
            "unexpected answer: {answer}"
        );
        assert!(
            answer.contains(&format!("pazari_test_scrapes_total {SCRAPES}\n")),
            "unexpected answer: {answer}"
        );
        assert!(
            answer.ends_with("# EOF\n"),
            "the exposition is unterminated: {answer}"
        );
    }

    #[tokio::test]
    async fn anything_but_a_scrape_is_turned_away() {
        let served = served().await;

        let answer = ask(
            served.address,
            b"POST /metrics HTTP/1.0\r\nContent-Length: 0\r\n\r\n",
        )
        .await;

        assert!(
            answer.starts_with("HTTP/1.0 405 Method Not Allowed\r\n"),
            "unexpected answer: {answer}"
        );
        assert!(
            !answer.contains("pazari_test_scrapes_total"),
            "a write was answered with the exposition: {answer}"
        );
    }

    #[tokio::test]
    async fn a_request_head_that_never_ends_is_cut_off_rather_than_buffered() {
        let served = served().await;

        let answer = ask(served.address, &vec![b'x'; GARBAGE]).await;

        assert!(
            answer.is_empty() || answer.contains(" 431 "),
            "unexpected answer: {answer}"
        );
        assert!(
            !answer.contains("pazari_test_scrapes_total"),
            "a request that never asked was answered with the exposition: {answer}"
        );
        assert!(
            scrape(served.address)
                .await
                .contains("pazari_test_scrapes_total"),
            "one endless request took the endpoint down with it",
        );
    }

    #[tokio::test]
    async fn scrapes_that_arrive_together_are_all_answered() {
        let served = served().await;

        let (first, second) = tokio::join!(scrape(served.address), scrape(served.address));

        assert!(first.contains("pazari_test_scrapes_total"));
        assert!(second.contains("pazari_test_scrapes_total"));
    }
}
