use chrono::offset::Utc;
use futures::future;

// See: https://tls.ulfheim.net
use rustls::internal::msgs::codec::{Codec, Reader};
use rustls::internal::msgs::enums::{ContentType, ProtocolVersion};
use rustls::internal::msgs::handshake::{
    HandshakeMessagePayload, HandshakePayload, ServerNamePayload,
};

use std::error::Error;
use std::net::ToSocketAddrs;

use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};

const TLS_RECORD_HEADER_LENGTH: usize = 5;
const TLS_HANDSHAKE_MAX_LENGTH: usize = 2048;

async fn peek(stream: &mut TcpStream, size: usize) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut buf = vec![0; size];
    let n = stream.peek(&mut buf).await?;

    if n == size {
        Ok(buf)
    } else {
        Err(format!("Peek size mismatch: {} != {}", n, size).into())
    }
}

async fn splice(inbound: TcpStream, outbound: TcpStream) -> Result<(), Box<dyn Error>> {
    let (mut ri, mut wi) = inbound.split();
    let (mut ro, mut wo) = outbound.split();

    // TODO: use splice(2) syscall
    let client_to_server = ri.copy(&mut wo);
    let server_to_client = ro.copy(&mut wi);

    future::try_join(client_to_server, server_to_client).await?;

    Ok(())
}

fn as_str<T: AsRef<str>>(s: T) -> String {
    format!("{}", s.as_ref())
}

async fn process(mut inbound: TcpStream) -> Result<(), Box<dyn Error>> {
    let buf = peek(&mut inbound, TLS_RECORD_HEADER_LENGTH).await?;
    let mut rd = Reader::init(&buf);

    let content_type = ContentType::read(&mut rd).unwrap();
    let protocol_version = ProtocolVersion::read(&mut rd).unwrap();
    let handshake_size = usize::from(u16::read(&mut rd).unwrap());

    if content_type != ContentType::Handshake {
        return Err("TLS message is not a handshake".into());
    }

    if handshake_size > TLS_HANDSHAKE_MAX_LENGTH {
        return Err(format!(
            "TLS handshake size is {}, expected {} max",
            handshake_size, TLS_HANDSHAKE_MAX_LENGTH
        )
        .into());
    }

    let buf = peek(&mut inbound, TLS_RECORD_HEADER_LENGTH + handshake_size).await?;
    let mut rd = Reader::init(&buf);
    rd.take(TLS_RECORD_HEADER_LENGTH);

    let handshake = HandshakeMessagePayload::read_version(&mut rd, protocol_version).unwrap();

    let client_hello = match handshake.payload {
        HandshakePayload::ClientHello(x) => x,
        _ => {
            return Err("TLS handshake is not Client Hello".into());
        }
    };

    let sni = match client_hello.get_sni_extension() {
        Some(x) => x,
        None => {
            return Err("Missing SNI".into());
        }
    };

    let host = match &sni[0].payload {
        ServerNamePayload::HostName(x) => x,
        ServerNamePayload::Unknown(_) => {
            return Err("Unknown SNI payload type".into());
        }
    };

    let host_str = as_str(host);

    if !host_str.ends_with("holohost.net") {
        return Err(format!("Rejected {}", host_str).into());
    }

    let addr = match format!("{}:443", host_str).to_socket_addrs() {
        Ok(mut addrs) => addrs.next().unwrap(),
        Err(_) => {
            return Err(format!("Failed to resolve {}", host_str).into());
        }
    };

    let outbound = TcpStream::connect(&addr).await?;
    splice(inbound, outbound).await
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut listener = TcpListener::bind("0.0.0.0:443").await?;

    loop {
        let (inbound, inbound_addr) = listener.accept().await?;
        tokio::spawn(async move {
            if let Err(e) = process(inbound).await {
                println!("{} {}: {}", Utc::now().naive_utc(), inbound_addr.ip(), e);
            }
        });
    }
}
