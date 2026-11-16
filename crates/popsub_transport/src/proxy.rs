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
//!
//! Load balancers that work at the TCP level can't add an HTTP header, so
//! they send a PROXY protocol header (HAProxy's, version 1 or 2) before
//! anything else on the connection instead. `read_proxy_header` reads one,
//! and the server only does so for connections from trusted proxies.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use tokio::io::{AsyncRead, AsyncReadExt};

/// The longest version 1 header, `\r\n` included, as the specification sets it.
const V1_MAX_BYTES: usize = 107;

/// The version 2 signature every binary header starts with.
const V2_SIGNATURE: [u8; 12] = *b"\r\n\r\n\0\r\nQUIT\n";

/// The most address and extension bytes a version 2 header may carry. The
/// format allows 65535; real load balancers send far fewer.
const V2_MAX_PAYLOAD: usize = 4096;

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

/// Reads a PROXY protocol header, version 1 or 2, from the start of a
/// connection, and nothing after it.
///
/// Version 1 is a text line such as `PROXY TCP4 203.0.113.7 10.0.0.5 51234 443\r\n`;
/// version 2 is binary, after a fixed signature. Extension fields (TLVs) in
/// version 2 are skipped.
///
/// Args:
///     stream: The connection, before TLS or the WebSocket handshake.
///
/// Returns:
///     The original client's address, or `None` when the header says the
///     connection carries no client (version 1 `UNKNOWN`, version 2 `LOCAL`,
///     or an address family other than IPv4 and IPv6), such as a load
///     balancer's health check.
///
/// Raises:
///     String: When the connection doesn't start with a valid header, or
///         closes or fails before one is read.
pub async fn read_proxy_header<R>(stream: &mut R) -> Result<Option<IpAddr>, String>
where
    R: AsyncRead + Unpin,
{
    let io = |e: std::io::Error| format!("reading the PROXY header failed: {e}");
    let mut start = [0u8; 6];
    stream.read_exact(&mut start).await.map_err(io)?;
    if &start == b"PROXY " {
        let mut line = start.to_vec();
        while !line.ends_with(b"\r\n") {
            if line.len() >= V1_MAX_BYTES {
                return Err("the PROXY header is longer than 107 bytes".to_string());
            }
            line.push(stream.read_u8().await.map_err(io)?);
        }
        return parse_v1(&line[..line.len() - 2]);
    }
    if start != V2_SIGNATURE[..6] {
        return Err("the connection didn't start with a PROXY header".to_string());
    }
    let mut rest = [0u8; 10];
    stream.read_exact(&mut rest).await.map_err(io)?;
    if rest[..6] != V2_SIGNATURE[6..] {
        return Err("the connection didn't start with a PROXY header".to_string());
    }
    let (version_command, family) = (rest[6], rest[7]);
    let length = usize::from(u16::from_be_bytes([rest[8], rest[9]]));
    if version_command >> 4 != 2 {
        return Err(format!(
            "unsupported PROXY header version {}",
            version_command >> 4
        ));
    }
    if length > V2_MAX_PAYLOAD {
        return Err(format!(
            "the PROXY header carries {length} bytes, more than {V2_MAX_PAYLOAD}"
        ));
    }
    let mut payload = vec![0u8; length];
    stream.read_exact(&mut payload).await.map_err(io)?;
    match version_command & 0x0f {
        0 => Ok(None),
        1 => parse_v2_source(family, &payload),
        command => Err(format!("unknown PROXY header command {command}")),
    }
}

/// Parses a version 1 line, without its `\r\n`.
///
/// Args:
///     line: The line, starting with `PROXY `.
///
/// Returns:
///     The source address, or `None` for `UNKNOWN`.
///
/// Raises:
///     String: When the line is malformed.
fn parse_v1(line: &[u8]) -> Result<Option<IpAddr>, String> {
    let malformed = || "malformed PROXY header".to_string();
    let text = std::str::from_utf8(line).map_err(|_| malformed())?;
    let fields: Vec<&str> = text.split(' ').collect();
    match fields.get(1).copied() {
        Some("UNKNOWN") => Ok(None),
        Some("TCP4") if fields.len() == 6 => fields[2]
            .parse::<Ipv4Addr>()
            .map(|address| Some(IpAddr::V4(address)))
            .map_err(|_| malformed()),
        Some("TCP6") if fields.len() == 6 => fields[2]
            .parse::<Ipv6Addr>()
            .map(|address| Some(IpAddr::V6(address).to_canonical()))
            .map_err(|_| malformed()),
        _ => Err(malformed()),
    }
}

/// Reads the source address from a version 2 `PROXY` header's payload.
///
/// Args:
///     family: The address family and transport byte.
///     payload: The addresses, followed by any extension fields.
///
/// Returns:
///     The source address, or `None` for families other than IPv4 and IPv6.
///
/// Raises:
///     String: When the payload is too short for its family.
fn parse_v2_source(family: u8, payload: &[u8]) -> Result<Option<IpAddr>, String> {
    let short = || "the PROXY header is too short for its addresses".to_string();
    match family >> 4 {
        1 => {
            if payload.len() < 12 {
                return Err(short());
            }
            let address = Ipv4Addr::new(payload[0], payload[1], payload[2], payload[3]);
            Ok(Some(IpAddr::V4(address)))
        }
        2 => {
            if payload.len() < 36 {
                return Err(short());
            }
            let mut bytes = [0u8; 16];
            bytes.copy_from_slice(&payload[..16]);
            Ok(Some(IpAddr::V6(Ipv6Addr::from(bytes)).to_canonical()))
        }
        _ => Ok(None),
    }
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

    /// Builds a version 2 header.
    ///
    /// Args:
    ///     command: 0 for `LOCAL`, 1 for `PROXY`.
    ///     family: The family and transport byte.
    ///     payload: The addresses and extensions.
    ///
    /// Returns:
    ///     The header bytes.
    fn v2(command: u8, family: u8, payload: &[u8]) -> Vec<u8> {
        let mut header = V2_SIGNATURE.to_vec();
        header.push(0x20 | command);
        header.push(family);
        header.extend((payload.len() as u16).to_be_bytes());
        header.extend(payload);
        header
    }

    /// Reads a header from bytes and returns what it gave and what was left.
    ///
    /// Args:
    ///     bytes: The start of a connection.
    ///
    /// Returns:
    ///     The outcome and the unread bytes.
    async fn read(bytes: &[u8]) -> (Result<Option<IpAddr>, String>, Vec<u8>) {
        let mut stream = bytes;
        let outcome = read_proxy_header(&mut stream).await;
        (outcome, stream.to_vec())
    }

    /// Checks version 1 headers for IPv4, IPv6 and `UNKNOWN`, and that the
    /// bytes after the header are left for the WebSocket handshake.
    #[tokio::test]
    async fn version_1_headers_give_the_source() {
        let (outcome, rest) = read(b"PROXY TCP4 203.0.113.7 10.0.0.5 51234 443\r\nGET /").await;
        assert_eq!(outcome, Ok(Some(ip("203.0.113.7"))));
        assert_eq!(rest, b"GET /");
        let (outcome, _) = read(b"PROXY TCP6 2001:db8::7 2001:db8::1 51234 443\r\n").await;
        assert_eq!(outcome, Ok(Some(ip("2001:db8::7"))));
        let (outcome, _) = read(b"PROXY UNKNOWN\r\n").await;
        assert_eq!(outcome, Ok(None));
    }

    /// Checks version 2 headers for IPv4 with extensions, IPv6 and `LOCAL`,
    /// and that the bytes after the header are left.
    #[tokio::test]
    async fn version_2_headers_give_the_source() {
        let mut ipv4 = vec![203, 0, 113, 7, 10, 0, 0, 5, 0xc8, 0x22, 0x01, 0xbb];
        ipv4.extend([0x04, 0x00, 0x03, b'a', b'b', b'c']);
        let mut bytes = v2(1, 0x11, &ipv4);
        bytes.extend(b"GET /");
        let (outcome, rest) = read(&bytes).await;
        assert_eq!(outcome, Ok(Some(ip("203.0.113.7"))));
        assert_eq!(rest, b"GET /");
        let mut ipv6 = ip_bytes("2001:db8::7");
        ipv6.extend(ip_bytes("2001:db8::1"));
        ipv6.extend([0, 1, 0, 2]);
        let (outcome, _) = read(&v2(1, 0x21, &ipv6)).await;
        assert_eq!(outcome, Ok(Some(ip("2001:db8::7"))));
        let (outcome, rest) = read(&[v2(0, 0x00, &[]), b"GET".to_vec()].concat()).await;
        assert_eq!(outcome, Ok(None));
        assert_eq!(rest, b"GET");
    }

    /// The 16 bytes of an IPv6 address.
    ///
    /// Args:
    ///     text: The address.
    ///
    /// Returns:
    ///     Its bytes.
    fn ip_bytes(text: &str) -> Vec<u8> {
        text.parse::<Ipv6Addr>().expect("IPv6").octets().to_vec()
    }

    /// Checks that missing, malformed, truncated and oversized headers are refused.
    #[tokio::test]
    async fn bad_headers_are_refused() {
        for bytes in [
            b"GET / HTTP/1.1\r\n\r\n".to_vec(),
            b"PROXY TCP4 not-an-ip 10.0.0.5 1 2\r\n".to_vec(),
            b"PROXY TCP4 203.0.113.7\r\n".to_vec(),
            b"PROXY TCP4 203.0.113.7 10.0.0.5 51234 443".to_vec(),
            [b"PROXY ".to_vec(), vec![b'x'; 200]].concat(),
            v2(1, 0x11, &[203, 0, 113]),
            v2(5, 0x11, &[0; 12]),
            [V2_SIGNATURE.to_vec(), vec![0x21, 0x11, 0xff, 0xff]].concat(),
        ] {
            let (outcome, _) = read(&bytes).await;
            assert!(outcome.is_err(), "{bytes:?} gave {outcome:?}");
        }
    }
}
