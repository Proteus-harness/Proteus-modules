use std::time::Duration;

pub(super) async fn send_with_transport_retry(
    mut builder: reqwest::RequestBuilder,
) -> Result<reqwest::Response, reqwest::Error> {
    const MAX_RETRIES: usize = 2;

    let mut attempt = 0usize;
    loop {
        // A reusable JSON body shares its encoded bytes across builder clones.
        // Always send the original builder, including any stored builder error.
        let retry = if attempt < MAX_RETRIES {
            builder.try_clone()
        } else {
            None
        };
        match builder.send().await {
            Ok(response) => return Ok(response),
            Err(error) if should_retry_transport_error(&error) => {
                let Some(retry) = retry else {
                    return Err(error);
                };
                attempt += 1;
                tokio::time::sleep(retry_delay(attempt)).await;
                builder = retry;
            }
            Err(error) => return Err(error),
        }
    }
}

fn should_retry_transport_error(error: &reqwest::Error) -> bool {
    error.is_connect() || error.is_timeout() || error.is_body()
}

fn retry_delay(attempt: usize) -> Duration {
    Duration::from_millis(150 * attempt as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn classifies_connect_error_as_retryable() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind test port");
        let addr = listener.local_addr().expect("local addr");
        drop(listener);

        let error = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client")
            .get(format!("http://{addr}"))
            .send()
            .await
            .expect_err("closed local listener should reject connection");

        assert!(should_retry_transport_error(&error));
    }
}
