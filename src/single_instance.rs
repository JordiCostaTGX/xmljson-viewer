use std::{
    env,
    io::{self, Read, Write},
    net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream},
    path::PathBuf,
    sync::mpsc,
    thread,
    time::Duration,
};

const MAGIC: &[u8; 8] = b"XMLJSON1";
const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;
const ACK: &[u8; 2] = b"OK";
const REJECTED: &[u8; 2] = b"NO";

pub enum Launch {
    /// The running window received the files; this process should exit.
    Forwarded,
    /// This window receives later open requests.
    Primary,
    /// Another window owns the listener; this one only shows its own files.
    Independent,
}

pub fn forward_or_start(
    files: Vec<PathBuf>,
    incoming: mpsc::Sender<Vec<PathBuf>>,
    new_window: bool,
) -> Result<Launch, String> {
    let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port());
    match TcpListener::bind(address) {
        Ok(listener) => {
            thread::Builder::new()
                .name("single-instance-ipc".into())
                .spawn(move || serve(listener, incoming))
                .map_err(|error| error.to_string())?;
            Ok(Launch::Primary)
        }
        Err(error) if error.kind() == io::ErrorKind::AddrInUse && new_window => {
            Ok(Launch::Independent)
        }
        Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
            let mut stream = TcpStream::connect_timeout(&address.into(), Duration::from_secs(2))
                .map_err(|error| format!("Could not contact the existing viewer: {error}"))?;
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .map_err(|error| error.to_string())?;
            write_request(&mut stream, &files).map_err(|error| error.to_string())?;
            let mut response = [0; 2];
            stream
                .read_exact(&mut response)
                .map_err(|error| error.to_string())?;
            if &response != ACK {
                return Err("The existing viewer rejected the open request".into());
            }
            Ok(Launch::Forwarded)
        }
        Err(error) => Err(format!(
            "Could not start the single-instance listener: {error}"
        )),
    }
}

fn port() -> u16 {
    let user = env::var("USERNAME")
        .or_else(|_| env::var("USER"))
        .unwrap_or_default();
    let hash = b"io.github.xmljsonviewer.Viewer"
        .iter()
        .chain(user.as_bytes())
        .fold(0x811c_9dc5u32, |hash, byte| {
            (hash ^ u32::from(*byte)).wrapping_mul(0x0100_0193)
        });
    49_152 + (hash % 16_384) as u16
}

fn serve(listener: TcpListener, incoming: mpsc::Sender<Vec<PathBuf>>) {
    for connection in listener.incoming() {
        let Ok(mut stream) = connection else {
            continue;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let response = match read_request(&mut stream) {
            Ok(files) => {
                if incoming.send(files).is_ok() {
                    ACK
                } else {
                    REJECTED
                }
            }
            Err(_) => REJECTED,
        };
        let _ = stream.write_all(response);
    }
}

fn write_request(writer: &mut impl Write, files: &[PathBuf]) -> io::Result<()> {
    let files: Vec<String> = files
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    let payload = serde_json::to_vec(&files)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if payload.len() > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Open request is too large",
        ));
    }
    writer.write_all(MAGIC)?;
    writer.write_all(&(payload.len() as u32).to_le_bytes())?;
    writer.write_all(&payload)
}

fn read_request(reader: &mut impl Read) -> io::Result<Vec<PathBuf>> {
    let mut magic = [0; 8];
    reader.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid local open request",
        ));
    }
    let mut length = [0; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    if length > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Open request is too large",
        ));
    }
    let mut payload = vec![0; length];
    reader.read_exact(&mut payload)?;
    let files: Vec<String> = serde_json::from_slice(&payload)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(files.into_iter().map(PathBuf::from).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrips_multiple_unicode_paths() {
        let paths = vec![
            PathBuf::from(r"C:\Documents\audit data.json"),
            PathBuf::from(r"C:\Documents\réservations.xml"),
        ];
        let mut encoded = Vec::new();
        write_request(&mut encoded, &paths).unwrap();
        assert_eq!(read_request(&mut encoded.as_slice()).unwrap(), paths);
    }
}
