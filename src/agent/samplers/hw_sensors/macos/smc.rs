//! Reads keys from the System Management Controller through the `AppleSMC`
//! user client, which any process can open.
//!
//! The protocol is not in the SDK. Every request and reply is one 80-byte
//! `Message` sent to struct method 2; its `command` byte selects key info (9)
//! or a read (5). The layout matches the `SMCKeyData_t` used by `smcFanControl`
//! and other open-source SMC tools. A key is four ASCII bytes packed
//! big-endian into a `u32`. A key's type is also a four-character code: this
//! module reads `flt ` (a little-endian `f32`) and `ui8 ` keys.

use crate::agent::samplers::iokit::{Service, UserClient};

/// `KERNEL_INDEX_SMC`: the struct method every SMC request goes through.
const SELECTOR: u32 = 2;
const READ_BYTES: u8 = 5;
const READ_KEY_INFO: u8 = 9;

const TYPE_FLT: u32 = fourcc(*b"flt ");
const TYPE_UI8: u32 = fourcc(*b"ui8 ");

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Version {
    major: u8,
    minor: u8,
    build: u8,
    reserved: u8,
    release: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PowerLimit {
    version: u16,
    length: u16,
    cpu: u32,
    gpu: u32,
    memory: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct KeyInfo {
    size: u32,
    kind: u32,
    attributes: u8,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Message {
    key: u32,
    version: Version,
    power_limit: PowerLimit,
    info: KeyInfo,
    result: u8,
    status: u8,
    command: u8,
    data32: u32,
    bytes: [u8; 32],
}

const _: () = assert!(std::mem::size_of::<Message>() == 80);

/// Pack a four-character code, such as an SMC key or type, big-endian.
pub(crate) const fn fourcc(code: [u8; 4]) -> u32 {
    u32::from_be_bytes(code)
}

/// The four characters of `code`, if they are printable ASCII.
pub(crate) fn fourcc_str(code: u32) -> Option<String> {
    let bytes = code.to_be_bytes();
    bytes
        .iter()
        .all(|b| b.is_ascii_graphic() || *b == b' ')
        .then(|| String::from_utf8_lossy(&bytes).into_owned())
}

/// A key `float_key` found to hold a `flt `, with the info a read needs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FloatKey {
    key: u32,
    info: KeyInfo,
}

/// The `f32` in a `flt ` key's reply bytes.
fn decode_flt(bytes: &[u8; 32]) -> f32 {
    f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// An open connection to `AppleSMC`.
pub(crate) struct Smc(UserClient);

impl Smc {
    pub(crate) fn open() -> Option<Self> {
        let service = Service::matching("AppleSMC")?.into_iter().next()?;
        service.open(0).map(Smc)
    }

    fn call(&self, input: &Message) -> Option<Message> {
        let mut output = Message::default();
        // SAFETY: `Message` is the 80-byte layout method 2 takes and returns,
        // and it holds only integers, so any reply bytes are a valid value.
        let ok = unsafe { self.0.call_struct(SELECTOR, input, &mut output) };
        (ok && output.result == 0).then_some(output)
    }

    fn key_info(&self, key: u32) -> Option<KeyInfo> {
        let input = Message {
            key,
            command: READ_KEY_INFO,
            ..Default::default()
        };
        self.call(&input).map(|m| m.info)
    }

    fn read(&self, key: u32, info: KeyInfo) -> Option<[u8; 32]> {
        let input = Message {
            key,
            info,
            command: READ_BYTES,
            ..Default::default()
        };
        self.call(&input).map(|m| m.bytes)
    }

    /// `key` as a readable `flt `, or `None` if the SMC has no such key or it
    /// has another type.
    pub(crate) fn float_key(&self, key: &str) -> Option<FloatKey> {
        let key = fourcc(key.as_bytes().try_into().ok()?);
        let info = self.key_info(key)?;
        (info.kind == TYPE_FLT && info.size == 4).then_some(FloatKey { key, info })
    }

    pub(crate) fn read_float(&self, key: FloatKey) -> Option<f32> {
        self.read(key.key, key.info).map(|b| decode_flt(&b))
    }

    /// The value of the `ui8 ` key `key`.
    pub(crate) fn read_u8(&self, key: &str) -> Option<u8> {
        let key = fourcc(key.as_bytes().try_into().ok()?);
        let info = self.key_info(key)?;
        if info.kind != TYPE_UI8 || info.size != 1 {
            return None;
        }
        self.read(key, info).map(|b| b[0])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fourcc_packs_big_endian_and_round_trips() {
        assert_eq!(fourcc(*b"TP1b"), 1414541666);
        assert_eq!(fourcc_str(1414541666).as_deref(), Some("TP1b"));
        assert_eq!(fourcc_str(fourcc(*b"flt ")).as_deref(), Some("flt "));
        assert_eq!(fourcc_str(5), None);
    }

    /// Reply bytes captured from `F0Ac` and `VP0b` on an M4 Max.
    #[test]
    fn flt_is_a_little_endian_f32() {
        let mut bytes = [0u8; 32];
        bytes[..4].copy_from_slice(&[0x0d, 0x3e, 0xaa, 0x44]);
        assert!((decode_flt(&bytes) - 1361.94).abs() < 0.01);
        bytes[..4].copy_from_slice(&0.97699f32.to_le_bytes());
        assert!((decode_flt(&bytes) - 0.97699).abs() < 1e-6);
    }

    /// Opens the SMC on this Mac and reads the fan count and a float key.
    #[test]
    fn reads_this_macs_smc() {
        let Some(smc) = Smc::open() else {
            eprintln!("no AppleSMC on this host; skipping");
            return;
        };
        let fans = smc.read_u8("FNum");
        println!("FNum = {fans:?}");
        assert!(smc.float_key("FNum").is_none(), "FNum is a ui8, not a flt");
        assert!(smc.float_key("Zz9~").is_none());
        if let Some(n) = fans.filter(|n| *n > 0) {
            let key = smc.float_key("F0Ac").expect("fan 0 speed is a flt");
            let rpm = smc.read_float(key).expect("read F0Ac");
            println!("{n} fans, F0Ac = {rpm} RPM");
            assert!((0.0..20_000.0).contains(&rpm));
        }
    }
}
