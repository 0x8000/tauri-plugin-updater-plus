use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use updater_plus::{Error, Result, TransportError, TransportErrorKind};

pub(crate) struct MqttWireClient {
    stream: TcpStream,
    packet_id: u16,
}

impl MqttWireClient {
    pub(crate) async fn connect(addr: &str, client_id: &str) -> Result<Self> {
        let mut stream = TcpStream::connect(addr).await.map_err(|err| {
            Error::Transport(
                TransportError::new(
                    TransportErrorKind::Unavailable,
                    format!("MQTT connect to {addr} failed: {err}"),
                )
                .with_source(err),
            )
        })?;

        let mut body = Vec::new();
        body.extend_from_slice(&[0x00, 0x04, b'M', b'Q', b'T', b'T', 0x04, 0x02, 0x00, 0x3c]);
        let cid = client_id.as_bytes();
        body.extend_from_slice(&(cid.len() as u16).to_be_bytes());
        body.extend_from_slice(cid);

        let mut pkt = vec![0x10];
        encode_remaining_length(body.len(), &mut pkt);
        pkt.extend_from_slice(&body);
        stream.write_all(&pkt).await?;
        stream.flush().await?;

        let (pkt_type, _, connack) = read_mqtt_packet(&mut stream).await?;
        if pkt_type != 2 || connack.len() < 2 || connack[1] != 0 {
            return Err(Error::Transport(TransportError::new(
                TransportErrorKind::Unavailable,
                "MQTT broker rejected CONNECT",
            )));
        }
        Ok(Self {
            stream,
            packet_id: 1,
        })
    }

    fn next_pid(&mut self) -> u16 {
        let pid = self.packet_id;
        self.packet_id = (self.packet_id % 65000) + 1;
        pid
    }

    pub(crate) async fn subscribe(&mut self, topic: &str) -> Result<()> {
        let pid = self.next_pid();
        let mut body = Vec::new();
        body.extend_from_slice(&pid.to_be_bytes());
        let tb = topic.as_bytes();
        body.extend_from_slice(&(tb.len() as u16).to_be_bytes());
        body.extend_from_slice(tb);
        body.push(1);

        let mut pkt = vec![0x82];
        encode_remaining_length(body.len(), &mut pkt);
        pkt.extend_from_slice(&body);
        self.stream.write_all(&pkt).await?;
        self.stream.flush().await?;

        let (pkt_type, _, _) = read_mqtt_packet(&mut self.stream).await?;
        if pkt_type != 9 {
            return Err(Error::Transport(TransportError::new(
                TransportErrorKind::Other,
                format!("expected MQTT SUBACK (9), got {pkt_type}"),
            )));
        }
        Ok(())
    }

    pub(crate) async fn publish(&mut self, topic: &str, payload: &[u8]) -> Result<()> {
        let pid = self.next_pid();
        let mut body = Vec::with_capacity(topic.len() + payload.len() + 4);
        let tb = topic.as_bytes();
        body.extend_from_slice(&(tb.len() as u16).to_be_bytes());
        body.extend_from_slice(tb);
        body.extend_from_slice(&pid.to_be_bytes());
        body.extend_from_slice(payload);

        let mut pkt = vec![0x32];
        encode_remaining_length(body.len(), &mut pkt);
        pkt.extend_from_slice(&body);
        self.stream.write_all(&pkt).await?;
        self.stream.flush().await?;
        Ok(())
    }

    pub(crate) async fn next_publish(&mut self) -> Result<(String, Vec<u8>)> {
        loop {
            let (pkt_type, flags, body) = read_mqtt_packet(&mut self.stream).await?;
            match pkt_type {
                4 | 13 => continue,
                3 => {
                    if body.len() < 2 {
                        return Err(Error::Transport(TransportError::new(
                            TransportErrorKind::Other,
                            "malformed MQTT PUBLISH",
                        )));
                    }
                    let tlen = u16::from_be_bytes([body[0], body[1]]) as usize;
                    let mut pos = 2 + tlen;
                    if body.len() < pos {
                        return Err(Error::Transport(TransportError::new(
                            TransportErrorKind::Other,
                            "truncated MQTT topic",
                        )));
                    }
                    let topic = String::from_utf8_lossy(&body[2..pos]).into_owned();
                    let qos = (flags >> 1) & 0x03;
                    if qos == 1 {
                        if body.len() < pos + 2 {
                            return Err(Error::Transport(TransportError::new(
                                TransportErrorKind::Other,
                                "missing MQTT QoS 1 packet id",
                            )));
                        }
                        let pid = u16::from_be_bytes([body[pos], body[pos + 1]]);
                        pos += 2;
                        let mut puback = [0x40, 0x02, 0, 0];
                        puback[2..4].copy_from_slice(&pid.to_be_bytes());
                        self.stream.write_all(&puback).await?;
                        self.stream.flush().await?;
                    }
                    return Ok((topic, body[pos..].to_vec()));
                }
                other => {
                    return Err(Error::Transport(TransportError::new(
                        TransportErrorKind::Other,
                        format!("unexpected MQTT packet type {other}"),
                    )));
                }
            }
        }
    }
}

fn encode_remaining_length(mut len: usize, out: &mut Vec<u8>) {
    loop {
        let mut byte = (len % 128) as u8;
        len /= 128;
        if len > 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if len == 0 {
            break;
        }
    }
}

async fn read_mqtt_packet(stream: &mut TcpStream) -> Result<(u8, u8, Vec<u8>)> {
    let mut fixed = [0u8; 1];
    stream.read_exact(&mut fixed).await.map_err(|err| {
        Error::Transport(
            TransportError::new(TransportErrorKind::Unavailable, format!("MQTT read: {err}"))
                .with_source(err),
        )
    })?;
    let pkt_type = fixed[0] >> 4;
    let flags = fixed[0] & 0x0f;

    let mut multiplier = 1usize;
    let mut rem_len = 0usize;
    for _ in 0..4 {
        let mut b = [0u8; 1];
        stream.read_exact(&mut b).await?;
        rem_len += ((b[0] & 0x7f) as usize) * multiplier;
        if (b[0] & 0x80) == 0 {
            break;
        }
        multiplier *= 128;
    }

    let mut body = vec![0u8; rem_len];
    if rem_len > 0 {
        stream.read_exact(&mut body).await?;
    }
    Ok((pkt_type, flags, body))
}
