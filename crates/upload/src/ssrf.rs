//! SSRF guard: address and URL scheme validation for URL references.

use std::net::{IpAddr, Ipv4Addr};

use crate::error::UploadError;

// ── Public helpers ──────────────────────────────────────────────────────────

/// Returns `true` if the resolved IP address is safe to connect to.
///
/// Blocked IPv4 ranges:
/// - Loopback: 127.0.0.0/8
/// - All-zeros: 0.0.0.0/8
/// - Private: 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
/// - Link-local: 169.254.0.0/16
/// - Carrier-grade NAT: 100.64.0.0/10
/// - Broadcast: 255.255.255.255
///
/// Blocked IPv6 ranges:
/// - Loopback: ::1
/// - Unspecified: ::
/// - Unique-local: fc00::/7 (fc00:: – fdff::)
/// - Link-local: fe80::/10
/// - IPv4-mapped private (::ffff:A.B.C.D where A.B.C.D is blocked IPv4)
pub fn is_address_allowed(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => !is_blocked_v4(v4),
        IpAddr::V6(v6) => {
            // Check if it's an IPv4-mapped address (::ffff:A.B.C.D)
            if let Some(v4) = v6.to_ipv4_mapped() {
                return !is_blocked_v4(v4);
            }
            let segments = v6.segments();
            // Loopback (::1)
            if v6 == std::net::Ipv6Addr::LOCALHOST {
                return false;
            }
            // Unspecified (::)
            if v6 == std::net::Ipv6Addr::UNSPECIFIED {
                return false;
            }
            // Unique-local: fc00::/7 — first segment high bits 1111110x
            if (segments[0] & 0xfe00) == 0xfc00 {
                return false;
            }
            // Link-local: fe80::/10 — first segment high bits 1111111010
            if (segments[0] & 0xffc0) == 0xfe80 {
                return false;
            }
            true
        }
    }
}

fn is_blocked_v4(v4: Ipv4Addr) -> bool {
    let octets = v4.octets();
    // 0.0.0.0/8
    if octets[0] == 0 {
        return true;
    }
    // Loopback 127.0.0.0/8
    if v4.is_loopback() {
        return true;
    }
    // Private: 10.0.0.0/8
    if octets[0] == 10 {
        return true;
    }
    // Private: 172.16.0.0/12
    if octets[0] == 172 && (16..=31).contains(&octets[1]) {
        return true;
    }
    // Private: 192.168.0.0/16
    if octets[0] == 192 && octets[1] == 168 {
        return true;
    }
    // Link-local: 169.254.0.0/16
    if octets[0] == 169 && octets[1] == 254 {
        return true;
    }
    // Carrier-grade NAT: 100.64.0.0/10
    if octets[0] == 100 && (64..=127).contains(&octets[1]) {
        return true;
    }
    // Broadcast
    if v4 == Ipv4Addr::BROADCAST {
        return true;
    }
    false
}

/// Resolve `host` to IP addresses and reject if any resolved IP is blocked.
/// Returns the first allowed IP so it can be used for connection pinning.
///
/// DNS pinning: ALL resolved addresses must be allowed, not just one.
pub async fn resolve_and_check(host: &str) -> Result<IpAddr, UploadError> {
    use tokio::net::lookup_host;
    let addrs: Vec<IpAddr> = lookup_host(format!("{host}:0"))
        .await
        .map_err(|e| UploadError::DnsResolutionFailed(format!("{e}")))?
        .map(|sa| sa.ip())
        .collect();

    if addrs.is_empty() {
        return Err(UploadError::DnsResolutionFailed(format!(
            "no addresses resolved for {host}"
        )));
    }

    for ip in &addrs {
        if !is_address_allowed(*ip) {
            return Err(UploadError::SsrfDenied(format!(
                "resolved address {ip} for host {host} is in a blocked range"
            )));
        }
    }

    Ok(addrs[0])
}
