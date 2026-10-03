//! SSRF guard: address and URL scheme validation for URL references.

use std::net::{IpAddr, Ipv4Addr};

use crate::error::UploadError;

// ── Public helpers ──────────────────────────────────────────────────────────

/// Returns `true` only when the address is publicly routable.
///
/// Private, loopback, link-local, shared, documentation, benchmarking,
/// protocol-assignment, multicast, unspecified, and reserved ranges are
/// rejected for both IPv4 and IPv6. IPv4-mapped/compatible IPv6 addresses are
/// evaluated as IPv4.
pub fn is_address_allowed(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => !is_blocked_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4() {
                return !is_blocked_v4(v4);
            }
            let segments = v6.segments();
            if v6.is_unspecified() || v6.is_loopback() {
                return false;
            }
            // Globally routable unicast is limited to 2000::/3.
            if segments[0] & 0xe000 != 0x2000 {
                return false;
            }
            // IETF protocol assignments 2001::/23.
            if segments[0] == 0x2001 && segments[1] < 0x0200 {
                return false;
            }
            // Documentation range 2001:db8::/32.
            if segments[0] == 0x2001 && segments[1] == 0x0db8 {
                return false;
            }
            // 6to4 can tunnel private IPv4 destinations.
            if segments[0] == 0x2002 {
                return false;
            }
            true
        }
    }
}

fn is_blocked_v4(v4: Ipv4Addr) -> bool {
    let [a, b, c, _] = v4.octets();
    matches!(a, 0 | 10 | 127)
        || (a == 100 && (64..=127).contains(&b)) // Shared address space.
        || (a == 169 && b == 254) // Link-local.
        || (a == 172 && (16..=31).contains(&b)) // Private.
        || (a == 192 && b == 0 && c == 0) // IETF protocol assignments.
        || (a == 192 && b == 0 && c == 2) // Documentation TEST-NET-1.
        || (a == 192 && b == 88 && c == 99) // Deprecated relay anycast.
        || (a == 192 && b == 168) // Private.
        || (a == 198 && (b == 18 || b == 19)) // Benchmarking.
        || (a == 198 && b == 51 && c == 100) // Documentation TEST-NET-2.
        || (a == 203 && b == 0 && c == 113) // Documentation TEST-NET-3.
        || a >= 224 // Multicast and reserved.
}

fn select_allowed_address(host: &str, addrs: &[IpAddr]) -> Result<IpAddr, UploadError> {
    if addrs.is_empty() {
        return Err(UploadError::DnsResolutionFailed(format!(
            "no addresses resolved for {host}"
        )));
    }

    for ip in addrs {
        if !is_address_allowed(*ip) {
            return Err(UploadError::SsrfDenied(format!(
                "resolved address {ip} for host {host} is in a blocked range"
            )));
        }
    }

    Ok(addrs[0])
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

    select_allowed_address(host, &addrs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_dns_answers_are_rejected_when_any_address_is_blocked() {
        let answers = [
            IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
        ];

        assert!(matches!(
            select_allowed_address("media.example", &answers),
            Err(UploadError::SsrfDenied(message))
                if message.contains("127.0.0.1") && message.contains("media.example")
        ));
    }
}
