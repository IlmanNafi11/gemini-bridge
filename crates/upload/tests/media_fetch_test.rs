use gemini_bridge_upload::{MediaFetchPolicy, MediaKind, UploadError, fetch_media};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nvalid png fixture";
const MP4: &[u8] = b"\x00\x00\x00\x18ftypisom\x00\x00\x00\x00isommp42";

async fn serve(response: Vec<u8>) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 2048];
        let _ = stream.read(&mut request).await;
        stream.write_all(&response).await.unwrap();
    });
    Url::parse(&format!("http://{addr}/media")).unwrap()
}

fn local_policy(max_bytes: u64) -> MediaFetchPolicy {
    let mut policy = MediaFetchPolicy::test_local();
    policy.max_bytes = max_bytes;
    policy
}

#[tokio::test]
async fn downloads_valid_image_and_video_signatures() {
    let image_body = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        PNG.len()
    );
    let mut image_response = image_body.into_bytes();
    image_response.extend_from_slice(PNG);
    let image_url = serve(image_response).await;
    let image = fetch_media(&image_url, MediaKind::Image, &local_policy(1024))
        .await
        .unwrap();
    assert_eq!(image.as_ref(), PNG);
    assert_eq!(MediaKind::Image.detect_mime(&image), Some("image/png"));

    let video_body = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        MP4.len()
    );
    let mut video_response = video_body.into_bytes();
    video_response.extend_from_slice(MP4);
    let video_url = serve(video_response).await;
    let video = fetch_media(&video_url, MediaKind::Video, &local_policy(1024))
        .await
        .unwrap();
    assert_eq!(video.as_ref(), MP4);
    assert_eq!(MediaKind::Video.detect_mime(&video), Some("video/mp4"));
}

#[tokio::test]
async fn rejects_oversized_content_length_before_reading_body() {
    let response = b"HTTP/1.1 200 OK\r\nContent-Length: 999\r\nConnection: close\r\n\r\n".to_vec();
    let url = serve(response).await;
    let error = fetch_media(&url, MediaKind::Image, &local_policy(32))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        UploadError::TooLarge {
            limit: 32,
            received: 999
        }
    ));
}

#[tokio::test]
async fn rejects_oversized_chunked_body_without_content_length() {
    let chunks = b"18\r\n\x89PNG\r\n\x1a\n0123456789012345\r\n0\r\n\r\n";
    let head = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
    let mut response = head.to_vec();
    response.extend_from_slice(chunks);
    let url = serve(response).await;
    let error = fetch_media(&url, MediaKind::Image, &local_policy(16))
        .await
        .unwrap_err();
    assert!(matches!(error, UploadError::TooLarge { limit: 16, .. }));
}

#[tokio::test]
async fn rejects_unsupported_image_and_video_signatures() {
    for kind in [MediaKind::Image, MediaKind::Video] {
        let payload = b"not a supported media file";
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            payload.len()
        );
        let mut response = header.into_bytes();
        response.extend_from_slice(payload);
        let url = serve(response).await;
        assert!(matches!(
            fetch_media(&url, kind, &local_policy(1024)).await,
            Err(UploadError::UnsupportedType(_))
        ));
    }
}

#[tokio::test]
async fn fetch_follows_redirect_and_validates_final_media_body() {
    let destination = serve({
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            PNG.len()
        )
        .into_bytes();
        response.extend_from_slice(PNG);
        response
    })
    .await;
    let location = destination.as_str().to_owned();
    let source = serve(
        format!(
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .into_bytes(),
    )
    .await;

    let image = fetch_media(&source, MediaKind::Image, &local_policy(1024))
        .await
        .unwrap();
    assert_eq!(image.as_ref(), PNG);
}

#[tokio::test]
async fn fetch_rejects_disallowed_redirect_before_following_it() {
    let source = serve(
        b"HTTP/1.1 302 Found\r\nLocation: file:///etc/passwd\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            .to_vec(),
    )
    .await;

    assert!(matches!(
        fetch_media(&source, MediaKind::Image, &local_policy(1024)).await,
        Err(UploadError::SsrfDenied(message)) if message.contains("only http and https")
    ));
}

#[tokio::test]
async fn injected_resolver_pins_each_redirect_hop_to_validated_address() {
    use gemini_bridge_upload::media_fetch::{MediaHostResolver, fetch_media_with_resolver};
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Arc;

    struct FixedResolver {
        resolved: Arc<tokio::sync::Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl MediaHostResolver for FixedResolver {
        async fn resolve_and_check(&self, host: &str) -> Result<IpAddr, UploadError> {
            self.resolved.lock().await.push(host.to_owned());
            Ok(IpAddr::V4(Ipv4Addr::LOCALHOST))
        }
    }

    let destination = serve({
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            PNG.len()
        )
        .into_bytes();
        response.extend_from_slice(PNG);
        response
    })
    .await;
    let destination_url = format!(
        "http://second.test:{}{}",
        destination.port().unwrap(),
        destination.path()
    );
    let source = serve(
        format!(
            "HTTP/1.1 302 Found\r\nLocation: {destination_url}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .into_bytes(),
    )
    .await;
    let source_url = Url::parse(&format!(
        "http://first.test:{}{}",
        source.port().unwrap(),
        source.path()
    ))
    .unwrap();
    let resolved = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let resolver = Arc::new(FixedResolver {
        resolved: resolved.clone(),
    });
    let mut policy = local_policy(1024);
    policy.strict_network = true;

    let image = fetch_media_with_resolver(&source_url, MediaKind::Image, &policy, resolver)
        .await
        .unwrap();
    assert_eq!(image.as_ref(), PNG);
    assert_eq!(
        resolved.lock().await.as_slice(),
        ["first.test", "second.test"]
    );
}

#[test]
fn strict_provider_policy_rejects_http_credentials_and_private_literal_ips() {
    let policy = MediaFetchPolicy::provider_cdn();
    for raw in [
        "http://lh3.googleusercontent.com/image.png",
        "https://user:pass@lh3.googleusercontent.com/image.png",
        "https://127.0.0.1/image.png",
        "https://[::1]/image.png",
        "https://evil-googleusercontent.com/image.png",
        "https://googleusercontent.com.evil.test/image.png",
    ] {
        let url = Url::parse(raw).unwrap();
        assert!(
            gemini_bridge_upload::media_fetch::validate_media_url(&url, &policy).is_err(),
            "unexpectedly accepted {raw}"
        );
    }
}

#[test]
fn redirect_targets_are_checked_against_the_same_policy() {
    let policy = MediaFetchPolicy::provider_cdn();
    let base = Url::parse("https://lh3.googleusercontent.com/image.png").unwrap();
    for target in [
        "http://lh3.googleusercontent.com/image.png",
        "https://127.0.0.1/admin",
        "https://evil-googleusercontent.com/image.png",
        "https://user@lh3.googleusercontent.com/image.png",
    ] {
        assert!(
            gemini_bridge_upload::media_fetch::validate_media_redirect(&base, target, &policy)
                .is_err(),
            "unexpectedly accepted redirect {target}"
        );
    }
}

#[test]
fn ipv4_ipv6_and_documentation_ranges_are_not_globally_routable() {
    for ip in [
        "192.0.2.1",
        "198.51.100.1",
        "203.0.113.1",
        "198.18.0.1",
        "224.0.0.1",
        "2001:db8::1",
        "ff02::1",
    ] {
        assert!(
            !gemini_bridge_upload::is_address_allowed(ip.parse().unwrap()),
            "unexpectedly allowed {ip}"
        );
    }
}
