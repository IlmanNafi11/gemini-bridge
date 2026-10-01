use bytes::Bytes;
use gemini_bridge_upload::detect_mime;

const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";
const JPEG_MAGIC: &[u8] = b"\xff\xd8\xff";
const GIF87_MAGIC: &[u8] = b"GIF87a";
const GIF89_MAGIC: &[u8] = b"GIF89a";
const WEBP_MAGIC: &[u8] = {
    // RIFF....WEBP
    static BYTES: &[u8] = b"RIFF\x00\x00\x00\x00WEBP";
    BYTES
};

#[test]
fn detect_png() {
    let bytes = Bytes::from([PNG_MAGIC, b"rest"].concat());
    assert_eq!(detect_mime(&bytes), Some("image/png"));
}

#[test]
fn detect_jpeg() {
    let bytes = Bytes::from([JPEG_MAGIC, b"rest"].concat());
    assert_eq!(detect_mime(&bytes), Some("image/jpeg"));
}

#[test]
fn detect_gif87a() {
    let bytes = Bytes::from([GIF87_MAGIC, b"rest"].concat());
    assert_eq!(detect_mime(&bytes), Some("image/gif"));
}

#[test]
fn detect_gif89a() {
    let bytes = Bytes::from([GIF89_MAGIC, b"rest"].concat());
    assert_eq!(detect_mime(&bytes), Some("image/gif"));
}

#[test]
fn detect_webp() {
    let bytes = Bytes::from(WEBP_MAGIC.to_vec());
    assert_eq!(detect_mime(&bytes), Some("image/webp"));
}

#[test]
fn pdf_not_detected() {
    let bytes = Bytes::from_static(b"%PDF-1.4 ...");
    assert_eq!(detect_mime(&bytes), None);
}

#[test]
fn zip_not_detected() {
    let bytes = Bytes::from_static(b"PK\x03\x04...");
    assert_eq!(detect_mime(&bytes), None);
}

#[test]
fn truncated_bytes_not_detected() {
    // 2 bytes is not enough for any magic signature
    let bytes = Bytes::from_static(b"\x89P");
    assert_eq!(detect_mime(&bytes), None);
}

#[test]
fn claimed_png_type_with_jpeg_body_is_rejected() {
    // detect_mime ignores claimed type and uses magic bytes only
    let bytes = Bytes::from([JPEG_MAGIC, b"rest"].concat());
    let detected = detect_mime(&bytes);
    // JPEG magic should detect as jpeg, not png
    assert_eq!(detected, Some("image/jpeg"));
    assert_ne!(detected, Some("image/png"));
}
