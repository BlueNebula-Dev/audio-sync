use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::network::{SharedNetworkState, TransportMode};

/// Localhost TCP port used by Apple's `usbmuxd` (Apple Mobile Device Service) on Windows.
pub const USBMUXD_ADDR: &str = "127.0.0.1:27015";

/// TCP port on which the iOS `NetworkClient` `NWListener` accepts USB-tunneled audio & control frames.
pub const IOS_USB_AUDIO_PORT: u16 = 54322;

const USBMUX_HEADER_LEN: usize = 16;
const USBMUX_VERSION_PLIST: u32 = 1;
const USBMUX_TYPE_PLIST: u32 = 8;
const USBMUX_CLIENT_NAME: &str = "AudioSyncPro";

/// Attached iOS device reported by `usbmuxd` `ListDevices`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbmuxDevice {
    pub device_id: u32,
    pub serial: String,
    pub connection_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PlistValue {
    Str(String),
    Int(i64),
    Bool,
    Dict(BTreeMap<String, PlistValue>),
    Array(Vec<PlistValue>),
    Other,
}

impl PlistValue {
    fn get(&self, key: &str) -> Option<&PlistValue> {
        match self {
            PlistValue::Dict(map) => map.get(key),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            PlistValue::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    fn as_int(&self) -> Option<i64> {
        match self {
            PlistValue::Int(i) => Some(*i),
            _ => None,
        }
    }

    fn as_array(&self) -> Option<&[PlistValue]> {
        match self {
            PlistValue::Array(items) => Some(items.as_slice()),
            _ => None,
        }
    }
}

struct PlistParser<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> PlistParser<'a> {
    fn parse(xml: &'a str) -> Option<PlistValue> {
        let mut p = Self { src: xml, pos: 0 };
        if let Some(plist_idx) = p.src.find("<plist") {
            if let Some(gt) = p.src[plist_idx..].find('>') {
                p.pos = plist_idx + gt + 1;
            }
        }
        p.parse_value()
    }

    fn skip_ws(&mut self) {
        while let Some(ch) = self.src[self.pos..].chars().next() {
            if ch.is_whitespace() {
                self.pos += ch.len_utf8();
            } else {
                break;
            }
        }
    }

    fn next_tag(&mut self) -> Option<String> {
        self.skip_ws();
        let rest = &self.src[self.pos..];
        let lt = rest.find('<')?;
        self.pos += lt + 1;
        let gt = self.src[self.pos..].find('>')?;
        let tag = self.src[self.pos..self.pos + gt].trim().to_string();
        self.pos += gt + 1;
        Some(tag)
    }

    fn read_until_close(&mut self, tag: &str) -> Option<String> {
        let close = format!("</{tag}>");
        let rest = &self.src[self.pos..];
        let end = rest.find(&close)?;
        let text = rest[..end]
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&");
        self.pos += end + close.len();
        Some(text)
    }

    fn parse_value(&mut self) -> Option<PlistValue> {
        loop {
            let tag = self.next_tag()?;
            if tag.starts_with('?') || tag.starts_with('!') {
                continue;
            }
            return self.parse_tag(&tag);
        }
    }

    fn parse_tag(&mut self, tag: &str) -> Option<PlistValue> {
        match tag {
            "string" => Some(PlistValue::Str(self.read_until_close("string")?)),
            "integer" => {
                let txt = self.read_until_close("integer")?;
                let parsed = txt.trim().parse::<i64>().ok()?;
                Some(PlistValue::Int(parsed))
            }
            "true/" | "false/" => Some(PlistValue::Bool),
            "array/" => Some(PlistValue::Array(Vec::new())),
            "dict/" => Some(PlistValue::Dict(BTreeMap::new())),
            "dict" => {
                let mut map = BTreeMap::new();
                loop {
                    let t = self.next_tag()?;
                    if t == "/dict" {
                        return Some(PlistValue::Dict(map));
                    }
                    if t != "key" {
                        return None;
                    }
                    let key = self.read_until_close("key")?;
                    let val = self.parse_value()?;
                    map.insert(key, val);
                }
            }
            "array" => {
                let mut items = Vec::new();
                loop {
                    let t = self.next_tag()?;
                    if t == "/array" {
                        return Some(PlistValue::Array(items));
                    }
                    items.push(self.parse_tag(&t)?);
                }
            }
            "data" => {
                let _ = self.read_until_close("data")?;
                Some(PlistValue::Other)
            }
            "real" => {
                let _ = self.read_until_close("real")?;
                Some(PlistValue::Other)
            }
            "date" => {
                let _ = self.read_until_close("date")?;
                Some(PlistValue::Other)
            }
            _ if tag.ends_with('/') => Some(PlistValue::Other),
            _ => None,
        }
    }
}

/// Converts a host-order TCP port into the byte-swapped integer format expected by `usbmuxd` `Connect`.
#[inline]
pub const fn port_to_usbmux_wire(port: u16) -> i64 {
    port.swap_bytes() as i64
}

/// Frames an XML plist payload with the 16-byte little-endian `usbmuxd` protocol header.
pub fn encode_usbmux_request(xml_body: &str, tag: u32) -> Vec<u8> {
    let body = xml_body.as_bytes();
    let total_len = (USBMUX_HEADER_LEN + body.len()) as u32;
    let mut out = Vec::with_capacity(total_len as usize);
    out.extend_from_slice(&total_len.to_le_bytes());
    out.extend_from_slice(&USBMUX_VERSION_PLIST.to_le_bytes());
    out.extend_from_slice(&USBMUX_TYPE_PLIST.to_le_bytes());
    out.extend_from_slice(&tag.to_le_bytes());
    out.extend_from_slice(body);
    out
}

fn build_list_devices_xml() -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         <key>ClientVersionString</key><string>{USBMUX_CLIENT_NAME}</string>\n\
         <key>MessageType</key><string>ListDevices</string>\n\
         <key>ProgName</key><string>{USBMUX_CLIENT_NAME}</string>\n\
         <key>kLibUSBMuxVersion</key><integer>3</integer>\n\
         </dict>\n\
         </plist>\n"
    )
}

fn build_connect_xml(device_id: u32, port: u16) -> String {
    let wire_port = port_to_usbmux_wire(port);
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         <key>ClientVersionString</key><string>{USBMUX_CLIENT_NAME}</string>\n\
         <key>DeviceID</key><integer>{device_id}</integer>\n\
         <key>MessageType</key><string>Connect</string>\n\
         <key>PortNumber</key><integer>{wire_port}</integer>\n\
         <key>ProgName</key><string>{USBMUX_CLIENT_NAME}</string>\n\
         <key>kLibUSBMuxVersion</key><integer>3</integer>\n\
         </dict>\n\
         </plist>\n"
    )
}

/// Parses attached devices from a `usbmuxd` `ListDevices` XML plist response.
pub fn parse_device_list_xml(xml: &str) -> Vec<UsbmuxDevice> {
    let Some(root) = PlistParser::parse(xml) else {
        return Vec::new();
    };
    let Some(list) = root.get("DeviceList").and_then(PlistValue::as_array) else {
        return Vec::new();
    };

    let mut devices = Vec::with_capacity(list.len());
    for entry in list {
        let props = entry.get("Properties");
        let Some(device_id) = entry
            .get("DeviceID")
            .and_then(PlistValue::as_int)
            .or_else(|| props.and_then(|p| p.get("DeviceID")).and_then(PlistValue::as_int))
        else {
            continue;
        };
        let serial = props
            .and_then(|p| p.get("SerialNumber"))
            .and_then(PlistValue::as_str)
            .unwrap_or_default()
            .to_string();
        let connection_type = props
            .and_then(|p| p.get("ConnectionType"))
            .and_then(PlistValue::as_str)
            .unwrap_or_default()
            .to_string();

        if let Ok(id) = u32::try_from(device_id) {
            devices.push(UsbmuxDevice {
                device_id: id,
                serial,
                connection_type,
            });
        }
    }
    devices
}

/// Extracts the numeric status code (`Number`) from a `usbmuxd` `Result` XML plist response.
pub fn parse_result_number_xml(xml: &str) -> Option<i64> {
    let root = PlistParser::parse(xml)?;
    root.get("Number").and_then(PlistValue::as_int)
}

async fn round_trip_usbmux(
    stream: &mut TcpStream,
    xml_body: &str,
    tag: u32,
) -> Result<String, String> {
    let req = encode_usbmux_request(xml_body, tag);
    stream
        .write_all(&req)
        .await
        .map_err(|e| format!("usbmux write failed: {e}"))?;
    stream
        .flush()
        .await
        .map_err(|e| format!("usbmux flush failed: {e}"))?;

    let mut header = [0u8; USBMUX_HEADER_LEN];
    stream
        .read_exact(&mut header)
        .await
        .map_err(|e| format!("usbmux header read failed: {e}"))?;

    let declared_len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    if !(USBMUX_HEADER_LEN..=256 * 1024).contains(&declared_len) {
        return Err(format!("invalid usbmux response length: {declared_len}"));
    }

    let mut body = vec![0u8; declared_len - USBMUX_HEADER_LEN];
    stream
        .read_exact(&mut body)
        .await
        .map_err(|e| format!("usbmux body read failed: {e}"))?;

    String::from_utf8(body).map_err(|e| format!("usbmux response is not UTF-8: {e}"))
}

/// Queries the `usbmuxd` daemon at `addr` (e.g. `127.0.0.1:27015`) for attached USB iOS devices.
pub async fn list_usb_devices_at(addr: &str) -> Result<Vec<UsbmuxDevice>, String> {
    let mut stream = tokio::time::timeout(Duration::from_millis(600), TcpStream::connect(addr))
        .await
        .map_err(|_| format!("usbmuxd connect timeout at {addr}"))?
        .map_err(|e| format!("usbmuxd unavailable at {addr}: {e}"))?;

    let xml = tokio::time::timeout(
        Duration::from_millis(800),
        round_trip_usbmux(&mut stream, &build_list_devices_xml(), 1),
    )
    .await
    .map_err(|_| String::from("usbmuxd ListDevices timeout"))??;

    Ok(parse_device_list_xml(&xml)
        .into_iter()
        .filter(|d| d.connection_type.eq_ignore_ascii_case("USB"))
        .collect())
}

/// Opens a direct low-latency (`TCP_NODELAY`) USB tunnel via `usbmuxd` to `port` on `device_id`.
pub async fn connect_usb_tunnel_at(
    addr: &str,
    device_id: u32,
    port: u16,
) -> Result<TcpStream, String> {
    let mut stream = tokio::time::timeout(Duration::from_millis(600), TcpStream::connect(addr))
        .await
        .map_err(|_| format!("usbmuxd connect timeout at {addr}"))?
        .map_err(|e| format!("usbmuxd unavailable at {addr}: {e}"))?;

    stream
        .set_nodelay(true)
        .map_err(|e| format!("failed to set TCP_NODELAY on USB tunnel: {e}"))?;

    let xml = tokio::time::timeout(
        Duration::from_millis(1000),
        round_trip_usbmux(&mut stream, &build_connect_xml(device_id, port), 2),
    )
    .await
    .map_err(|_| String::from("usbmuxd Connect timeout"))??;

    match parse_result_number_xml(&xml) {
        Some(0) => Ok(stream),
        Some(num) => Err(format!(
            "usbmuxd refused tunnel to device {device_id} port {port} (Number={num})"
        )),
        None => Err(String::from("usbmuxd Connect reply missing Number field")),
    }
}

/// Polls Windows `usbmuxd` (`127.0.0.1:27015`) and establishes a USB Lightning tunnel when an iPhone is attached.
pub struct UsbTunnelWatcher;

impl UsbTunnelWatcher {
    pub fn spawn(
        running: Arc<AtomicBool>,
        state: Arc<SharedNetworkState>,
        usb_stream_active: Arc<AtomicBool>,
        tunnel_tx: mpsc::Sender<(TcpStream, String)>,
    ) {
        tokio::spawn(async move {
            while running.load(Ordering::Relaxed) {
                match list_usb_devices_at(USBMUXD_ADDR).await {
                    Ok(devices) => {
                        state.usbmux_available.store(true, Ordering::Relaxed);
                        if let Some(dev) = devices.into_iter().next() {
                            let label = if dev.serial.len() > 8 {
                                format!("iPhone USB ({}…)", &dev.serial[..8])
                            } else if !dev.serial.is_empty() {
                                format!("iPhone USB ({})", dev.serial)
                            } else {
                                format!("iPhone USB (#{})", dev.device_id)
                            };
                            *state.usb_device_name.lock() = Some(label.clone());

                            let allow_usb = state.transport_mode() != TransportMode::WifiUdp;
                            if allow_usb && !usb_stream_active.load(Ordering::Relaxed) {
                                match connect_usb_tunnel_at(
                                    USBMUXD_ADDR,
                                    dev.device_id,
                                    IOS_USB_AUDIO_PORT,
                                )
                                .await
                                {
                                    Ok(stream) => {
                                        usb_stream_active.store(true, Ordering::SeqCst);
                                        if tunnel_tx.send((stream, label)).await.is_err() {
                                            break;
                                        }
                                    }
                                    Err(err) => {
                                        log::debug!("USB tunnel dial deferred: {err}");
                                    }
                                }
                            }
                        } else {
                            *state.usb_device_name.lock() = None;
                        }
                    }
                    Err(_) => {
                        state.usbmux_available.store(false, Ordering::Relaxed);
                        *state.usb_device_name.lock() = None;
                    }
                }

                tokio::time::sleep(Duration::from_millis(1500)).await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{FramedPacket, PacketType, StreamId};
    use tokio::net::TcpListener;

    #[test]
    fn test_usbmux_request_framing_and_port_swap() {
        assert_eq!(port_to_usbmux_wire(54322), 0x32D4);
        assert_eq!(port_to_usbmux_wire(22087), 0x4756);

        let xml = build_list_devices_xml();
        let req = encode_usbmux_request(&xml, 9);
        let declared = u32::from_le_bytes([req[0], req[1], req[2], req[3]]) as usize;
        let version = u32::from_le_bytes([req[4], req[5], req[6], req[7]]);
        let msg_type = u32::from_le_bytes([req[8], req[9], req[10], req[11]]);
        let tag = u32::from_le_bytes([req[12], req[13], req[14], req[15]]);

        assert_eq!(declared, USBMUX_HEADER_LEN + xml.len());
        assert_eq!(version, USBMUX_VERSION_PLIST);
        assert_eq!(msg_type, USBMUX_TYPE_PLIST);
        assert_eq!(tag, 9);
        assert_eq!(&req[USBMUX_HEADER_LEN..], xml.as_bytes());
    }

    #[test]
    fn test_parse_device_list_and_result_xml() {
        let list_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
    <key>DeviceList</key>
    <array>
        <dict>
            <key>DeviceID</key>
            <integer>7</integer>
            <key>MessageType</key>
            <string>Attached</string>
            <key>Properties</key>
            <dict>
                <key>ConnectionType</key>
                <string>USB</string>
                <key>SerialNumber</key>
                <string>00008110-0011223344556677</string>
            </dict>
        </dict>
        <dict>
            <key>DeviceID</key>
            <integer>8</integer>
            <key>Properties</key>
            <dict>
                <key>ConnectionType</key>
                <string>Network</string>
                <key>SerialNumber</key>
                <string>WIFI-DEV</string>
            </dict>
        </dict>
    </array>
</dict>
</plist>"#;

        let devs = parse_device_list_xml(list_xml);
        assert_eq!(devs.len(), 2);
        assert_eq!(devs[0].device_id, 7);
        assert_eq!(devs[0].connection_type, "USB");
        assert_eq!(devs[0].serial, "00008110-0011223344556677");
        assert_eq!(devs[1].connection_type, "Network");

        let empty_xml =
            "<plist version=\"1.0\"><dict><key>DeviceList</key><array/></dict></plist>";
        assert!(parse_device_list_xml(empty_xml).is_empty());

        let result_ok =
            "<plist version=\"1.0\"><dict><key>MessageType</key><string>Result</string><key>Number</key><integer>0</integer></dict></plist>";
        assert_eq!(parse_result_number_xml(result_ok), Some(0));
    }

    #[tokio::test]
    async fn test_mock_usbmuxd_connect_and_tcp_framed_packet_tunnel() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock usbmuxd");
        let addr = listener.local_addr().unwrap().to_string();

        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut sock, _) = listener.accept().await.expect("accept");
                let mut hdr = [0u8; USBMUX_HEADER_LEN];
                sock.read_exact(&mut hdr).await.expect("read req hdr");
                let total_len = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as usize;
                let tag = u32::from_le_bytes([hdr[12], hdr[13], hdr[14], hdr[15]]);
                let mut body = vec![0u8; total_len - USBMUX_HEADER_LEN];
                sock.read_exact(&mut body).await.expect("read req body");
                let body_str = String::from_utf8(body).unwrap();

                if body_str.contains("ListDevices") {
                    let reply_xml = "<plist version=\"1.0\"><dict><key>DeviceList</key><array><dict><key>DeviceID</key><integer>4</integer><key>Properties</key><dict><key>ConnectionType</key><string>USB</string><key>SerialNumber</key><string>TEST-IPHONE13</string></dict></dict></array></dict></plist>";
                    sock.write_all(&encode_usbmux_request(reply_xml, tag))
                        .await
                        .unwrap();
                } else if body_str.contains("Connect") {
                    assert!(body_str.contains("<integer>4</integer>"));
                    assert!(body_str.contains(&format!("<integer>{}</integer>", port_to_usbmux_wire(IOS_USB_AUDIO_PORT))));
                    let reply_xml = "<plist version=\"1.0\"><dict><key>MessageType</key><string>Result</string><key>Number</key><integer>0</integer></dict></plist>";
                    sock.write_all(&encode_usbmux_request(reply_xml, tag))
                        .await
                        .unwrap();

                    // Socket is now raw pipe: read length-prefixed FramedPacket and echo it back
                    let mut len_buf = [0u8; 2];
                    sock.read_exact(&mut len_buf).await.unwrap();
                    let frame_len = u16::from_be_bytes(len_buf) as usize;
                    let mut frame_buf = vec![0u8; 2 + frame_len];
                    frame_buf[..2].copy_from_slice(&len_buf);
                    sock.read_exact(&mut frame_buf[2..]).await.unwrap();
                    let (pkt, _) = FramedPacket::decode_tcp_frame(&frame_buf).unwrap().unwrap();
                    assert_eq!(pkt.header.stream_id, StreamId::PcToPhone);
                    sock.write_all(&pkt.encode_tcp()).await.unwrap();
                }
            }
        });

        let devices = list_usb_devices_at(&addr).await.expect("list devices");
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].device_id, 4);

        let mut tunnel = connect_usb_tunnel_at(&addr, 4, IOS_USB_AUDIO_PORT)
            .await
            .expect("connect tunnel");
        let pkt = FramedPacket::new(
            PacketType::AudioOpus,
            StreamId::PcToPhone,
            12,
            3456,
            vec![1, 2, 3, 4],
        );
        tunnel.write_all(&pkt.encode_tcp()).await.unwrap();

        let mut rx_buf = [0u8; 64];
        let n = tunnel.read(&mut rx_buf).await.unwrap();
        let (echoed, _) = FramedPacket::decode_tcp_frame(&rx_buf[..n])
            .unwrap()
            .unwrap();
        assert_eq!(echoed, pkt);

        server.await.unwrap();
    }
}
