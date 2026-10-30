//! The Wayland wire format: a header (object, size and opcode), then the
//! arguments in 32-bit words, in the host's byte order. File descriptors
//! travel beside the bytes, in order.

use crate::protocol::Arg;

pub const HEADER: usize = 8;

fn word(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(b.get(at..at + 4)?.try_into().unwrap()))
}

/// The object, opcode and total size of the message at the start of `b`,
/// if its header is there.
pub fn header(b: &[u8]) -> Option<(u32, u16, usize)> {
    let object = word(b, 0)?;
    let w = word(b, 4)?;
    Some((object, (w & 0xffff) as u16, (w >> 16) as usize))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Int(i32),
    Uint(u32),
    Fixed(i32),
    String(Option<Vec<u8>>),
    Object(u32),
    NewId(u32),
    /// wl_registry.bind's new_id: interface, version, id.
    TypedNewId(Vec<u8>, u32, u32),
    Array(Vec<u8>),
    Fd,
}

/// An argument and where its (last) word is in the message, for rewriting.
#[derive(Debug, Clone)]
pub struct Parsed {
    pub value: Value,
    pub offset: usize,
}

/// The arguments of `msg` (a whole message, header included).
pub fn parse(msg: &[u8], args: &[Arg]) -> Result<Vec<Parsed>, String> {
    let mut at = HEADER;
    let mut out = Vec::with_capacity(args.len());
    let blob = |at: &mut usize| -> Result<Vec<u8>, String> {
        let len = word(msg, *at).ok_or("message too short")? as usize;
        let start = *at + 4;
        let padded = len.checked_add(3).ok_or("bad length")? & !3;
        let end = start.checked_add(padded).ok_or("bad length")?;
        if end > msg.len() {
            return Err("message too short".into());
        }
        *at = end;
        Ok(msg[start..start + len].to_vec())
    };
    let string = |at: &mut usize| -> Result<Option<Vec<u8>>, String> {
        let b = blob(at)?;
        if b.is_empty() {
            return Ok(None);
        }
        match b.split_last() {
            Some((0, s)) => Ok(Some(s.to_vec())),
            _ => Err("string without its NUL".into()),
        }
    };
    for arg in args {
        let offset = at;
        let mut next = || -> Result<u32, String> {
            let w = word(msg, at).ok_or("message too short")?;
            at += 4;
            Ok(w)
        };
        let value = match arg {
            Arg::Int => Value::Int(next()? as i32),
            Arg::Uint => Value::Uint(next()?),
            Arg::Fixed => Value::Fixed(next()? as i32),
            Arg::Object => Value::Object(next()?),
            Arg::NewId(Some(_)) => Value::NewId(next()?),
            Arg::NewId(None) => {
                let iface = string(&mut at)?.ok_or("bind without an interface")?;
                let version = word(msg, at).ok_or("message too short")?;
                let id = word(msg, at + 4).ok_or("message too short")?;
                at += 8;
                out.push(Parsed { value: Value::TypedNewId(iface, version, id), offset: at - 4 });
                continue;
            }
            Arg::String => Value::String(string(&mut at)?),
            Arg::Array => Value::Array(blob(&mut at)?),
            Arg::Fd => Value::Fd,
        };
        out.push(Parsed { value, offset });
    }
    if at != msg.len() {
        return Err(format!("{} bytes left over", msg.len() - at));
    }
    Ok(out)
}

/// Builds messages, for tests and for the ones the proxy makes up.
#[derive(Default)]
pub struct Builder {
    buf: Vec<u8>,
}

impl Builder {
    pub fn new(object: u32, opcode: u16) -> Self {
        let mut b = Builder { buf: Vec::new() };
        b.buf.extend_from_slice(&object.to_ne_bytes());
        b.buf.extend_from_slice(&(opcode as u32).to_ne_bytes());
        b
    }

    pub fn uint(mut self, v: u32) -> Self {
        self.buf.extend_from_slice(&v.to_ne_bytes());
        self
    }

    pub fn int(self, v: i32) -> Self {
        self.uint(v as u32)
    }

    pub fn string(mut self, s: &[u8]) -> Self {
        let len = s.len() + 1;
        self.buf.extend_from_slice(&(len as u32).to_ne_bytes());
        self.buf.extend_from_slice(s);
        self.buf.push(0);
        while !self.buf.len().is_multiple_of(4) {
            self.buf.push(0);
        }
        self
    }

    pub fn build(mut self) -> Vec<u8> {
        let size = self.buf.len() as u32;
        let w = u32::from_ne_bytes(self.buf[4..8].try_into().unwrap());
        self.buf[4..8].copy_from_slice(&(size << 16 | w).to_ne_bytes());
        self.buf
    }
}

pub fn set_word(msg: &mut [u8], offset: usize, v: u32) {
    msg[offset..offset + 4].copy_from_slice(&v.to_ne_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let m = Builder::new(2, 0).uint(7).string(b"wl_seat").uint(5).uint(9).build();
        assert_eq!(header(&m), Some((2, 0, m.len())));
        let args = parse(&m, &[Arg::Uint, Arg::NewId(None)]).unwrap();
        assert_eq!(args[0].value, Value::Uint(7));
        assert_eq!(args[1].value, Value::TypedNewId(b"wl_seat".to_vec(), 5, 9));
        assert_eq!(word(&m, args[1].offset), Some(9));
    }

    #[test]
    fn null_string_and_padding() {
        let m = Builder::new(3, 1).uint(0).string(b"abc").build();
        let args = parse(&m, &[Arg::String, Arg::String]).unwrap();
        assert_eq!(args[0].value, Value::String(None));
        assert_eq!(args[1].value, Value::String(Some(b"abc".to_vec())));
    }

    #[test]
    fn rejects_bad_lengths() {
        let m = Builder::new(3, 1).uint(100).build();
        assert!(parse(&m, &[Arg::String]).is_err());
        let m = Builder::new(3, 1).uint(1).uint(2).build();
        assert!(parse(&m, &[Arg::Uint]).is_err());
    }
}
