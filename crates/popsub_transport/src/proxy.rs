//! Working out a client's real address behind reverse proxies.
//!
//! Behind a proxy every connection's socket comes from the proxy, so rate
//! limits keyed on it would lump every client together. `TrustedProxies`
//! lists the proxies the operator runs. Only a connection whose socket peer
//! is one of them has its `X-Forwarded-For` header read, and the header is
//! read from the right: each proxy appends the address it received the
//! request from, so the rightmost address that isn't a trusted proxy is the
//! client. Anything to the left of it was written by the client and could be
//! made up. A peer that isn't trusted is used as is, whatever headers it sends.

use std::net::IpAddr;

/// A range of addresses: one address, or a CIDR block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Range {
    /// The first address of the range, with the host bits cleared.
    network: IpAddr,
    /// How many leading bits must match.
    prefix: u8,
}

impl Range {
    /// Parses `10.0.0.1`, `10.0.0.0/8`, `::1` or `fd00::/8`.
    ///
    /// Args:
    ///     text: The entry.
    ///
    /// Returns:
    ///     The range.
    ///
    /// Raises:
    ///     String: When the address or prefix length doesn't parse, or the
    ///         prefix is longer than the address.
    fn parse(text: &str) -> Result<Self, String> {
        let invalid = |why: &str| format!("'{text}' is not an IP address or CIDR range: {why}");
        let (address, prefix) = match text.split_once('/') {
            Some((address, prefix)) => (
                address,
                Some(
                    prefix
                        .parse::<u8>()
                        .map_err(|_| invalid("bad prefix length"))?,
                ),
            ),
            None => (text, None),
        };
        let address = address
            .parse::<IpAddr>()
            .map_err(|_| invalid("bad address"))?
            .to_canonical();
        let bits = match address {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        let prefix = prefix.unwrap_or(bits);
        if prefix > bits {
            return Err(invalid("prefix longer than the address"));
        }
        Ok(Self {
            network: mask(address, prefix),
            prefix,
        })
    }

    /// Whether an address is in the range.
    ///
    /// Args:
    ///     address: The address, already canonical.
    ///
    /// Returns:
    ///     True when it is the same family and its leading `prefix` bits match.
    fn contains(&self, address: IpAddr) -> bool {
        let same_family = matches!(
            (self.network, address),
            (IpAddr::V4(_), IpAddr::V4(_)) | (IpAddr::V6(_), IpAddr::V6(_))
        );
        same_family && mask(address, self.prefix) == self.network
    }
}

/// Clears all but the leading bits of an address.
///
/// Args:
///     address: The address.
///     prefix: How many leading bits to keep.
///
/// Returns:
///     The address with the rest cleared.
fn mask(address: IpAddr, prefix: u8) -> IpAddr {
    match address {
        IpAddr::V4(v4) => {
            let keep = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
            IpAddr::V4((u32::from(v4) & keep).into())
        }
        IpAddr::V6(v6) => {
            let keep = u128::MAX.checked_shl(128 - u32::from(prefix)).unwrap_or(0);
            IpAddr::V6((u128::from(v6) & keep).into())
        }
    }
}

/// The reverse proxies whose `X-Forwarded-For` header the server believes.
///
/// IPv4 addresses arriving as IPv4-mapped IPv6 (`::ffff:10.0.0.1`, as on a
/// dual-stack socket) match IPv4 entries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustedProxies {
    /// The trusted ranges.
    ranges: Vec<Range>,
}

impl TrustedProxies {
    /// Parses the `server.trusted_proxies` setting.
    ///
    /// Args:
    ///     entries: IP addresses or CIDR ranges.
    ///
    /// Returns:
    ///     The trusted proxies; empty trusts no one.
    ///
    /// Raises:
    ///     String: Naming the first entry that isn't an address or range.
    pub fn parse<S: AsRef<str>>(entries: &[S]) -> Result<Self, String> {
        let ranges = entries
            .iter()
            .map(|entry| Range::parse(entry.as_ref().trim()))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { ranges })
    }

    /// Whether no proxy is trusted.
    ///
    /// Returns:
    ///     True when the list is empty.
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// Whether an address belongs to a trusted proxy.
    ///
    /// Args:
    ///     address: The address.
    ///
    /// Returns:
    ///     True when one of the ranges contains it.
    pub fn trusts(&self, address: IpAddr) -> bool {
        let address = address.to_canonical();
        self.ranges.iter().any(|range| range.contains(address))
    }

    /// Works out the client's address for a connection.
    ///
    /// When the socket peer is a trusted proxy, the `X-Forwarded-For` values
    /// (every header, in order, joined) are read from the right, skipping
    /// trusted proxies; the first other address is the client. If every
    /// address in it is trusted, the leftmost is used. An entry that doesn't
    /// parse stops the walk, and the last address before it is used, since
    /// nothing further left can be believed. Without a trusted peer, or
    /// without the header, the peer is the client.
    ///
    /// Args:
    ///     peer: The socket's remote address.
    ///     forwarded_for: The values of every `X-Forwarded-For` header.
    ///
    /// Returns:
    ///     The client's address.
    pub fn client_address(&self, peer: IpAddr, forwarded_for: &[String]) -> IpAddr {
        let peer = peer.to_canonical();
        if !self.trusts(peer) {
            return peer;
        }
        let mut client = peer;
        let hops = forwarded_for
            .iter()
            .flat_map(|value| value.split(','))
            .map(str::trim)
            .collect::<Vec<_>>();
        for hop in hops.into_iter().rev() {
            let Some(address) = parse_hop(hop) else {
                break;
            };
            client = address;
            if !self.trusts(address) {
                break;
            }
        }
        client
    }
}

/// Parses one `X-Forwarded-For` entry. Proxies write a bare address, but some
/// add a port (`203.0.113.7:51234`, `[2001:db8::1]:51234`), which is dropped.
///
/// Args:
///     hop: The entry, trimmed.
///
/// Returns:
///     The address, canonical, or `None` when it isn't one.
fn parse_hop(hop: &str) -> Option<IpAddr> {
    let address = if let Ok(address) = hop.parse::<IpAddr>() {
        address
    } else if let Ok(socket) = hop.parse::<std::net::SocketAddr>() {
        socket.ip()
    } else {
        hop.strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
            .and_then(|inner| inner.parse::<IpAddr>().ok())?
    };
    Some(address.to_canonical())
}

/// Unit tests for parsing trusted ranges and resolving client addresses.
#[cfg(test)]
mod tests {
    use super::*;

    /// Parses an address for a test.
    ///
    /// Args:
    ///     text: The address.
    ///
    /// Returns:
    ///     The address.
    fn ip(text: &str) -> IpAddr {
        text.parse().expect("a test address")
    }

    /// Checks single addresses and CIDR ranges in both families, and that
    /// mapped IPv4 addresses match IPv4 entries.
    #[test]
    fn ranges_match_what_they_cover() {
        let proxies = TrustedProxies::parse(&["10.0.0.0/8", "192.168.1.7", "fd00::/8", "::1"])
            .expect("parse");
        assert!(proxies.trusts(ip("10.255.0.1")));
        assert!(!proxies.trusts(ip("11.0.0.1")));
        assert!(proxies.trusts(ip("192.168.1.7")));
        assert!(!proxies.trusts(ip("192.168.1.8")));
        assert!(proxies.trusts(ip("fd12::5")));
        assert!(!proxies.trusts(ip("fe80::1")));
        assert!(proxies.trusts(ip("::1")));
        assert!(proxies.trusts(ip("::ffff:10.1.2.3")));
        let everything = TrustedProxies::parse(&["0.0.0.0/0"]).expect("parse");
        assert!(everything.trusts(ip("203.0.113.9")));
        assert!(!everything.trusts(ip("2001:db8::1")));
        assert!(TrustedProxies::default().is_empty());
    }

    /// Checks that malformed entries are refused with the entry named.
    #[test]
    fn bad_entries_are_refused() {
        for bad in ["proxy.internal", "10.0.0.0/33", "::/129", "10.0.0.1/x", ""] {
            let error = TrustedProxies::parse(&[bad]).expect_err(bad);
            assert!(error.contains(&format!("'{bad}'")), "{error}");
        }
    }

    /// Checks the right-to-left walk: trusted hops are skipped, the first
    /// untrusted one is the client, and a made-up left part is ignored.
    #[test]
    fn the_rightmost_untrusted_hop_is_the_client() {
        let proxies = TrustedProxies::parse(&["10.0.0.0/8"]).expect("parse");
        let peer = ip("10.0.0.2");
        let header = |value: &str| vec![value.to_string()];
        assert_eq!(
            proxies.client_address(peer, &header("203.0.113.7")),
            ip("203.0.113.7")
        );
        assert_eq!(
            proxies.client_address(peer, &header("1.2.3.4, 203.0.113.7, 10.0.0.9")),
            ip("203.0.113.7")
        );
        assert_eq!(
            proxies.client_address(
                peer,
                &["1.2.3.4".to_string(), "203.0.113.7, 10.0.0.9".to_string()]
            ),
            ip("203.0.113.7")
        );
        assert_eq!(
            proxies.client_address(peer, &header("10.0.0.7, 10.0.0.9")),
            ip("10.0.0.7")
        );
        assert_eq!(proxies.client_address(peer, &[]), peer);
    }

    /// Checks that an untrusted peer's header is ignored, that garbage stops
    /// the walk, and that ports are dropped.
    #[test]
    fn untrusted_peers_and_garbage_are_not_believed() {
        let proxies = TrustedProxies::parse(&["10.0.0.0/8"]).expect("parse");
        let header = |value: &str| vec![value.to_string()];
        assert_eq!(
            proxies.client_address(ip("198.51.100.1"), &header("203.0.113.7")),
            ip("198.51.100.1")
        );
        assert_eq!(
            proxies.client_address(ip("10.0.0.2"), &header("203.0.113.7, unknown, 10.0.0.9")),
            ip("10.0.0.9")
        );
        assert_eq!(
            proxies.client_address(ip("10.0.0.2"), &header("unknown")),
            ip("10.0.0.2")
        );
        assert_eq!(
            proxies.client_address(ip("10.0.0.2"), &header("203.0.113.7:51234")),
            ip("203.0.113.7")
        );
        assert_eq!(
            proxies.client_address(ip("10.0.0.2"), &header("[2001:db8::1]:443")),
            ip("2001:db8::1")
        );
        assert_eq!(
            proxies.client_address(ip("::ffff:10.0.0.2"), &header("203.0.113.7")),
            ip("203.0.113.7")
        );
    }
}
