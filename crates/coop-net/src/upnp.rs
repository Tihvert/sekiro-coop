//! UPnP IGD port mapping (std only): SSDP discovery, tiny HTTP/1.1 client, SOAP calls.
//!
//! Lets the host open a UDP port on the router so a friend can connect with
//! a join code `public_ip:port` without installing anything.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

const SSDP_ADDR: &str = "239.255.255.250:1900";
const SSDP_TARGETS: [&str; 4] = [
    "urn:schemas-upnp-org:service:WANIPConnection:1",
    "urn:schemas-upnp-org:service:WANIPConnection:2",
    "urn:schemas-upnp-org:service:WANPPPConnection:1",
    "urn:schemas-upnp-org:device:InternetGatewayDevice:1",
];
const DISCOVER_TIMEOUT: Duration = Duration::from_millis(3000);
const NET_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_RESPONSE: usize = 1 << 20;

/// An active port mapping on the router.
#[derive(Debug, Clone)]
pub struct Mapping {
    pub external_ip: Ipv4Addr,
    pub port: u16,
    pub local_ip: Ipv4Addr,
    control_url: String,
    service_type: String,
}

impl Mapping {
    /// SOAP DeletePortMapping for the same port/UDP.
    pub fn remove(&self) -> Result<(), String> {
        let args = format!(
            "<NewRemoteHost></NewRemoteHost><NewExternalPort>{}</NewExternalPort><NewProtocol>UDP</NewProtocol>",
            self.port
        );
        soap_call(&self.control_url, &self.service_type, "DeletePortMapping", &args)
            .map(|_| ())
            .map_err(|e| format!("Could not remove the UPnP port mapping: {}", e.msg))
    }

    /// Maps one more port on the same router to this machine (lease 0, until
    /// removed with `remove_extra`).
    pub fn add_extra(&self, port: u16, protocol: &str, description: &str) -> Result<(), String> {
        let args = format!(
            "<NewRemoteHost></NewRemoteHost><NewExternalPort>{port}</NewExternalPort>\n             <NewProtocol>{protocol}</NewProtocol><NewInternalPort>{port}</NewInternalPort>\n             <NewInternalClient>{}</NewInternalClient><NewEnabled>1</NewEnabled>\n             <NewPortMappingDescription>{}</NewPortMappingDescription>\n             <NewLeaseDuration>0</NewLeaseDuration>",
            self.local_ip,
            xml_escape(description)
        );
        soap_call(&self.control_url, &self.service_type, "AddPortMapping", &args)
            .map(|_| ())
            .map_err(|e| format!("Router refused the port mapping: {}", e.msg))
    }

    pub fn remove_extra(&self, port: u16, protocol: &str) -> Result<(), String> {
        let args = format!(
            "<NewRemoteHost></NewRemoteHost><NewExternalPort>{port}</NewExternalPort><NewProtocol>{protocol}</NewProtocol>"
        );
        soap_call(&self.control_url, &self.service_type, "DeletePortMapping", &args)
            .map(|_| ())
            .map_err(|e| e.msg)
    }

    /// "1.2.3.4:28100"
    pub fn join_code(&self) -> String {
        format!("{}:{}", self.external_ip, self.port)
    }
}

/// True if the IP is private/CGNAT/loopback (or otherwise unroutable): the join
/// code will not work from the internet and the caller should warn.
pub fn is_non_public(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_loopback()
        || ip.is_unspecified()
        || o[0] == 10
        || (o[0] == 172 && (16..=31).contains(&o[1]))
        || (o[0] == 192 && o[1] == 168)
        || (o[0] == 100 && (64..=127).contains(&o[1]))
        || (o[0] == 169 && o[1] == 254)
}

/// Discover the router, map `port` (UDP) to this machine and query the external IP.
pub fn map_udp_port(port: u16, description: &str, lease_secs: u32) -> Result<Mapping, String> {
    let locations = discover()?;
    if locations.is_empty() {
        return Err("No UPnP router found (is UPnP enabled on the router?)".to_string());
    }

    let mut last_err = String::new();
    let mut found = None;
    for loc in &locations {
        match fetch_service(loc) {
            Ok(svc) => {
                found = Some((loc.clone(), svc));
                break;
            }
            Err(e) => last_err = e,
        }
    }
    let (location, (control_url, service_type)) = found.ok_or(last_err)?;

    let router = router_addr(&location)?;
    let local_ip = local_ip_towards(router)?;

    let add = |lease: u32| {
        let args = format!(
            "<NewRemoteHost></NewRemoteHost><NewExternalPort>{port}</NewExternalPort>\
             <NewProtocol>UDP</NewProtocol><NewInternalPort>{port}</NewInternalPort>\
             <NewInternalClient>{local_ip}</NewInternalClient><NewEnabled>1</NewEnabled>\
             <NewPortMappingDescription>{}</NewPortMappingDescription>\
             <NewLeaseDuration>{lease}</NewLeaseDuration>",
            xml_escape(description)
        );
        soap_call(&control_url, &service_type, "AddPortMapping", &args)
    };
    let mut res = add(lease_secs);
    if lease_secs != 0 {
        if let Err(e) = &res {
            if e.code == Some(725) {
                res = add(0);
            }
        }
    }
    res.map_err(|e| format!("Router refused the port mapping: {}", e.msg))?;

    let mapping = Mapping {
        external_ip: Ipv4Addr::UNSPECIFIED,
        port,
        local_ip,
        control_url,
        service_type,
    };

    let ext = soap_call(&mapping.control_url, &mapping.service_type, "GetExternalIPAddress", "")
        .map_err(|e| e.msg)
        .and_then(|body| {
            tag_text(&body, "NewExternalIPAddress")
                .ok_or_else(|| "Router returned no external IP address".to_string())
        })
        .and_then(|s| {
            s.parse::<Ipv4Addr>()
                .map_err(|_| format!("Router returned an unusable external IP: '{s}'"))
        });
    match ext {
        Ok(external_ip) => Ok(Mapping { external_ip, ..mapping }),
        Err(e) => {
            let _ = mapping.remove();
            Err(format!("Could not get the external IP from the router: {e}"))
        }
    }
}

// ---------------------------------------------------------------- discovery

/// The local IPv4 used for the default route (no packet is sent: connecting a
/// UDP socket only picks the route). With VPN/virtual adapters present a socket
/// bound to 0.0.0.0 may send the multicast out of the wrong one, which is a
/// likely cause of "No UPnP router found" on a router that has UPnP on (2026-10-22).
fn primary_local_ip() -> Option<Ipv4Addr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:80").ok()?;
    match s.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(ip) if !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}

/// Public for diagnostics (`examples/upnp_probe.rs`): discovery only, opens nothing.
pub fn discover_locations() -> Result<Vec<String>, String> {
    let mut all = Vec::new();
    let mut binds: Vec<String> = Vec::new();
    if let Some(ip) = primary_local_ip() {
        binds.push(format!("{ip}:0"));
    }
    binds.push("0.0.0.0:0".into());
    let mut last_err = None;
    for b in binds {
        match discover_from(&b) {
            Ok(locs) => {
                for l in locs {
                    if !all.contains(&l) {
                        all.push(l);
                    }
                }
                if !all.is_empty() {
                    break;
                }
            }
            Err(e) => last_err = Some(e),
        }
    }
    if all.is_empty() && let Some(e) = last_err {
        return Err(e);
    }
    Ok(all)
}

fn discover() -> Result<Vec<String>, String> {
    discover_locations()
}

fn discover_from(bind: &str) -> Result<Vec<String>, String> {
    let sock = UdpSocket::bind(bind).map_err(|e| format!("Network error (UDP socket): {e}"))?;
    if let Ok(std::net::SocketAddr::V4(a)) = sock.local_addr() && !a.ip().is_unspecified() {
        let _ = sock.join_multicast_v4(&Ipv4Addr::new(239, 255, 255, 250), a.ip());
    }
    let _ = sock.set_multicast_ttl_v4(2);
    sock.set_read_timeout(Some(Duration::from_millis(250)))
        .map_err(|e| format!("Network error: {e}"))?;

    let send_all = |sock: &UdpSocket| -> bool {
        let mut ok = false;
        for st in SSDP_TARGETS {
            let msg = format!(
                "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 1\r\nST: {st}\r\n\r\n"
            );
            if sock.send_to(msg.as_bytes(), SSDP_ADDR).is_ok() {
                ok = true;
            }
        }
        ok
    };
    if !send_all(&sock) {
        return Err("Network error: could not send UPnP discovery packet".to_string());
    }

    let start = Instant::now();
    let mut resent = false;
    let mut first_hit: Option<Instant> = None;
    let mut locations: Vec<String> = Vec::new();
    let mut buf = [0u8; 2048];
    while start.elapsed() < DISCOVER_TIMEOUT {
        if !resent && start.elapsed() > Duration::from_millis(800) && locations.is_empty() {
            resent = true;
            send_all(&sock);
        }
        if let Some(t) = first_hit {
            if t.elapsed() > Duration::from_millis(400) {
                break;
            }
        }
        match sock.recv_from(&mut buf) {
            Ok((n, _)) => {
                if let Some(loc) = parse_ssdp_location(&String::from_utf8_lossy(&buf[..n])) {
                    if !locations.contains(&loc) {
                        locations.push(loc);
                    }
                    first_hit.get_or_insert_with(Instant::now);
                }
            }
            Err(_) => continue, // timeout slice; loop checks the deadline
        }
    }
    Ok(locations)
}

fn parse_ssdp_location(resp: &str) -> Option<String> {
    for line in resp.lines() {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("location") {
                let v = v.trim();
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

/// Fetch the device description and return (absolute control URL, service type).
fn fetch_service(location: &str) -> Result<(String, String), String> {
    let (status, body) = http_request("GET", location, &[], &[])?;
    if status != 200 {
        return Err(format!("Router description request failed (HTTP {status})"));
    }
    let xml = String::from_utf8_lossy(&body);
    find_control_url(&xml, location)
        .ok_or_else(|| "Router has no WAN connection service (UPnP IGD not supported)".to_string())
}

fn router_addr(location: &str) -> Result<SocketAddr, String> {
    let (host, port, _) = parse_http_url(location)?;
    (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| format!("Cannot resolve router address: {e}"))?
        .find(|a| a.is_ipv4())
        .ok_or_else(|| "Router address is not IPv4".to_string())
}

fn local_ip_towards(router: SocketAddr) -> Result<Ipv4Addr, String> {
    let sock = UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("Network error: {e}"))?;
    sock.connect(router).map_err(|e| format!("Cannot route to the router: {e}"))?;
    match sock.local_addr() {
        Ok(SocketAddr::V4(a)) if !a.ip().is_unspecified() => Ok(*a.ip()),
        _ => Err("Could not determine the local IP address".to_string()),
    }
}

// --------------------------------------------------------------------- SOAP

struct SoapFail {
    code: Option<u32>,
    msg: String,
}

fn soap_call(control_url: &str, service: &str, action: &str, args: &str) -> Result<String, SoapFail> {
    let body = format!(
        "<?xml version=\"1.0\"?>\r\n<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body>\
         <u:{action} xmlns:u=\"{service}\">{args}</u:{action}></s:Body></s:Envelope>"
    );
    let soap_action = format!("\"{service}#{action}\"");
    let headers = [
        ("Content-Type", "text/xml; charset=\"utf-8\""),
        ("SOAPAction", soap_action.as_str()),
    ];
    let (status, resp) = http_request("POST", control_url, &headers, body.as_bytes())
        .map_err(|msg| SoapFail { code: None, msg })?;
    let text = String::from_utf8_lossy(&resp).into_owned();
    if status == 200 {
        return Ok(text);
    }
    let (code, desc) = parse_soap_fault(&text);
    let msg = match (code, desc) {
        (Some(c), Some(d)) => format!("{d} (error {c})"),
        (Some(c), None) => format!("error {c}"),
        (None, Some(d)) => d,
        (None, None) => format!("HTTP {status}"),
    };
    Err(SoapFail { code, msg })
}

fn parse_soap_fault(xml: &str) -> (Option<u32>, Option<String>) {
    let code = tag_text(xml, "errorCode").and_then(|s| s.trim().parse().ok());
    let desc = tag_text(xml, "errorDescription").filter(|s| !s.is_empty());
    (code, desc)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

// ---------------------------------------------------------------------- XML

fn local_name_eq(tag: &str, name: &str) -> bool {
    tag.rsplit(':').next().unwrap_or(tag).eq_ignore_ascii_case(name)
}

/// Inner contents of every element whose local name matches `name`
/// (case-insensitive, namespace prefix ignored), in document order.
fn find_blocks<'a>(xml: &'a str, name: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(rel) = xml[pos..].find('<') {
        let start = pos + rel;
        let rest = &xml[start + 1..];
        if rest.starts_with(['/', '!', '?']) {
            pos = start + 1;
            continue;
        }
        let Some(gt) = rest.find('>') else { break };
        let tag_full = &rest[..gt];
        let self_close = tag_full.ends_with('/');
        let tag_name = tag_full
            .trim_end_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or("");
        let content_start = start + 1 + gt + 1;
        pos = content_start;
        if !local_name_eq(tag_name, name) {
            continue;
        }
        if self_close {
            out.push("");
            continue;
        }
        let mut p = content_start;
        while let Some(r2) = xml[p..].find("</") {
            let cs = p + r2;
            let after = &xml[cs + 2..];
            let Some(g2) = after.find('>') else { break };
            if local_name_eq(after[..g2].trim(), name) {
                out.push(&xml[content_start..cs]);
                break;
            }
            p = cs + 2;
        }
    }
    out
}

/// Trimmed, unescaped text of the first element named `name`.
fn tag_text(xml: &str, name: &str) -> Option<String> {
    find_blocks(xml, name)
        .first()
        .map(|b| xml_unescape(b.trim()))
}

/// Find the WANIPConnection / WANPPPConnection service in a device description
/// and return (absolute control URL, service type). IP services win over PPP.
fn find_control_url(xml: &str, location: &str) -> Option<(String, String)> {
    let base = tag_text(xml, "URLBase")
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| location.to_string());
    let mut ppp = None;
    for svc in find_blocks(xml, "service") {
        let Some(stype) = tag_text(svc, "serviceType") else { continue };
        let Some(ctrl) = tag_text(svc, "controlURL").filter(|c| !c.is_empty()) else { continue };
        let lower = stype.to_ascii_lowercase();
        if lower.contains(":wanipconnection:") {
            return Some((join_url(&base, &ctrl), stype));
        }
        if lower.contains(":wanpppconnection:") && ppp.is_none() {
            ppp = Some((join_url(&base, &ctrl), stype));
        }
    }
    ppp
}

// --------------------------------------------------------------------- HTTP

/// Split "http://host:port/path" into (host, port, path).
fn parse_http_url(url: &str) -> Result<(String, u16, String), String> {
    let url = url.trim();
    let rest = if url.len() >= 7 && url[..7].eq_ignore_ascii_case("http://") {
        &url[7..]
    } else {
        return Err(format!("Unsupported router URL: {url}"));
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].to_string()),
        None => (rest, "/".to_string()),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => (
            h,
            p.parse::<u16>().map_err(|_| format!("Bad port in router URL: {url}"))?,
        ),
        _ => (authority, 80),
    };
    if host.is_empty() {
        return Err(format!("Bad router URL: {url}"));
    }
    Ok((host.to_string(), port, path))
}

/// Resolve `rel` against `base` (absolute, root-relative or path-relative).
fn join_url(base: &str, rel: &str) -> String {
    let rel = rel.trim();
    if rel.len() >= 7 && rel[..7].eq_ignore_ascii_case("http://") {
        return rel.to_string();
    }
    let Some(scheme_end) = base.find("://") else {
        return rel.to_string();
    };
    let after = scheme_end + 3;
    let origin_end = base[after..].find('/').map_or(base.len(), |i| after + i);
    if rel.starts_with('/') {
        return format!("{}{}", &base[..origin_end], rel);
    }
    let path = &base[origin_end..];
    let path = path.split(['?', '#']).next().unwrap_or("");
    let dir = match path.rfind('/') {
        Some(i) => &path[..=i],
        None => "/",
    };
    format!("{}{}{}", &base[..origin_end], dir, rel)
}

fn http_request(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<(u16, Vec<u8>), String> {
    let (host, port, path) = parse_http_url(url)?;
    let addr = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| format!("Cannot resolve {host}: {e}"))?
        .next()
        .ok_or_else(|| format!("Cannot resolve {host}"))?;
    let mut stream = TcpStream::connect_timeout(&addr, NET_TIMEOUT)
        .map_err(|e| format!("Cannot connect to router at {addr}: {e}"))?;
    let _ = stream.set_read_timeout(Some(NET_TIMEOUT));
    let _ = stream.set_write_timeout(Some(NET_TIMEOUT));

    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if method == "POST" || !body.is_empty() {
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    req.push_str("\r\n");
    let mut out = req.into_bytes();
    out.extend_from_slice(body);
    stream.write_all(&out).map_err(|e| format!("Router connection error: {e}"))?;

    let mut raw = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&chunk[..n]);
                if raw.len() > MAX_RESPONSE {
                    return Err("Router response too large".to_string());
                }
            }
            // Timeout after we already have data: treat as end of response.
            Err(_) if !raw.is_empty() => break,
            Err(e) => return Err(format!("Router did not answer: {e}")),
        }
    }
    split_http_response(&raw)
}

/// Split a raw HTTP response into (status, body), decoding chunked bodies and
/// honouring Content-Length.
fn split_http_response(raw: &[u8]) -> Result<(u16, Vec<u8>), String> {
    let sep = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| "Malformed HTTP response from router".to_string())?;
    let head = String::from_utf8_lossy(&raw[..sep]);
    let body = &raw[sep + 4..];
    let mut lines = head.lines();
    let status_line = lines.next().unwrap_or("");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| format!("Bad HTTP status line: '{status_line}'"))?;

    let mut chunked = false;
    let mut content_len: Option<usize> = None;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim().to_ascii_lowercase();
            let v = v.trim();
            if k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked") {
                chunked = true;
            } else if k == "content-length" {
                content_len = v.parse().ok();
            }
        }
    }
    let body = if chunked {
        decode_chunked(body)?
    } else if let Some(n) = content_len {
        body[..n.min(body.len())].to_vec()
    } else {
        body.to_vec()
    };
    Ok((status, body))
}

fn decode_chunked(mut data: &[u8]) -> Result<Vec<u8>, String> {
    let bad = || "Malformed chunked response from router".to_string();
    let mut out = Vec::new();
    loop {
        let eol = data.windows(2).position(|w| w == b"\r\n").ok_or_else(bad)?;
        let size_line = std::str::from_utf8(&data[..eol]).map_err(|_| bad())?;
        let size_str = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_str, 16).map_err(|_| bad())?;
        data = &data[eol + 2..];
        if size == 0 {
            return Ok(out);
        }
        if data.len() < size {
            return Err(bad());
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size..];
        data = data.strip_prefix(b"\r\n").unwrap_or(data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DESC: &str = r#"<?xml version="1.0"?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
  <specVersion><major>1</major><minor>0</minor></specVersion>
  <device>
    <deviceType>urn:schemas-upnp-org:device:InternetGatewayDevice:1</deviceType>
    <friendlyName>Router</friendlyName>
    <serviceList>
      <service>
        <serviceType>urn:schemas-upnp-org:service:Layer3Forwarding:1</serviceType>
        <controlURL>/ctl/L3F</controlURL>
      </service>
    </serviceList>
    <deviceList>
      <device>
        <deviceType>urn:schemas-upnp-org:device:WANDevice:1</deviceType>
        <deviceList>
          <device>
            <deviceType>urn:schemas-upnp-org:device:WANConnectionDevice:1</deviceType>
            <serviceList>
              <service>
                <serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</serviceType>
                <serviceId>urn:upnp-org:serviceId:WANIPConn1</serviceId>
                <controlURL>/ctl/IPConn</controlURL>
                <eventSubURL>/evt/IPConn</eventSubURL>
              </service>
            </serviceList>
          </device>
        </deviceList>
      </device>
    </deviceList>
  </device>
</root>"#;

    #[test]
    fn control_url_relative_to_location() {
        let (url, st) = find_control_url(DESC, "http://192.168.1.1:5000/rootDesc.xml").unwrap();
        assert_eq!(url, "http://192.168.1.1:5000/ctl/IPConn");
        assert_eq!(st, "urn:schemas-upnp-org:service:WANIPConnection:1");
    }

    #[test]
    fn control_url_with_urlbase_and_namespaces() {
        let xml = r#"<ROOT><URLBase>http://10.0.0.1:49152/</URLBase><DEVICE>
            <u:Service><u:SERVICETYPE>urn:schemas-upnp-org:service:WANPPPConnection:1</u:SERVICETYPE>
            <u:CONTROLURL>upnp/control/ppp</u:CONTROLURL></u:Service>
            </DEVICE></ROOT>"#;
        let (url, st) = find_control_url(xml, "http://192.168.1.1:5000/rootDesc.xml").unwrap();
        assert_eq!(url, "http://10.0.0.1:49152/upnp/control/ppp");
        assert!(st.contains("WANPPPConnection"));
    }

    #[test]
    fn control_url_missing() {
        let xml = "<root><service><serviceType>urn:x:service:Foo:1</serviceType><controlURL>/a</controlURL></service></root>";
        assert!(find_control_url(xml, "http://1.2.3.4/d.xml").is_none());
    }

    #[test]
    fn url_joining() {
        let b = "http://192.168.1.1:5000/a/b/desc.xml";
        assert_eq!(join_url(b, "/ctl"), "http://192.168.1.1:5000/ctl");
        assert_eq!(join_url(b, "ctl"), "http://192.168.1.1:5000/a/b/ctl");
        assert_eq!(join_url(b, "http://x:1/y"), "http://x:1/y");
        assert_eq!(join_url("http://h:9", "ctl"), "http://h:9/ctl");
        assert_eq!(join_url("http://h:9/", "ctl"), "http://h:9/ctl");
    }

    #[test]
    fn url_parsing() {
        assert_eq!(
            parse_http_url("http://192.168.1.1:5000/x/y?z=1").unwrap(),
            ("192.168.1.1".to_string(), 5000, "/x/y?z=1".to_string())
        );
        assert_eq!(
            parse_http_url("http://router").unwrap(),
            ("router".to_string(), 80, "/".to_string())
        );
        assert!(parse_http_url("https://x/").is_err());
    }

    #[test]
    fn external_ip_response() {
        let body = r#"<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body>
            <u:GetExternalIPAddressResponse xmlns:u="urn:schemas-upnp-org:service:WANIPConnection:1">
            <NewExternalIPAddress>203.0.113.7</NewExternalIPAddress></u:GetExternalIPAddressResponse></s:Body></s:Envelope>"#;
        assert_eq!(tag_text(body, "NewExternalIPAddress").unwrap(), "203.0.113.7");
    }

    #[test]
    fn soap_fault_725() {
        let body = r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><s:Fault>
            <faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring><detail>
            <UPnPError xmlns="urn:schemas-upnp-org:control-1-0"><errorCode>725</errorCode>
            <errorDescription>OnlyPermanentLeasesSupported</errorDescription></UPnPError>
            </detail></s:Fault></s:Body></s:Envelope>"#;
        let (code, desc) = parse_soap_fault(body);
        assert_eq!(code, Some(725));
        assert_eq!(desc.as_deref(), Some("OnlyPermanentLeasesSupported"));
        assert_eq!(parse_soap_fault("garbage"), (None, None));
    }

    #[test]
    fn chunked_decoding() {
        let raw = b"4\r\nWiki\r\n5;ext=1\r\npedia\r\nE\r\n in\r\n\r\nchunks.\r\n0\r\n\r\n";
        assert_eq!(decode_chunked(raw).unwrap(), b"Wikipedia in\r\n\r\nchunks.");
        assert!(decode_chunked(b"zz\r\nabc").is_err());
        assert!(decode_chunked(b"10\r\nshort").is_err());
    }

    #[test]
    fn http_split_plain_and_chunked() {
        let r = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nServer: x\r\n\r\nhelloEXTRA";
        let (s, b) = split_http_response(r).unwrap();
        assert_eq!((s, b.as_slice()), (200, &b"hello"[..]));

        let r = b"HTTP/1.1 500 Internal Server Error\r\nTransfer-Encoding: Chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n";
        let (s, b) = split_http_response(r).unwrap();
        assert_eq!((s, b.as_slice()), (500, &b"abc"[..]));

        let r = b"HTTP/1.0 200 OK\r\n\r\nno length";
        assert_eq!(split_http_response(r).unwrap().1, b"no length");
        assert!(split_http_response(b"HTTP/1.1 200 OK\r\nno end").is_err());
    }

    #[test]
    fn ssdp_location_parsing() {
        let r = "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=120\r\nlocation: http://192.168.1.1:5000/rootDesc.xml\r\nST: x\r\n\r\n";
        assert_eq!(
            parse_ssdp_location(r).as_deref(),
            Some("http://192.168.1.1:5000/rootDesc.xml")
        );
        assert!(parse_ssdp_location("HTTP/1.1 200 OK\r\n\r\n").is_none());
    }

    #[test]
    fn escaping() {
        assert_eq!(xml_escape("a<b>&\"c"), "a&lt;b&gt;&amp;&quot;c");
        assert_eq!(xml_unescape("a&amp;b&lt;"), "a&b<");
    }

    #[test]
    fn non_public_ips() {
        let ip = |a, b, c, d| Ipv4Addr::new(a, b, c, d);
        for p in [ip(10, 1, 2, 3), ip(172, 16, 0, 1), ip(172, 31, 255, 255), ip(192, 168, 0, 5),
                  ip(100, 64, 0, 1), ip(100, 127, 255, 255), ip(127, 0, 0, 1)] {
            assert!(is_non_public(p), "{p}");
        }
        for p in [ip(8, 8, 8, 8), ip(172, 15, 0, 1), ip(172, 32, 0, 1), ip(100, 63, 0, 1),
                  ip(100, 128, 0, 1), ip(192, 169, 0, 1), ip(203, 0, 113, 7)] {
            assert!(!is_non_public(p), "{p}");
        }
    }

    #[test]
    fn join_code_format() {
        let m = Mapping {
            external_ip: Ipv4Addr::new(1, 2, 3, 4),
            port: 28100,
            local_ip: Ipv4Addr::new(192, 168, 1, 10),
            control_url: String::new(),
            service_type: String::new(),
        };
        assert_eq!(m.join_code(), "1.2.3.4:28100");
    }
}

/// Diagnostic, read-only: the router's WAN IP via GetExternalIPAddress.
/// Opens no port. Used by `examples/upnp_probe.rs` to detect carrier NAT.
pub fn external_ip() -> Result<Ipv4Addr, String> {
    let locations = discover()?;
    let mut last_err = "No UPnP router found (is UPnP enabled on the router?)".to_string();
    for loc in &locations {
        match fetch_service(loc) {
            Ok((control_url, service_type)) => {
                let body = soap_call(&control_url, &service_type, "GetExternalIPAddress", "").map_err(|e| e.msg)?;
                let s = tag_text(&body, "NewExternalIPAddress")
                    .ok_or_else(|| "Router returned no external IP address".to_string())?;
                return s.parse().map_err(|_| format!("Router returned an unusable external IP: '{s}'"));
            }
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}
