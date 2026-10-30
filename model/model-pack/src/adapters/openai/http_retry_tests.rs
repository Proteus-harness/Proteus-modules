use super::*;
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[test]
fn request_retry_config_matches_codex_limits_and_rejects_invalid_values() {
    for (config, expected) in [
        (json!({}), 4),
        (json!({"request_max_retries": 0}), 0),
        (json!({"request_max_retries": 2}), 2),
        (json!({"request_max_retries": 101}), 100),
    ] {
        assert_eq!(
            RequestRetry::from_config(&config).unwrap().max_retries,
            expected
        );
    }
    for value in [json!(-1), json!(1.5), json!("4"), json!(true), Value::Null] {
        assert!(RequestRetry::from_config(&json!({"request_max_retries": value})).is_err());
    }
}

async fn http_fixture(
    statuses: Vec<u16>,
) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let count = Arc::new(AtomicUsize::new(0));
    let requests = count.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                headers.push(socket.read_u8().await.unwrap());
            }
            let index = requests.fetch_add(1, Ordering::SeqCst);
            let status = statuses[index.min(statuses.len() - 1)];
            let body = format!("response-{index}");
            socket.write_all(format!(
                "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()
            ).as_bytes()).await.unwrap();
        }
    });
    (endpoint, count, server)
}

#[tokio::test]
async fn retryable_statuses_reuse_the_request_and_preserve_the_final_response() {
    for (statuses, retries, expected_status, expected_count) in [
        (vec![500, 502, 503, 200], 4, 200, 4),
        (vec![503], 1, 503, 2),
        (vec![500], 0, 500, 1),
        (vec![400], 4, 400, 1),
        (vec![401], 4, 401, 1),
        (vec![403], 4, 403, 1),
        (vec![429], 4, 429, 1),
    ] {
        let (url, count, server) = http_fixture(statuses).await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let response = RequestRetry {
            max_retries: retries,
        }
        .send(|| client.get(&url))
        .await
        .unwrap();
        assert_eq!(response.status().as_u16(), expected_status);
        assert_eq!(
            response.text().await.unwrap(),
            format!("response-{}", expected_count - 1)
        );
        assert_eq!(count.load(Ordering::SeqCst), expected_count);
        server.abort();
    }
}

#[tokio::test]
async fn noncloneable_transport_retries_are_bounded_and_invalid_requests_fail_before_retry() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    for (url, expected_count) in [(url.as_str(), 3), ("not a URL", 1)] {
        let mut attempts = 0;
        let error = RequestRetry { max_retries: 2 }
            .send(|| {
                attempts += 1;
                // Streaming bodies must retain the per-attempt builder path.
                client
                    .get(url)
                    .body(reqwest::Body::wrap_stream(futures_util::stream::once(
                        async { Ok::<_, std::io::Error>("fixture") },
                    )))
            })
            .await
            .unwrap_err();
        assert_eq!(attempts, expected_count);
        assert!(error.is_connect() || error.is_builder());
    }
}

#[tokio::test]
async fn retryable_json_is_serialized_once_and_every_attempt_keeps_identical_bytes() {
    struct CountedJson<'a> {
        serializations: &'a AtomicUsize,
        value: &'a Value,
    }
    impl serde::Serialize for CountedJson<'_> {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            self.serializations.fetch_add(1, Ordering::SeqCst);
            serde::Serialize::serialize(self.value, serializer)
        }
    }

    tokio::time::timeout(Duration::from_secs(10), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        // Both halves belong to this timeout, rather than a detached task:
        // timeout or panic drops the fixture listener/socket with the client.
        let server = async move {
            let mut received = Vec::new();
            for status in [503, 502, 200] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut headers = Vec::new();
                while !headers.ends_with(b"\r\n\r\n") {
                    headers.push(socket.read_u8().await.unwrap());
                }
                let headers = String::from_utf8(headers).unwrap();
                assert!(headers.starts_with("POST / HTTP/1.1\r\n"));
                let length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                let mut body = vec![0; length];
                socket.read_exact(&mut body).await.unwrap();
                received.push(body);
                socket
                    .write_all(
                        format!(
                            "HTTP/1.1 {status} Fixture\r\nContent-Length: 5\r\nConnection: close\r\n\r\nfinal"
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
            received
        };
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let value = json!({"input": "Текст\n\"quoted\"", "tools": [{"name": "fixture"}]});
        let serializations = AtomicUsize::new(0);
        let payload = CountedJson {
            serializations: &serializations,
            value: &value,
        };
        let send = async {
            let response = RequestRetry { max_retries: 2 }
                .send(|| client.post(&url).json(&payload))
                .await
                .unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::OK);
            response.text().await.unwrap()
        };
        let (response, received) = tokio::join!(send, server);
        assert_eq!(response, "final");
        assert_eq!(serializations.load(Ordering::SeqCst), 1);
        let expected = serde_json::to_vec(&value).unwrap();
        assert_eq!(received, vec![expected; 3]);
    })
    .await
    .expect("retry JSON loopback exchange must finish within 10 seconds");
}

#[tokio::test]
async fn serialization_errors_are_not_rebuilt_or_retried() {
    struct FailingJson<'a>(&'a AtomicUsize);
    impl serde::Serialize for FailingJson<'_> {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(serde::ser::Error::custom("fixture serialization failure"))
        }
    }

    let serializations = AtomicUsize::new(0);
    let payload = FailingJson(&serializations);
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let error = RequestRetry { max_retries: 2 }
        .send(|| client.post("http://127.0.0.1:1").json(&payload))
        .await
        .unwrap_err();
    assert!(error.is_builder());
    assert_eq!(serializations.load(Ordering::SeqCst), 1);
}
