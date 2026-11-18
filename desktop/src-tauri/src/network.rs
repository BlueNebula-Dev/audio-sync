use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket as StdUdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mdns_sd::{ServiceDaemon, ServiceInfo};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use crate::audio_engine::AudioConfig;
pub use crate::protocol::{
    sequence_diff, ControlMessage, FramedPacket, PacketHeader, PacketType, ProtocolError, StreamId,
    HEADER_SIZE, MAX_TCP_FRAME_SIZE, MAX_UDP_DATAGRAM_SIZE, TCP_LENGTH_PREFIX_SIZE,
};
use crate::usb_tunnel::UsbTunnelWatcher;

/// User-selected transport preference between PC and iPhone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportMode {
    Auto,
    WifiUdp,
    UsbCable,
}

impl TransportMode {
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::WifiUdp => 1,
            Self::UsbCable => 2,
        }
    }

    pub fn from_u8(val: u8) -> Self {
        match val {
            1 => Self::WifiUdp,
            2 => Self::UsbCable,
            _ => Self::Auto,
        }
    }
}

/// Currently active audio/control link transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActiveTransport {
    None,
    WifiUdp,
    UsbCable,
}

impl ActiveTransport {
    pub fn as_u8(self) -> u8 {
        match self {
            Self::None => 0,
            Self::WifiUdp => 1,
            Self::UsbCable => 2,
        }
    }

    pub fn from_u8(val: u8) -> Self {
        match val {
            1 => Self::WifiUdp,
            2 => Self::UsbCable,
            _ => Self::None,
        }
    }
}

/// Default UDP port for bi-directional Opus audio & control packets.
pub const DEFAULT_UDP_PORT: u16 = 54321;

/// mDNS / Bonjour service type discovered by the iPhone client (`NWBrowser`).
pub const MDNS_SERVICE_TYPE: &str = "_audiosync._udp.local.";

#[derive(Debug, Error)]
pub enum NetworkError {
    #[error("Failed to bind UDP socket on port {port}: {source}")]
    BindError {
        port: u16,
        #[source]
        source: std::io::Error,
    },
    #[error("mDNS daemon error: {0}")]
    MdnsError(String),
    #[error("Protocol error: {0}")]
    Protocol(#[from] ProtocolError),
}

/// Snapshot of network connection status, peer metadata, and latency telemetry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetworkStatus {
    pub local_ip: String,
    pub udp_port: u16,
    pub mdns_active: bool,
    pub connected: bool,
    pub peer_address: Option<String>,
    pub peer_device_name: Option<String>,
    pub rtt_ms: f32,
    pub packets_sent: u64,
    pub packets_received: u64,
    pub packets_lost: u64,
    pub transport_mode: TransportMode,
    pub active_transport: ActiveTransport,
    pub usbmux_available: bool,
    pub usb_device_name: Option<String>,
}

/// Lock-free / shared network state updated by the UDP/USB network loops.
pub struct SharedNetworkState {
    pub local_ip: Mutex<String>,
    pub udp_port: AtomicU32,
    pub mdns_active: AtomicBool,
    pub connected: AtomicBool,
    pub peer_addr: Mutex<Option<SocketAddr>>,
    pub peer_device_name: Mutex<Option<String>>,
    pub last_peer_seen: Mutex<Option<Instant>>,
    pub rtt_micros: AtomicU32,
    pub packets_sent: AtomicU64,
    pub packets_received: AtomicU64,
    pub packets_lost: AtomicU64,
    pub config_revision: AtomicU64,
    pub transport_mode: AtomicU8,
    pub active_transport: AtomicU8,
    pub usbmux_available: AtomicBool,
    pub usb_device_name: Mutex<Option<String>>,
}

impl Default for SharedNetworkState {
    fn default() -> Self {
        Self {
            local_ip: Mutex::new(detect_local_ipv4().to_string()),
            udp_port: AtomicU32::new(DEFAULT_UDP_PORT as u32),
            mdns_active: AtomicBool::new(false),
            connected: AtomicBool::new(false),
            peer_addr: Mutex::new(None),
            peer_device_name: Mutex::new(None),
            last_peer_seen: Mutex::new(None),
            rtt_micros: AtomicU32::new(0),
            packets_sent: AtomicU64::new(0),
            packets_received: AtomicU64::new(0),
            packets_lost: AtomicU64::new(0),
            config_revision: AtomicU64::new(1),
            transport_mode: AtomicU8::new(TransportMode::Auto.as_u8()),
            active_transport: AtomicU8::new(ActiveTransport::None.as_u8()),
            usbmux_available: AtomicBool::new(false),
            usb_device_name: Mutex::new(None),
        }
    }
}

impl SharedNetworkState {
    pub fn transport_mode(&self) -> TransportMode {
        TransportMode::from_u8(self.transport_mode.load(Ordering::Relaxed))
    }

    pub fn set_transport_mode(&self, mode: TransportMode) {
        self.transport_mode.store(mode.as_u8(), Ordering::SeqCst);
    }

    pub fn active_transport(&self) -> ActiveTransport {
        ActiveTransport::from_u8(self.active_transport.load(Ordering::Relaxed))
    }

    pub fn set_active_transport(&self, active: ActiveTransport) {
        self.active_transport
            .store(active.as_u8(), Ordering::Relaxed);
    }

    pub fn next_revision(&self) -> u64 {
        self.config_revision.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn current_revision(&self) -> u64 {
        self.config_revision.load(Ordering::SeqCst)
    }

    pub fn build_sync_message(&self, cfg: &AudioConfig, revision: u64) -> ControlMessage {
        ControlMessage::SyncConfig {
            revision,
            pc_to_phone_enabled: cfg.pc_to_phone_enabled,
            phone_to_pc_enabled: cfg.phone_to_pc_enabled,
            pc_muted: cfg.pc_muted,
            mic_muted: cfg.mic_muted,
            push_to_talk_enabled: cfg.push_to_talk_enabled,
            push_to_talk_active: cfg.push_to_talk_active,
            aec_enabled: cfg.aec_enabled,
            rnnoise_enabled: cfg.rnnoise_enabled,
            vad_gate_threshold: cfg.vad_gate_threshold,
            mic_gain_db: cfg.mic_gain_db,
            pc_volume: cfg.pc_volume,
            opus_bitrate_bps: cfg.opus_bitrate_bps,
            frame_duration_ms: cfg.frame_duration_ms,
            jitter_buffer_packets: cfg.jitter_buffer_packets,
        }
    }
    pub fn snapshot(&self) -> NetworkStatus {
        let rtt_ms = self.rtt_micros.load(Ordering::Relaxed) as f32 / 1000.0;
        NetworkStatus {
            local_ip: self.local_ip.lock().clone(),
            udp_port: self.udp_port.load(Ordering::Relaxed) as u16,
            mdns_active: self.mdns_active.load(Ordering::Relaxed),
            connected: self.connected.load(Ordering::Relaxed),
            peer_address: self.peer_addr.lock().map(|a| a.to_string()),
            peer_device_name: self.peer_device_name.lock().clone(),
            rtt_ms,
            packets_sent: self.packets_sent.load(Ordering::Relaxed),
            packets_received: self.packets_received.load(Ordering::Relaxed),
            packets_lost: self.packets_lost.load(Ordering::Relaxed),
            transport_mode: self.transport_mode(),
            active_transport: self.active_transport(),
            usbmux_available: self.usbmux_available.load(Ordering::Relaxed),
            usb_device_name: self.usb_device_name.lock().clone(),
        }
    }

    /// Manually sets the target iPhone IP & port (for manual IP fallback in the UI).
    pub fn set_manual_peer(&self, addr: SocketAddr) {
        *self.peer_addr.lock() = Some(addr);
        *self.last_peer_seen.lock() = Some(Instant::now());
        self.connected.store(true, Ordering::Relaxed);
    }
}

/// Detects the primary LAN (or iPhone USB Personal Hotspot `172.20.10.x`) IPv4 address without sending traffic.
pub fn detect_local_ipv4() -> Ipv4Addr {
    for target in ["8.8.8.8:80", "172.20.10.1:54321"] {
        if let Ok(sock) = StdUdpSocket::bind("0.0.0.0:0") {
            if sock.connect(target).is_ok() {
                if let Ok(local_addr) = sock.local_addr() {
                    if let IpAddr::V4(ipv4) = local_addr.ip() {
                        if !ipv4.is_loopback() {
                            return ipv4;
                        }
                    }
                }
            }
        }
    }
    Ipv4Addr::new(127, 0, 0, 1)
}

/// Active mDNS responder guard that broadcasts `_audiosync._udp.local.` on the local Wi-Fi network.
pub struct MdnsResponder {
    daemon: ServiceDaemon,
    fullname: String,
}

impl MdnsResponder {
    pub fn start(port: u16, state: &SharedNetworkState) -> Result<Self, NetworkError> {
        let daemon =
            ServiceDaemon::new().map_err(|e| NetworkError::MdnsError(e.to_string()))?;

        let ipv4 = detect_local_ipv4();
        *state.local_ip.lock() = ipv4.to_string();

        let host_label = std::env::var("COMPUTERNAME")
            .unwrap_or_else(|_| "AudioSync-PC".to_string())
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect::<String>();

        let instance_name = if host_label.is_empty() {
            "AudioSync-PC".to_string()
        } else {
            format!("AudioSync-{host_label}")
        };
        let host_fqdn = format!("{instance_name}.local.");

        let properties = [
            ("version", "1.0"),
            ("codec", "opus"),
            ("sample_rate", "48000"),
            ("channels", "2"),
            ("duplex", "full"),
        ];

        let service_info = ServiceInfo::new(
            MDNS_SERVICE_TYPE,
            &instance_name,
            &host_fqdn,
            IpAddr::V4(ipv4),
            port,
            &properties[..],
        )
        .map_err(|e| NetworkError::MdnsError(e.to_string()))?;

        let fullname = service_info.get_fullname().to_string();
        daemon
            .register(service_info)
            .map_err(|e| NetworkError::MdnsError(e.to_string()))?;

        state.mdns_active.store(true, Ordering::Relaxed);
        log::info!(
            "mDNS responder broadcasting {} at {}:{}",
            fullname,
            ipv4,
            port
        );

        Ok(Self { daemon, fullname })
    }

    pub fn shutdown(self, state: &SharedNetworkState) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
        state.mdns_active.store(false, Ordering::Relaxed);
    }
}

/// Manages the non-blocking Tokio UDP socket loop and mDNS responder.
pub struct NetworkEngine {
    pub state: Arc<SharedNetworkState>,
    audio_config: Arc<Mutex<AudioConfig>>,
    running: Arc<AtomicBool>,
}

impl NetworkEngine {
    pub fn new(audio_config: Arc<Mutex<AudioConfig>>) -> Self {
        Self {
            state: Arc::new(SharedNetworkState::default()),
            audio_config,
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    /// Starts the mDNS responder and spawns the non-blocking bi-directional UDP streaming task.
    pub async fn start(
        &self,
        port: u16,
        mut outbound_opus_rx: mpsc::Receiver<Vec<u8>>,
        inbound_mic_tx: mpsc::Sender<FramedPacket>,
        mut outbound_control_rx: mpsc::Receiver<ControlMessage>,
        remote_config_tx: mpsc::UnboundedSender<AudioConfig>,
    ) -> Result<(), NetworkError> {
        if self.running.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        self.state.udp_port.store(port as u32, Ordering::Relaxed);

        let bind_addr = SocketAddr::from(([0, 0, 0, 0], port));
        let socket = UdpSocket::bind(bind_addr)
            .await
            .map_err(|source| NetworkError::BindError { port, source })?;

        let mdns = match MdnsResponder::start(port, &self.state) {
            Ok(responder) => Some(responder),
            Err(err) => {
                log::warn!("mDNS responder failed to start (manual IP still available): {err}");
                None
            }
        };

        let running = Arc::clone(&self.running);
        let state = Arc::clone(&self.state);
        let audio_config = Arc::clone(&self.audio_config);

        let (usb_tunnel_tx, mut usb_tunnel_rx) =
            mpsc::channel::<(tokio::net::TcpStream, String)>(4);
        let (usb_pkt_tx, mut usb_pkt_rx) = mpsc::channel::<FramedPacket>(256);
        let usb_stream_active = Arc::new(AtomicBool::new(false));

        UsbTunnelWatcher::spawn(
            Arc::clone(&running),
            Arc::clone(&state),
            Arc::clone(&usb_stream_active),
            usb_tunnel_tx,
        );

        tokio::spawn(async move {
            let start_instant = Instant::now();
            // Stack-allocated wire buffers (zero heap allocations in packet hot paths)
            let mut recv_buf = [0u8; MAX_TCP_FRAME_SIZE + TCP_LENGTH_PREFIX_SIZE];
            let mut send_buf = [0u8; MAX_TCP_FRAME_SIZE + TCP_LENGTH_PREFIX_SIZE];

            let mut outbound_seq: u16 = 0;
            let mut ping_seq: u16 = 0;
            let mut last_inbound_mic_seq: Option<u16> = None;

            let mut usb_writer: Option<OwnedWriteHalf> = None;
            let mut usb_reader_handle: Option<tokio::task::JoinHandle<()>> = None;

            let mut ping_interval = tokio::time::interval(Duration::from_secs(1));
            ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            while running.load(Ordering::Relaxed) {
                // Tear down active USB socket immediately if user forced Wi-Fi UDP mode
                if state.transport_mode() == TransportMode::WifiUdp && usb_writer.is_some() {
                    usb_writer = None;
                    if let Some(handle) = usb_reader_handle.take() {
                        handle.abort();
                    }
                    usb_stream_active.store(false, Ordering::SeqCst);
                    if state.peer_addr.lock().is_some() {
                        state.set_active_transport(ActiveTransport::WifiUdp);
                    } else {
                        state.set_active_transport(ActiveTransport::None);
                    }
                }

                tokio::select! {
                    // 0. Newly established USB-to-Lightning `usbmuxd` tunnel
                    Some((stream, usb_label)) = usb_tunnel_rx.recv() => {
                        if state.transport_mode() == TransportMode::WifiUdp {
                            usb_stream_active.store(false, Ordering::SeqCst);
                            continue;
                        }
                        if let Some(handle) = usb_reader_handle.take() {
                            handle.abort();
                        }
                        let _ = stream.set_nodelay(true);
                        let (mut reader, writer) = stream.into_split();
                        usb_writer = Some(writer);
                        *state.peer_device_name.lock() = Some(usb_label);
                        *state.last_peer_seen.lock() = Some(Instant::now());
                        state.connected.store(true, Ordering::Relaxed);
                        state.set_active_transport(ActiveTransport::UsbCable);

                        let pkt_tx = usb_pkt_tx.clone();
                        let active_flag = Arc::clone(&usb_stream_active);
                        let st_clone = Arc::clone(&state);
                        usb_reader_handle = Some(tokio::spawn(async move {
                            let mut acc = [0u8; 16384];
                            let mut acc_len: usize = 0;
                            let mut tmp = [0u8; 4096];
                            loop {
                                match reader.read(&mut tmp).await {
                                    Ok(0) | Err(_) => break,
                                    Ok(n) => {
                                        if acc_len + n > acc.len() {
                                            acc_len = 0;
                                        }
                                        let copy_n = n.min(acc.len() - acc_len);
                                        acc[acc_len..acc_len + copy_n].copy_from_slice(&tmp[..copy_n]);
                                        acc_len += copy_n;

                                        let mut offset = 0usize;
                                        while offset < acc_len {
                                            match FramedPacket::decode_tcp_frame(&acc[offset..acc_len]) {
                                                Ok(Some((pkt, consumed))) => {
                                                    offset += consumed;
                                                    if pkt_tx.try_send(pkt).is_err() {
                                                        // Drop oldest under backpressure rather than stalling USB socket
                                                    }
                                                }
                                                Ok(None) => break,
                                                Err(_) => {
                                                    acc_len = 0;
                                                    offset = 0;
                                                    break;
                                                }
                                            }
                                        }
                                        if offset > 0 && offset < acc_len {
                                            acc.copy_within(offset..acc_len, 0);
                                            acc_len -= offset;
                                        } else if offset >= acc_len {
                                            acc_len = 0;
                                        }
                                    }
                                }
                            }
                            active_flag.store(false, Ordering::SeqCst);
                            if st_clone.active_transport() == ActiveTransport::UsbCable {
                                st_clone.set_active_transport(ActiveTransport::None);
                            }
                        }));

                        // Send an immediate Ping + SyncConfig over the new USB link
                        if let Some(writer) = usb_writer.as_mut() {
                            let ts_micros = (start_instant.elapsed().as_micros() & 0xFFFF_FFFF) as u32;
                            let ping = FramedPacket::ping(ping_seq, ts_micros);
                            ping_seq = ping_seq.wrapping_add(1);
                            if let Ok(len) = ping.encode_tcp_into(&mut send_buf) {
                                let _ = writer.write_all(&send_buf[..len]).await;
                            }
                            let cfg = audio_config.lock().clone();
                            let sync_msg = state.build_sync_message(&cfg, state.current_revision().max(1));
                            if let Ok(pkt) = FramedPacket::control(0, ts_micros, &sync_msg) {
                                if let Ok(len) = pkt.encode_tcp_into(&mut send_buf) {
                                    let _ = writer.write_all(&send_buf[..len]).await;
                                }
                            }
                        }
                    }

                    // 1a. Non-blocking inbound USB TCP framed packet handler
                    Some(packet) = usb_pkt_rx.recv() => {
                        if state.transport_mode() == TransportMode::WifiUdp {
                            continue;
                        }
                        *state.last_peer_seen.lock() = Some(Instant::now());
                        state.connected.store(true, Ordering::Relaxed);
                        state.set_active_transport(ActiveTransport::UsbCable);
                        state.packets_received.fetch_add(1, Ordering::Relaxed);

                        handle_inbound_packet(
                            packet,
                            None,
                            &socket,
                            &mut usb_writer,
                            &usb_stream_active,
                            &state,
                            &audio_config,
                            &inbound_mic_tx,
                            &remote_config_tx,
                            &start_instant,
                            &mut last_inbound_mic_seq,
                            &mut send_buf,
                        ).await;
                    }

                    // 1b. Non-blocking inbound UDP datagram handler
                    recv_res = socket.recv_from(&mut recv_buf) => {
                        match recv_res {
                            Ok((len, src_addr)) => {
                                if len < HEADER_SIZE {
                                    continue;
                                }
                                let Ok(packet) = FramedPacket::decode(&recv_buf[..len]) else {
                                    continue;
                                };

                                *state.peer_addr.lock() = Some(src_addr);

                                // If USB Cable mode is exclusively selected or USB is actively streaming, ignore redundant UDP traffic
                                let mode = state.transport_mode();
                                if mode == TransportMode::UsbCable
                                    || (mode == TransportMode::Auto && usb_writer.is_some() && usb_stream_active.load(Ordering::Relaxed))
                                {
                                    continue;
                                }

                                *state.last_peer_seen.lock() = Some(Instant::now());
                                state.connected.store(true, Ordering::Relaxed);
                                state.set_active_transport(ActiveTransport::WifiUdp);
                                state.packets_received.fetch_add(1, Ordering::Relaxed);

                                handle_inbound_packet(
                                    packet,
                                    Some(src_addr),
                                    &socket,
                                    &mut usb_writer,
                                    &usb_stream_active,
                                    &state,
                                    &audio_config,
                                    &inbound_mic_tx,
                                    &remote_config_tx,
                                    &start_instant,
                                    &mut last_inbound_mic_seq,
                                    &mut send_buf,
                                ).await;
                            }
                            Err(err) => {
                                log::debug!("UDP recv_from transient error: {err}");
                            }
                        }
                    }

                    // 2. Non-blocking outbound PC-to-iPhone audio frame transmitter
                    Some(opus_payload) = outbound_opus_rx.recv() => {
                        let ts_micros = (start_instant.elapsed().as_micros() & 0xFFFF_FFFF) as u32;
                        let pkt = FramedPacket::new(
                            PacketType::AudioOpus,
                            StreamId::PcToPhone,
                            outbound_seq,
                            ts_micros,
                            opus_payload,
                        );
                        outbound_seq = outbound_seq.wrapping_add(1);

                        if send_packet_active(
                            &pkt,
                            None,
                            &socket,
                            &mut usb_writer,
                            &usb_stream_active,
                            &state,
                            &mut send_buf,
                        ).await {
                            state.packets_sent.fetch_add(1, Ordering::Relaxed);
                        }
                    }

                    // 3. Outbound control synchronization messages (SyncConfig, MuteState, etc.)
                    Some(ctrl_msg) = outbound_control_rx.recv() => {
                        let ts_micros = (start_instant.elapsed().as_micros() & 0xFFFF_FFFF) as u32;
                        if let Ok(pkt) = FramedPacket::control(0, ts_micros, &ctrl_msg) {
                            let _ = send_packet_active(
                                &pkt,
                                None,
                                &socket,
                                &mut usb_writer,
                                &usb_stream_active,
                                &state,
                                &mut send_buf,
                            ).await;
                        }
                    }

                    // 4. Periodic Ping latency probe & peer timeout watchdog
                    _ = ping_interval.tick() => {
                        if usb_writer.is_some() && !usb_stream_active.load(Ordering::Relaxed) {
                            usb_writer = None;
                            if state.active_transport() == ActiveTransport::UsbCable {
                                state.set_active_transport(ActiveTransport::None);
                            }
                        }

                        let timed_out = {
                            let last_seen = *state.last_peer_seen.lock();
                            match last_seen {
                                Some(instant) => instant.elapsed() > Duration::from_secs(8),
                                None => false,
                            }
                        };

                        if timed_out {
                            state.connected.store(false, Ordering::Relaxed);
                            state.set_active_transport(ActiveTransport::None);
                        }

                        if state.connected.load(Ordering::Relaxed) {
                            let ts_micros = (start_instant.elapsed().as_micros() & 0xFFFF_FFFF) as u32;
                            let ping = FramedPacket::ping(ping_seq, ts_micros);
                            ping_seq = ping_seq.wrapping_add(1);
                            let _ = send_packet_active(
                                &ping,
                                None,
                                &socket,
                                &mut usb_writer,
                                &usb_stream_active,
                                &state,
                                &mut send_buf,
                            ).await;
                        }
                    }
                }
            }

            if let Some(handle) = usb_reader_handle.take() {
                handle.abort();
            }
            if let Some(responder) = mdns {
                responder.shutdown(&state);
            }
        });

        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
async fn send_packet_active(
    pkt: &FramedPacket,
    explicit_udp_addr: Option<SocketAddr>,
    socket: &UdpSocket,
    usb_writer: &mut Option<OwnedWriteHalf>,
    usb_stream_active: &AtomicBool,
    state: &SharedNetworkState,
    send_buf: &mut [u8],
) -> bool {
    let mode = state.transport_mode();
    if mode != TransportMode::WifiUdp && usb_stream_active.load(Ordering::Relaxed) {
        if let Some(writer) = usb_writer.as_mut() {
            if let Ok(encoded_len) = pkt.encode_tcp_into(send_buf) {
                if writer.write_all(&send_buf[..encoded_len]).await.is_ok() {
                    return true;
                }
                // USB connection dropped; clean up writer so Auto mode can fall back to Wi-Fi UDP
                *usb_writer = None;
                usb_stream_active.store(false, Ordering::SeqCst);
                if state.active_transport() == ActiveTransport::UsbCable {
                    state.set_active_transport(ActiveTransport::None);
                }
            }
        }
    }

    if mode != TransportMode::UsbCable {
        let target_addr = explicit_udp_addr.or_else(|| *state.peer_addr.lock());
        if let Some(peer_addr) = target_addr {
            if let Ok(encoded_len) = pkt.encode_into(send_buf) {
                return socket.send_to(&send_buf[..encoded_len], peer_addr).await.is_ok();
            }
        }
    }

    false
}

#[allow(clippy::too_many_arguments)]
async fn handle_inbound_packet(
    packet: FramedPacket,
    src_addr: Option<SocketAddr>,
    socket: &UdpSocket,
    usb_writer: &mut Option<OwnedWriteHalf>,
    usb_stream_active: &AtomicBool,
    state: &SharedNetworkState,
    audio_config: &Mutex<AudioConfig>,
    inbound_mic_tx: &mpsc::Sender<FramedPacket>,
    remote_config_tx: &mpsc::UnboundedSender<AudioConfig>,
    start_instant: &Instant,
    last_inbound_mic_seq: &mut Option<u16>,
    send_buf: &mut [u8],
) {
    match packet.header.packet_type {
        PacketType::AudioOpus => {
            if packet.header.stream_id == StreamId::PhoneToPc {
                if let Some(prev_seq) = *last_inbound_mic_seq {
                    let diff = sequence_diff(packet.header.sequence_num, prev_seq);
                    if diff > 1 {
                        state
                            .packets_lost
                            .fetch_add((diff - 1) as u64, Ordering::Relaxed);
                    }
                }
                *last_inbound_mic_seq = Some(packet.header.sequence_num);
                let _ = inbound_mic_tx.try_send(packet);
            }
        }
        PacketType::Ping => {
            let pong = FramedPacket::pong(packet.header.sequence_num, packet.header.timestamp);
            let _ = send_packet_active(
                &pong,
                src_addr,
                socket,
                usb_writer,
                usb_stream_active,
                state,
                send_buf,
            )
            .await;
        }
        PacketType::Pong => {
            let now_micros = (start_instant.elapsed().as_micros() & 0xFFFF_FFFF) as u32;
            let rtt_us = now_micros.wrapping_sub(packet.header.timestamp);
            state.rtt_micros.store(rtt_us, Ordering::Relaxed);
        }
        PacketType::Control => {
            if let Ok(ctrl_msg) = serde_json::from_slice::<ControlMessage>(&packet.payload) {
                handle_control_message(
                    ctrl_msg,
                    src_addr,
                    socket,
                    usb_writer,
                    usb_stream_active,
                    state,
                    audio_config,
                    remote_config_tx,
                    start_instant,
                    send_buf,
                )
                .await;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_control_message(
    msg: ControlMessage,
    src_addr: Option<SocketAddr>,
    socket: &UdpSocket,
    usb_writer: &mut Option<OwnedWriteHalf>,
    usb_stream_active: &AtomicBool,
    state: &SharedNetworkState,
    audio_config: &Mutex<AudioConfig>,
    remote_config_tx: &mpsc::UnboundedSender<AudioConfig>,
    start_instant: &Instant,
    send_buf: &mut [u8],
) {
    match msg {
        ControlMessage::Handshake { device_name, .. } => {
            *state.peer_device_name.lock() = Some(device_name);
            let cfg = audio_config.lock().clone();
            let server_name =
                std::env::var("COMPUTERNAME").unwrap_or_else(|_| "AudioSync-PC".to_string());
            let ack = ControlMessage::HandshakeAck {
                server_name,
                pc_to_phone_enabled: cfg.pc_to_phone_enabled,
                phone_to_pc_enabled: cfg.phone_to_pc_enabled,
                opus_bitrate: cfg.opus_bitrate_bps,
            };
            let ts = (start_instant.elapsed().as_micros() & 0xFFFF_FFFF) as u32;
            if let Ok(pkt) = FramedPacket::control(0, ts, &ack) {
                let _ = send_packet_active(
                    &pkt,
                    src_addr,
                    socket,
                    usb_writer,
                    usb_stream_active,
                    state,
                    send_buf,
                )
                .await;
            }
            let sync_rev = state.current_revision().max(1);
            let sync_msg = state.build_sync_message(&cfg, sync_rev);
            if let Ok(pkt) = FramedPacket::control(0, ts, &sync_msg) {
                let _ = send_packet_active(
                    &pkt,
                    src_addr,
                    socket,
                    usb_writer,
                    usb_stream_active,
                    state,
                    send_buf,
                )
                .await;
            }
        }
        ControlMessage::MuteState { stream_id, muted } => {
            let updated_cfg = {
                let mut cfg = audio_config.lock();
                match stream_id {
                    StreamId::PcToPhone => cfg.pc_muted = muted,
                    StreamId::PhoneToPc => cfg.mic_muted = muted,
                    StreamId::Control => {}
                }
                cfg.clone()
            };
            let _ = remote_config_tx.send(updated_cfg);
        }
        ControlMessage::StreamConfig {
            opus_bitrate,
            frame_duration_ms,
            jitter_capacity,
            noise_suppression_enabled,
            gain_db,
        } => {
            let updated_cfg = {
                let mut cfg = audio_config.lock();
                cfg.opus_bitrate_bps = opus_bitrate.clamp(32_000, 128_000);
                cfg.frame_duration_ms = if frame_duration_ms == 20 { 20 } else { 10 };
                cfg.jitter_buffer_packets = jitter_capacity.clamp(1, 32);
                cfg.rnnoise_enabled = noise_suppression_enabled;
                cfg.mic_gain_db = gain_db.clamp(-12.0, 24.0);
                cfg.clone()
            };
            let _ = remote_config_tx.send(updated_cfg);
        }
        ControlMessage::SyncConfig {
            revision,
            pc_to_phone_enabled,
            phone_to_pc_enabled,
            pc_muted,
            mic_muted,
            push_to_talk_enabled,
            push_to_talk_active,
            aec_enabled,
            rnnoise_enabled,
            vad_gate_threshold,
            mic_gain_db,
            pc_volume,
            opus_bitrate_bps,
            frame_duration_ms,
            jitter_buffer_packets,
        } => {
            let current = state.config_revision.load(Ordering::SeqCst);
            if revision >= current {
                state.config_revision.store(revision, Ordering::SeqCst);
                let updated_cfg = {
                    let mut cfg = audio_config.lock();
                    cfg.pc_to_phone_enabled = pc_to_phone_enabled;
                    cfg.phone_to_pc_enabled = phone_to_pc_enabled;
                    cfg.pc_muted = pc_muted;
                    cfg.mic_muted = mic_muted;
                    cfg.push_to_talk_enabled = push_to_talk_enabled;
                    cfg.push_to_talk_active = push_to_talk_active;
                    cfg.aec_enabled = aec_enabled;
                    cfg.rnnoise_enabled = rnnoise_enabled;
                    cfg.vad_gate_threshold = vad_gate_threshold.clamp(0.0, 0.95);
                    cfg.mic_gain_db = mic_gain_db.clamp(-12.0, 24.0);
                    cfg.pc_volume = pc_volume.clamp(0.0, 2.0);
                    cfg.opus_bitrate_bps = opus_bitrate_bps.clamp(32_000, 128_000);
                    cfg.frame_duration_ms = if frame_duration_ms == 20 { 20 } else { 10 };
                    cfg.jitter_buffer_packets = jitter_buffer_packets.clamp(1, 32);
                    cfg.clone()
                };
                let _ = remote_config_tx.send(updated_cfg);
            }
        }
        ControlMessage::Disconnect { .. } => {
            state.connected.store(false, Ordering::Relaxed);
            *state.peer_addr.lock() = None;
        }
        ControlMessage::HandshakeAck { .. } => {}
    }
}
