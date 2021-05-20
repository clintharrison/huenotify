#![warn(clippy::all)]

use std::{collections::HashMap, time::Duration};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use structopt::StructOpt;
use tokio::net::UdpSocket;
use tokio_util::sync::CancellationToken;
#[derive(PartialEq, Eq, Debug, Clone)]
struct Bridge {
    location: String,
    server: String,
    st: String,
    usn: String,
    hue_bridge_id: String,
}

#[derive(PartialEq, Eq, Debug, Clone)]
struct HueDevice {}

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

    fn list_devices(&mut self) -> Result<Vec<HueDevice>> {
        unimplemented!("list_devices unimplemented")
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

// Parse a M-SEARCH response
fn parse_search_response<'a>(message: &str) -> Result<HeaderMap> {
    // split on \n, even though it might be \r\n
    let (_method_proto, headers) = message.split_once('\n').unwrap_or_default();
    assert!(
        _method_proto.trim_end() == "HTTP/1.1 200 OK",
        "unexpected response line: {}",
        _method_proto.trim_end()
    );
    Ok(headers
        .split('\n')
        .flat_map(|line| line.split_once(": "))
        .map(|(k, v)| (k.trim(), v.trim()))
        .collect())
}

#[test]
fn simple_parse_headers() {
    let hdrs = parse_search_response(BROADCAST_MESSAGE).unwrap();
    assert_ne!(hdrs.len(), 0, "expected some headers to be parsed");
    assert_eq!(*hdrs.get("HOST").unwrap(), "239.255.255.200:1900");
    assert_eq!(
        *hdrs.get("USER-AGENT").unwrap(),
        "Rust/0.1 UPnP/1.1 huenotify/0.1"
    );
    dbg!(hdrs);
}

#[test]
fn untrimmed_parse_headers() {
    let text = "HTTP/1.1 200 OK\r\n\
        HOST: 239.255.255.250:1900\r\n\
        EXT:\r\n\
        CACHE-CONTROL: max-age=100\r\n\
        LOCATION: http://192.168.1.22:80/description.xml\r\n\
        SERVER: Hue/1.0 UPnP/1.0 IpBridge/1.44.0\r\n\
        hue-bridgeid: FAFAFAFFFECECE25\r\n\
        ST: upnp:rootdevice\r\n\
        USN: uuid:5ba27c1b-b3c3-4da3-ac78-16efe35582f6::upnp:rootdevice\r\n\r\n";

    let hdrs = parse_search_response(text).unwrap();
    assert_ne!(hdrs.len(), 0, "expected some headers to be parsed");
    assert_eq!(*hdrs.get("HOST").unwrap(), "239.255.255.200:1900");
    assert_eq!(
        *hdrs.get("USER-AGENT").unwrap(),
        "Rust/0.1 UPnP/1.1 huenotify/0.1"
    );
    dbg!(hdrs);
}

#[derive(PartialEq, Eq)]
enum ReadMode {
    EarlyExit,
    List,
}

async fn read_bridges(
    token: CancellationToken,
    socket: UdpSocket,
    mode: ReadMode,
) -> Result<Vec<Bridge>> {
    let mut bridges = Vec::new();
    let mut buf = [0; 1024];

    let timeout = if mode == ReadMode::List {
        Duration::from_secs(3600)
    } else {
        Duration::from_millis(2000)
    };

    loop {
        let (len, _addr) = tokio::select! {
            read_result = tokio::time::timeout(timeout, socket.recv_from(&mut buf)) => { read_result??  }
            _ = token.cancelled() => { break }
        };
        if len == 0 {
            break;
        }

        let resp = std::str::from_utf8(&buf[0..len])?;
        let headers = parse_search_response(resp)?;
        if let Some(bridge) = Bridge::from_headers(headers) {
            if mode == ReadMode::List {
                println!("{:?}", &bridge);
            }
            bridges.push(bridge);
        }
    }
    Ok(bridges)
}

impl SsdpClient {
    async fn broadcast_discover(
        timeout_millis: u64,
        read_mode: ReadMode,
    ) -> Result<Vec<Bridge>, anyhow::Error> {
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        let dest_addr = "239.255.255.250:1900";
        socket
            .send_to(BROADCAST_MESSAGE.as_bytes(), dest_addr)
            .await?;

        // we _could_ read forever. but we won't, so we'll stop trying after `timeout_millis`
        let token = CancellationToken::new();
        let res = read_bridges(token.clone(), socket, read_mode);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(timeout_millis)).await;
            token.cancel();
        });
        res.await
    }
}

#[derive(StructOpt)]
#[structopt(rename_all = "kebab-case")]
struct Args {
    #[structopt(long, default_value = "5000")]
    timeout: u64,

    #[structopt(long)]
    auto_register: bool,

    #[structopt(subcommand)]
    cmd: Command,
}

#[derive(StructOpt)]
enum Command {
    ListBridges {},

    ListDevices {},

    Poke {
        quantity: u64,
        duration: u64,
        #[structopt(subcommand)]
        target: LightChangeStyle,
    },
}

#[derive(StructOpt)]
enum LightChangeStyle {
    Dim { percent: u64 },
}

#[tokio::main]
async fn main() {
    let args = Args::from_args();
    match args {
        Args {
            timeout,
            auto_register: _,
            cmd: Command::ListBridges {},
        } => {
            let bridges = SsdpClient::broadcast_discover(timeout, ReadMode::List).await;
            match bridges {
                Ok(bridges) => {
                    println!("\n{} bridges discovered.", bridges.len());
                }
                Err(error) => println!("no bridges found, uh oh :(\n{:?}", error),
            }
        }
        Args {
            timeout,
            auto_register,
            cmd: Command::ListDevices {},
        } => {
            match get_one_bridge(timeout, auto_register).await {
                Ok(bridge) => {
                    println!("Using bridge {:?}", &bridge);
                }
                _ => {
                    println!("could not get a hue bridge");
                }
            };
        }
        Args {
            timeout,
            auto_register,
            cmd:
                Command::Poke {
                    quantity,
                    duration,
                    target,
                },
        } => {
            let bridge = get_one_bridge(timeout, auto_register).await;
        }
    }
}

async fn get_one_bridge(timeout: u64, auto_register: bool) -> Result<Bridge> {
    let device = SsdpClient::broadcast_discover(timeout, ReadMode::EarlyExit).await;
    match device {
        Ok(mut devices) if devices.len() == 1 => {
            let device = devices.remove(0);
            println!("using first discovered device: {:?}", &device);
            let username = fetch_username(&device, auto_register)?;

            Ok(device)
        }
        Ok(_) => {
            anyhow::bail!("no bridges found")
        }
        Err(error) => anyhow::bail!("no bridges found, uh oh :(\n{:?}", error),
    }
}

#[derive(Serialize, Deserialize)]
struct AppConfig {
    bridges: Vec<BridgeConfig>,
}

#[derive(Serialize, Deserialize)]
struct BridgeConfig {
    hue_bridge_id: String,
    username: String,
}

impl ::std::default::Default for AppConfig {
    fn default() -> Self {
        Self { bridges: vec![] }
    }
}

// todo: take path as a param or something so we can test this
fn fetch_username(bridge: &Bridge, auto_register: bool) -> Result<String> {
    let config: AppConfig = confy::load("huenotify")?;
    match config
        .bridges
        .iter()
        .find(|b| b.hue_bridge_id == bridge.hue_bridge_id)
    {
        Some(BridgeConfig {
            hue_bridge_id: _,
            username,
        }) => Ok(username.clone()),
        None => {
            anyhow::ensure!(
                auto_register,
                "a username was not found in the config for {:?}, and auto_register is false",
                bridge.hue_bridge_id
            );
            try_auto_register(bridge)
        }
    }
}

fn try_auto_register(bridge: &Bridge) -> Result<String> {
    unimplemented!("auto register not enabled yet")
}
