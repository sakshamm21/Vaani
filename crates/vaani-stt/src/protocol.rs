//! Messages between the app and `vaani-worker`, sent over the worker's stdin and stdout as
//! length-prefixed `postcard` frames: a little-endian u32 byte length, then the payload.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};
use vaani_models::ModelKind;

/// Frames bigger than this are refused (about 30 minutes of 16 kHz f32 audio).
const MAX_FRAME: u32 = 128 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    Load {
        kind: ModelKind,
        dir: String,
        threads: u32,
    },
    Transcribe {
        samples: Vec<f32>,
        language: String,
    },
    Shutdown,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    Loaded { ms: u64 },
    Text { text: String, ms: u64 },
    Error(String),
}

pub fn write<T: Serialize>(w: &mut impl Write, msg: &T) -> Result<(), crate::SttError> {
    let bytes = postcard::to_stdvec(msg)?;
    let len = u32::try_from(bytes.len())
        .ok()
        .filter(|l| *l <= MAX_FRAME)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "frame too large"))?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(&bytes)?;
    w.flush()?;
    Ok(())
}

pub fn read<T: for<'de> Deserialize<'de>>(r: &mut impl Read) -> Result<T, crate::SttError> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len);
    if len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large").into());
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf)?;
    Ok(postcard::from_bytes(&buf)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut buf = Vec::new();
        write(
            &mut buf,
            &Request::Transcribe {
                samples: vec![0.5, -0.25],
                language: "en".into(),
            },
        )
        .unwrap();
        write(
            &mut buf,
            &Response::Text {
                text: "hi".into(),
                ms: 3,
            },
        )
        .unwrap();
        let mut r = &buf[..];
        match read::<Request>(&mut r).unwrap() {
            Request::Transcribe { samples, language } => {
                assert_eq!(samples, vec![0.5, -0.25]);
                assert_eq!(language, "en");
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            read::<Response>(&mut r).unwrap(),
            Response::Text { ms: 3, .. }
        ));
        assert!(read::<Response>(&mut r).is_err());
    }
}
