//! Diagnostic: is a UPnP router answering? Discovery only, opens no port.
//! `cargo run -p coop-net --example upnp_probe`
fn main() {
    match coop_net::upnp::discover_locations() {
        Ok(locs) if locs.is_empty() => println!("no UPnP device answered"),
        Ok(locs) => {
            for l in locs {
                println!("found: {l}");
            }
        }
        Err(e) => println!("error: {e}"),
    }
    match coop_net::upnp::external_ip() {
        Ok(ip) => println!(
            "router WAN IP: {ip}{}",
            if coop_net::upnp::is_non_public(ip) { " (NOT public: carrier NAT, friends cannot connect in)" } else { " (public)" }
        ),
        Err(e) => println!("external IP: error: {e}"),
    }
}
