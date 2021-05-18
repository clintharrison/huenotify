#![warn(clippy::all)]

use std::{collections::HashMap, time::Duration};

use anyhow::Result;
use structopt::StructOpt;
use tokio::net::UdpSocket;
use tokio_util::sync::CancellationToken;

#[derive(PartialEq, Eq, Debug)]
struct Bridge {
    location: String,
    server: String,
    st: String,
    usn: String,
    hue_bridge_id: String,
}

impl<'a> Bridge {
    fn from_headers(headers: HeaderMap<'a>) -> Option<Self> {
        if let (Some(location), Some(server), Some(st), Some(usn), Some(hue_bridge_id)) = (
            headers.get("LOCATION"),
            headers.get("SERVER"),
            headers.get("ST"),
            headers.get("USN"),
            headers.get("hue-bridgeid"),
        ) {
            Some(Bridge {
                location: location.to_string(),
                server: server.to_string(),
                st: st.to_string(),
                usn: usn.to_string(),
                hue_bridge_id: hue_bridge_id.to_string(),
            })
        } else {
            None
        }
    }
}
type HeaderMap<'a> = HashMap<&'a str, &'a str>;

struct SsdpClient {}

const BROADCAST_MESSAGE: &str = "\
    M-SEARCH * HTTP/1.1\r\n\
    HOST: 239.255.255.250:1900\r\n\
    MAN: \"ssdp:discover\"\r\n\
    ST: \"upnp:rootdevice\"\r\n\
    USER-AGENT: Rust/0.1 UPnP/1.1 huenotify/0.1\r\n\
    ";

fn parse_headers<'a>(message: &str) -> Result<HeaderMap> {
    // split on \n, even though it might be \r\n
    let (_method_proto, headers) = message.split_once('\n').unwrap_or_default();
    Ok(headers
        .split('\n')
        .flat_map(|line| line.split_once(": "))
        .map(|(k, v)| (k.trim(), v.trim()))
        .collect())
}

#[test]
fn simple_parse_headers() {
    let hdrs = parse_headers(BROADCAST_MESSAGE).unwrap();
    assert_ne!(hdrs.len(), 0, "expected some headers to be parsed");
    assert_eq!(*hdrs.get("HOST").unwrap(), "239.255.255.200:1900");
    assert_eq!(*hdrs.get("USER-AGENT").unwrap(), "Rust/0.1 UPnP/1.1 huenotify/0.1");
    dbg!(hdrs);
}

async fn read_devices(token: CancellationToken, socket: UdpSocket) -> Result<Vec<Bridge>> {
    let mut devices = Vec::new();
    let mut buf = [0; 1024];
    loop {
        if token.is_cancelled() {
            break;
        }

        let res =
            tokio::time::timeout(Duration::from_millis(1000), socket.recv_from(&mut buf)).await;
        if let Err(_) = res {
            println!("timer's up!");
            break;
        }
        let (len, _addr) = res.unwrap()?;
        if len == 0 {
            break;
        }
        let resp = std::str::from_utf8(&buf[0..len])?;
        println!("msg: {:?}", resp);
        let headers = parse_headers(resp)?;

        if let Some(bridge) = Bridge::from_headers(headers) {
            devices.push(bridge);
            // should we keep going..?
            break;
        }
    }
    Ok(devices)
}

impl SsdpClient {
    async fn broadcast_discover(timeout_millis: u64) -> Result<Vec<Bridge>, anyhow::Error> {
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        let dest_addr = "239.255.255.250:1900";
        socket
            .send_to(BROADCAST_MESSAGE.as_bytes(), dest_addr)
            .await?;

        // we _could_ read forever. but we won't, so we'll stop trying after `timeout_millis`
        let token = CancellationToken::new();
        let res = read_devices(token.clone(), socket);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(timeout_millis)).await;
            token.cancel();
        });
        res.await
    }
}

#[derive(StructOpt)]
enum Args {
    List {
        #[structopt(default_value = "5000")]
        timeout: u64,
    },
}

#[tokio::main]
async fn main() {
    let command = Args::from_args();

    match command {
        Args::List { timeout } => {
            let devices = SsdpClient::broadcast_discover(timeout).await;
            match devices {
                Ok(bridges) => {
                    for bridge in bridges.iter() {
                        println!("{:?}", bridge)
                    }
                    println!("\n{} devices discovered.", bridges.len());
                }
                Err(error) => println!("no bridges found, uh oh :(\n{:?}", error),
            }
        }
    }
}
