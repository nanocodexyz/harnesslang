use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};

use crate::run::blob::base64_encode;

pub const WEBSOCKET_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
pub const HANDSHAKE_LINES_MAX: usize = 64;
pub const HANDSHAKE_LINE_BYTES_MAX: u64 = 8192;
const KEY_BYTES: usize = 16;
const MASK_BYTES: usize = 4;
const OPCODE_CONTINUATION: u8 = 0x0;
const OPCODE_TEXT: u8 = 0x1;
const OPCODE_BINARY: u8 = 0x2;
const OPCODE_CLOSE: u8 = 0x8;
const OPCODE_PING: u8 = 0x9;
const OPCODE_PONG: u8 = 0xa;
const FIN_BIT: u8 = 0x80;
const MASK_BIT: u8 = 0x80;
const OPCODE_BITS: u8 = 0x0f;
const LENGTH_BITS: u8 = 0x7f;
const LENGTH_16: u8 = 126;
const LENGTH_64: u8 = 127;
const SWITCHING_PROTOCOLS: &str = "101";

pub struct Address {
    pub secure: bool,
    pub host: String,
    pub port: u16,
    pub path: String,
}

const WSS_PORT: u16 = 443;
const WS_PORT: u16 = 80;

pub fn parse(url: &str) -> Result<Address, String> {
    let (secure, rest) = if let Some(rest) = url.strip_prefix("wss://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("ws://") {
        (false, rest)
    } else {
        return Err(format!("tail url {url} is not ws:// or wss://"));
    };
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (
            host,
            port.parse()
                .map_err(|_| format!("tail url {url} has an invalid port"))?,
        ),
        None => (authority, if secure { WSS_PORT } else { WS_PORT }),
    };
    if host.is_empty() {
        return Err(format!("tail url {url} has no host"));
    }
    Ok(Address {
        secure,
        host: host.to_owned(),
        port,
        path: path.to_owned(),
    })
}

pub struct Socket {
    reader: BufReader<Box<dyn Read>>,
    writer: Box<dyn Write>,
    child: Option<Child>,
    random: std::fs::File,
}

pub enum Message {
    Text(String),
    Binary(Vec<u8>),
    Closed,
}

pub fn connect(
    address: &Address,
    protocol: &str,
    certificate: Option<&str>,
    user_agent: &str,
) -> Result<Socket, String> {
    let random = std::fs::File::open("/dev/urandom")
        .map_err(|error| format!("cannot open /dev/urandom: {error}"))?;
    let (reader, writer, child): (Box<dyn Read>, Box<dyn Write>, Option<Child>) = if address.secure
    {
        let mut child = tls_pipe(address, certificate)?;
        let reader = Box::new(child.stdout.take().expect("stdout is piped"));
        let writer = Box::new(child.stdin.take().expect("stdin is piped"));
        (reader, writer, Some(child))
    } else {
        let stream = TcpStream::connect((address.host.as_str(), address.port))
            .map_err(|error| format!("cannot connect to {}:{}: {error}", address.host, address.port))?;
        let writer = stream
            .try_clone()
            .map_err(|error| format!("cannot share the tail socket: {error}"))?;
        (Box::new(stream), Box::new(writer), None)
    };
    let mut socket = Socket {
        reader: BufReader::new(reader),
        writer,
        child,
        random,
    };
    socket.handshake(address, protocol, user_agent)?;
    Ok(socket)
}

fn tls_pipe(address: &Address, certificate: Option<&str>) -> Result<Child, String> {
    let mut command = Command::new("openssl");
    command.args([
        "s_client",
        "-quiet",
        "-verify_return_error",
        "-connect",
        &format!("{}:{}", address.host, address.port),
        "-servername",
        &address.host,
        "-verify_hostname",
        &address.host,
    ]);
    if address.host.parse::<std::net::IpAddr>().is_ok() {
        command.args(["-verify_ip", &address.host]);
    }
    if let Some(certificate) = certificate {
        command.args(["-CAfile", certificate]);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("cannot run openssl s_client for the tail: {error}"))
}

impl Socket {
    fn handshake(&mut self, address: &Address, protocol: &str, user_agent: &str) -> Result<(), String> {
        let mut key = [0_u8; KEY_BYTES];
        self.random
            .read_exact(&mut key)
            .map_err(|error| format!("cannot read /dev/urandom: {error}"))?;
        let key = base64_encode(&key);
        let request = format!(
            "GET {} HTTP/1.1\r\nHost: {}:{}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Protocol: {protocol}\r\nUser-Agent: {user_agent}\r\n\r\n",
            address.path, address.host, address.port
        );
        self.writer
            .write_all(request.as_bytes())
            .and_then(|()| self.writer.flush())
            .map_err(|error| format!("cannot send the tail handshake: {error}"))?;
        let expected = base64_encode(&super::sha1::sha1(format!("{key}{WEBSOCKET_GUID}").as_bytes()));
        let mut status = String::new();
        let mut accepted = false;
        let mut chosen = false;
        for index in 0..HANDSHAKE_LINES_MAX {
            let mut line = String::new();
            let read = (&mut self.reader)
                .take(HANDSHAKE_LINE_BYTES_MAX)
                .read_line(&mut line)
                .map_err(|error| format!("cannot read the tail handshake: {error}"))?;
            if read == 0 {
                return Err(format!("the tail server closed during the handshake after: {status}"));
            }
            let line = line.trim_end();
            if index == 0 {
                status = line.to_owned();
                continue;
            }
            if line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                let value = value.trim();
                accepted |= name.eq_ignore_ascii_case("sec-websocket-accept") && value == expected;
                chosen |= name.eq_ignore_ascii_case("sec-websocket-protocol") && value == protocol;
            }
        }
        if status.split_whitespace().nth(1) != Some(SWITCHING_PROTOCOLS) {
            return Err(format!("the tail server refused the upgrade: {status}"));
        }
        if !accepted {
            return Err("the tail server answered a wrong Sec-WebSocket-Accept".to_owned());
        }
        if !chosen {
            return Err(format!("the tail server did not choose protocol {protocol}"));
        }
        Ok(())
    }

    fn send_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<(), String> {
        let mut frame = vec![FIN_BIT | opcode];
        let length = payload.len();
        if length < usize::from(LENGTH_16) {
            frame.push(MASK_BIT | u8::try_from(length).expect("below 126"));
        } else if let Ok(short) = u16::try_from(length) {
            frame.push(MASK_BIT | LENGTH_16);
            frame.extend_from_slice(&short.to_be_bytes());
        } else {
            frame.push(MASK_BIT | LENGTH_64);
            frame.extend_from_slice(&u64::try_from(length).expect("fits u64").to_be_bytes());
        }
        let mut mask = [0_u8; MASK_BYTES];
        self.random
            .read_exact(&mut mask)
            .map_err(|error| format!("cannot read /dev/urandom: {error}"))?;
        frame.extend_from_slice(&mask);
        frame.extend(payload.iter().enumerate().map(|(index, byte)| byte ^ mask[index % MASK_BYTES]));
        self.writer
            .write_all(&frame)
            .and_then(|()| self.writer.flush())
            .map_err(|error| format!("cannot write to the tail: {error}"))
    }

    pub fn send_text(&mut self, text: &str) -> Result<(), String> {
        self.send_frame(OPCODE_TEXT, text.as_bytes())
    }

    pub fn close(&mut self) -> Result<(), String> {
        let sent = self.send_frame(OPCODE_CLOSE, &[]);
        if let Some(child) = &mut self.child {
            child.kill().map_err(|error| format!("cannot stop openssl: {error}"))?;
            child.wait().map_err(|error| format!("cannot wait for openssl: {error}"))?;
        }
        self.child = None;
        sent.or(Ok(()))
    }

    fn read_exact(&mut self, bytes: &mut [u8]) -> Result<bool, String> {
        match self.reader.read_exact(bytes) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
            Err(error) => Err(format!("cannot read from the tail: {error}")),
        }
    }

    fn frame(&mut self, message_bytes_max: u64) -> Result<Option<(bool, u8, Vec<u8>)>, String> {
        let mut head = [0_u8; 2];
        if !self.read_exact(&mut head)? {
            return Ok(None);
        }
        let length = match head[1] & LENGTH_BITS {
            LENGTH_16 => {
                let mut bytes = [0_u8; 2];
                if !self.read_exact(&mut bytes)? {
                    return Ok(None);
                }
                u64::from(u16::from_be_bytes(bytes))
            }
            LENGTH_64 => {
                let mut bytes = [0_u8; 8];
                if !self.read_exact(&mut bytes)? {
                    return Ok(None);
                }
                u64::from_be_bytes(bytes)
            }
            short => u64::from(short),
        };
        if length > message_bytes_max {
            return Err(format!(
                "a tail frame of {length} bytes is over tail_message_bytes_max ({message_bytes_max})"
            ));
        }
        let mut mask = [0_u8; MASK_BYTES];
        let masked = head[1] & MASK_BIT != 0;
        if masked && !self.read_exact(&mut mask)? {
            return Ok(None);
        }
        let mut payload = vec![0_u8; usize::try_from(length).expect("bounded below the maximum")];
        if !self.read_exact(&mut payload)? {
            return Ok(None);
        }
        if masked {
            for (index, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[index % MASK_BYTES];
            }
        }
        Ok(Some((head[0] & FIN_BIT != 0, head[0] & OPCODE_BITS, payload)))
    }

    pub fn next_message(&mut self, message_bytes_max: u64, frames_max: u64) -> Result<Message, String> {
        let mut assembled: Option<(u8, Vec<u8>)> = None;
        for _ in 0..frames_max {
            let Some((fin, opcode, payload)) = self.frame(message_bytes_max)? else {
                return Ok(Message::Closed);
            };
            match opcode {
                OPCODE_PING => self.send_frame(OPCODE_PONG, &payload)?,
                OPCODE_PONG => {}
                OPCODE_CLOSE => return Ok(Message::Closed),
                OPCODE_TEXT | OPCODE_BINARY | OPCODE_CONTINUATION => {
                    let (kind, mut bytes) = match (assembled.take(), opcode) {
                        (None, OPCODE_CONTINUATION) => {
                            return Err("the tail sent a continuation with no message open".to_owned());
                        }
                        (None, kind) => (kind, Vec::new()),
                        (Some(open), OPCODE_CONTINUATION) => open,
                        (Some(_), _) => {
                            return Err("the tail started a message inside another".to_owned());
                        }
                    };
                    bytes.extend_from_slice(&payload);
                    if u64::try_from(bytes.len()).expect("fits u64") > message_bytes_max {
                        return Err(format!(
                            "a tail message is over tail_message_bytes_max ({message_bytes_max})"
                        ));
                    }
                    if !fin {
                        assembled = Some((kind, bytes));
                        continue;
                    }
                    return Ok(if kind == OPCODE_TEXT {
                        Message::Text(
                            String::from_utf8(bytes)
                                .map_err(|_| "the tail sent a text message that is not UTF-8".to_owned())?,
                        )
                    } else {
                        Message::Binary(bytes)
                    });
                }
                other => return Err(format!("the tail sent unknown opcode {other}")),
            }
        }
        Err(format!("the tail sent more than {frames_max} frames without a whole message"))
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            drop(child.kill());
            drop(child.wait());
        }
    }
}
