#![warn(clippy::all)]

use std::{collections::HashMap, time::Duration};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use structopt::StructOpt;
use tokio::net::UdpSocket;
use tokio_util::sync::CancellationToken;
use url::Url;
#[derive(PartialEq, Eq, Debug, Clone)]
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

    fn short_name(&self) -> String{
        let description_xml: Url = Url::parse(&self.location).expect("could not parse self.location");
        Url::parse(&description_xml[..url::Position::BeforePath]).expect("could not find partial path").to_string()
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
    let hdrs = parse_search_response(BROADCAST_MESSAGE).expect("broadcast message must parse");
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
        Duration::from_millis(5000)
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

            if mode == ReadMode::EarlyExit {
                break;
            }
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
    no_auto_register: bool,

    #[structopt(subcommand)]
    cmd: Command,
}

#[derive(StructOpt)]
enum Command {
    ListBridges {},

    ListLights {},

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
            no_auto_register: _,
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
            no_auto_register,
            cmd: Command::ListLights {},
        } => {
            match get_one_bridge(timeout, !no_auto_register).await {
                Ok(authed_bridge) => {
                    println!(
                        "Using bridge {} as user {}",
                        &authed_bridge.bridge.short_name(), &authed_bridge.username
                    );
                    let lights = authed_bridge
                        .list_lights()
                        .await
                        .expect("could not retrieve list of lights");
                    for light in lights {
                        println!("{:?}", light);
                    }
                }
                Err(err) => {
                    println!("could not get a hue bridge, {:?}", err);
                }
            };
        }
        Args {
            timeout,
            no_auto_register,
            cmd:
                Command::Poke {
                    quantity: _,
                    duration: _,
                    target: _,
                },
        } => {
            let _bridge = get_one_bridge(timeout, !no_auto_register).await;
        }
    }
}

type ListLightsResponse = HashMap<String, Light>;

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Light {
    name: String,
}

struct AuthenticatedBridge {
    username: String,
    bridge: Bridge,
}

impl AuthenticatedBridge {
    async fn list_lights(&self) -> Result<Vec<Light>> {
        let url = url_for_bridge(
            &self.bridge,
            format!("/api/{}/lights", &self.username).as_str(),
        )
        .expect("could not construct url for lights");
        let res = reqwest::get(url).await?;
        Ok(res
            .json::<ListLightsResponse>()
            .await?
            .values()
            .cloned()
            .collect())
    }
}

async fn get_one_bridge(timeout: u64, auto_register: bool) -> Result<AuthenticatedBridge> {
    let res = SsdpClient::broadcast_discover(timeout, ReadMode::EarlyExit).await;
    match res {
        Ok(mut bridges) if bridges.len() == 1 => {
            let bridge = bridges.remove(0);
            let username = fetch_username(&bridge, auto_register).await?;

            Ok(AuthenticatedBridge { username, bridge })
        }
        Ok(_) => {
            anyhow::bail!("no bridges found")
        }
        Err(error) => anyhow::bail!("no bridges found, uh oh :(\n{:?}", error),
    }
}

#[derive(Serialize, Deserialize, Debug)]
struct AppConfig {
    bridges: HashMap<String, String>,
}

impl ::std::default::Default for AppConfig {
    fn default() -> Self {
        Self {
            bridges: HashMap::new(),
        }
    }
}

// todo: take path as a param or something so we can test this
async fn fetch_username(bridge: &Bridge, auto_register: bool) -> Result<String> {
    let config: AppConfig = confy::load("huenotify")?;

    if let Some(username) = config.bridges.get(&bridge.hue_bridge_id) {
        Ok(username.clone())
    } else {
        anyhow::ensure!(
            auto_register,
            "a username was not found in the config for {:?}, and auto_register is false",
            bridge.hue_bridge_id
        );
        try_auto_register(config, bridge).await
    }
}

fn url_for_bridge(bridge: &Bridge, path: &str) -> Result<Url, url::ParseError> {
    let description_xml: Url = Url::parse(&bridge.location)?;
    Url::parse(&description_xml[..url::Position::BeforePath])?.join(path)
}

#[derive(Serialize, Deserialize, Debug)]
struct RegisterResponse {
    success: RegisterResponseSuccess,
}

#[derive(Serialize, Deserialize, Debug)]
struct RegisterResponseSuccess {
    username: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct HueErrorResponse {
    error: HueErrorType,
}

#[derive(Serialize, Deserialize, Debug)]
struct HueErrorType {
    #[serde(rename = "type")]
    error_type: u64,
    address: String,
    description: String,
}

async fn try_auto_register(mut config: AppConfig, bridge: &Bridge) -> Result<String> {
    println!("trying to auto register...");
    let registration_url = url_for_bridge(bridge, "/api");
    let client = reqwest::Client::new();

    let body: HashMap<&str, &str> = [("devicetype", "huenotify")].iter().cloned().collect();

    let res = client.post(registration_url.expect("registration url must be valid"));

    let res = res.json(&body).send().await?;

    let body = res.text().await?;

    let register_responses: Vec<RegisterResponse> = match serde_json::de::from_str(&body).context("deserializing response from register request") {
        Ok(rs) => rs,
        Err(e) => {
            if let Ok(hue_error) = serde_json::de::from_str::<Vec<HueErrorResponse>>(&body) {
                anyhow::bail!("got hue error message!\n{:#?}", &hue_error);
            };
            anyhow::bail!("got an error from register response: {}", e)
        }
    };

    anyhow::ensure!(
        register_responses.len() == 1,
        "register response should have one result:\n{:#?}",
        register_responses
    );

    let username = register_responses[0].success.username.clone();
    println!("got a username {}", username);
    if let Some(x) = config.bridges.get_mut(&bridge.hue_bridge_id) {
        *x = username.clone();
    }
    if let Err(err) = confy::store("huenotify", config) {
        println!("couldn't save config! {:?}", err);
    }

    Ok(username)
}
