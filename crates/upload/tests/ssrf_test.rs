use gemini_bridge_upload::ssrf::{is_address_allowed, resolve_and_check};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[test]
fn ssrf_loopback_v4_blocked() {
    assert!(!is_address_allowed(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
    assert!(!is_address_allowed(IpAddr::V4(Ipv4Addr::new(127, 1, 2, 3))));
}

#[test]
fn ssrf_private_v4_blocked() {
    // 10.0.0.0/8
    assert!(!is_address_allowed(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
    assert!(!is_address_allowed(IpAddr::V4(Ipv4Addr::new(
        10, 255, 255, 254
    ))));
    // 172.16.0.0/12
    assert!(!is_address_allowed(IpAddr::V4(Ipv4Addr::new(
        172, 16, 0, 1
    ))));
    assert!(!is_address_allowed(IpAddr::V4(Ipv4Addr::new(
        172, 31, 255, 254
    ))));
    // 192.168.0.0/16
    assert!(!is_address_allowed(IpAddr::V4(Ipv4Addr::new(
        192, 168, 1, 1
    ))));
    assert!(!is_address_allowed(IpAddr::V4(Ipv4Addr::new(
        192, 168, 0, 254
    ))));
}

#[test]
fn ssrf_link_local_v4_blocked() {
    assert!(!is_address_allowed(IpAddr::V4(Ipv4Addr::new(
        169, 254, 1, 1
    ))));
    assert!(!is_address_allowed(IpAddr::V4(Ipv4Addr::new(
        169, 254, 169, 254
    ))));
}

#[test]
fn ssrf_cgnat_v4_blocked() {
    assert!(!is_address_allowed(IpAddr::V4(Ipv4Addr::new(
        100, 64, 0, 1
    ))));
    assert!(!is_address_allowed(IpAddr::V4(Ipv4Addr::new(
        100, 127, 255, 254
    ))));
}

#[test]
fn ssrf_broadcast_and_zeros_v4_blocked() {
    assert!(!is_address_allowed(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0))));
    assert!(!is_address_allowed(IpAddr::V4(Ipv4Addr::new(0, 1, 2, 3))));
    assert!(!is_address_allowed(IpAddr::V4(Ipv4Addr::new(
        255, 255, 255, 255
    ))));
}

#[test]
fn ssrf_loopback_v6_blocked() {
    assert!(!is_address_allowed(IpAddr::V6(Ipv6Addr::LOCALHOST)));
}

#[test]
fn ssrf_unspecified_v6_blocked() {
    assert!(!is_address_allowed(IpAddr::V6(Ipv6Addr::UNSPECIFIED)));
}

#[test]
fn ssrf_unique_local_v6_blocked() {
    let ip: IpAddr = "fc00::1".parse().unwrap();
    assert!(!is_address_allowed(ip));
    let ip: IpAddr = "fd12:3456:789a::1".parse().unwrap();
    assert!(!is_address_allowed(ip));
}

#[test]
fn ssrf_link_local_v6_blocked() {
    let ip: IpAddr = "fe80::1".parse().unwrap();
    assert!(!is_address_allowed(ip));
    let ip: IpAddr = "febf:ffff:ffff:ffff::1".parse().unwrap();
    assert!(!is_address_allowed(ip));
}

#[test]
fn ssrf_v4_mapped_private_blocked() {
    let ip: IpAddr = "::ffff:10.0.0.1".parse().unwrap();
    assert!(!is_address_allowed(ip));
    let ip: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
    assert!(!is_address_allowed(ip));
    let ip: IpAddr = "::ffff:192.168.1.1".parse().unwrap();
    assert!(!is_address_allowed(ip));
}

#[test]
fn ssrf_public_ips_allowed() {
    let ip: IpAddr = "1.1.1.1".parse().unwrap();
    assert!(is_address_allowed(ip));
    let ip: IpAddr = "8.8.8.8".parse().unwrap();
    assert!(is_address_allowed(ip));
    let ip: IpAddr = "93.184.216.34".parse().unwrap();
    assert!(is_address_allowed(ip));
    let ip: IpAddr = "2606:4700:4700::1111".parse().unwrap();
    assert!(is_address_allowed(ip));
}

#[tokio::test]
async fn resolve_and_check_local_blocked() {
    let res = resolve_and_check("127.0.0.1").await;
    assert!(res.is_err());
}
