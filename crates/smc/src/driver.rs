//! Raw SMC transport.
//!
//! Holds the `AppleSMC` `IOConnectCallStructMethod` struct layout, the
//! [`Driver`] trait, and the IOKit implementation. Every `unsafe` block in
//! this crate lives in this file.

use std::ffi::{c_char, c_void};

use crate::SmcError;

/// Selector for the `AppleSMC` user client struct method.
const KERNEL_INDEX_SMC: u32 = 2;

/// `data8` command: fill `key_info` for a key. A nonzero `data_size` in the
/// reply means the key exists.
pub const CMD_READ_KEY_INFO: u8 = 9;
/// `data8` command: read `key_info.data_size` bytes into `bytes`.
pub const CMD_READ_BYTES: u8 = 5;
/// `data8` command: write `key_info.data_size` bytes from `bytes`.
pub const CMD_WRITE_BYTES: u8 = 6;

/// SMC result code for "this machine does not have that key".
pub const SMC_KEY_NOT_FOUND: u8 = 0x84;

/// `kIOReturnNotPrivileged` — the write needs root.
const K_IO_RETURN_NOT_PRIVILEGED: i32 = 0xe000_02c1_u32 as i32;

/// Largest payload one struct call can carry.
pub const MAX_DATA_LEN: usize = 32;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Vers {
    pub major: u8,
    pub minor: u8,
    pub build: u8,
    pub reserved: u8,
    pub release: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PLimit {
    pub version: u16,
    pub length: u16,
    pub cpu: u32,
    pub gpu: u32,
    pub mem: u32,
}

/// Size and type the SMC reports for one key.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyInfo {
    /// Payload length in bytes. Zero means the key is absent.
    pub data_size: u32,
    /// Type FourCC, for example `ui8 ` or `ui32`.
    pub data_type: u32,
    pub data_attributes: u8,
}

/// The 80-byte struct the `AppleSMC` user client takes and returns.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SmcKeyData {
    /// Key FourCC, big-endian: `b"CH0B"` becomes `u32::from_be_bytes`.
    pub key: u32,
    pub vers: Vers,
    pub p_limit: PLimit,
    pub key_info: KeyInfo,
    pub result: u8,
    pub status: u8,
    /// The command. See the `CMD_*` constants.
    pub data8: u8,
    pub data32: u32,
    pub bytes: [u8; MAX_DATA_LEN],
}

/// Encodes a 4-character key as a big-endian FourCC.
pub fn fourcc(key: &str) -> Result<u32, SmcError> {
    let bytes = key.as_bytes();
    if bytes.len() != 4 || !key.is_ascii() {
        return Err(SmcError::InvalidKey(key.to_string()));
    }
    Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Decodes a big-endian FourCC back to a 4-character key.
pub fn fourcc_str(key: u32) -> String {
    String::from_utf8_lossy(&key.to_be_bytes()).into_owned()
}

/// Turns a nonzero `result` field into a typed error.
///
/// `requested` is the key the caller asked for. The SMC leaves the key
/// field of the reply at zero when it refuses a call, so without the
/// fallback the message read "SMC key  not found on this Mac".
pub fn check_result(requested: u32, output: &SmcKeyData) -> Result<(), SmcError> {
    let key = fourcc_str(if output.key == 0 {
        requested
    } else {
        output.key
    });
    match output.result {
        0 => Ok(()),
        SMC_KEY_NOT_FOUND => Err(SmcError::KeyNotFound(key)),
        result => Err(SmcError::Smc { key, result }),
    }
}

/// One round trip to the SMC.
pub trait Driver {
    /// Sends `input` and returns the reply, or a typed error.
    fn call(&mut self, input: SmcKeyData) -> Result<SmcKeyData, SmcError>;
}

// ---------------------------------------------------------------------------
// IOKit bindings
// ---------------------------------------------------------------------------

type IoReturn = i32;
type MachPort = u32;
type IoObject = u32;
type IoConnect = u32;
type IoIterator = u32;
type CfDictionaryRef = *const c_void;

const MACH_PORT_NULL: MachPort = 0;
const IO_OBJECT_NULL: IoObject = 0;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOMainPort(bootstrap_port: MachPort, main_port: *mut MachPort) -> IoReturn;
    fn IOServiceMatching(name: *const c_char) -> CfDictionaryRef;
    fn IOServiceGetMatchingServices(
        main_port: MachPort,
        matching: CfDictionaryRef,
        existing: *mut IoIterator,
    ) -> IoReturn;
    fn IOIteratorNext(iterator: IoIterator) -> IoObject;
    fn IOServiceOpen(
        service: IoObject,
        owning_task: MachPort,
        kind: u32,
        connect: *mut IoConnect,
    ) -> IoReturn;
    fn IOServiceClose(connect: IoConnect) -> IoReturn;
    fn IOObjectRelease(object: IoObject) -> IoReturn;
    fn IOConnectCallStructMethod(
        connection: IoConnect,
        selector: u32,
        input: *const c_void,
        input_cnt: usize,
        output: *mut c_void,
        output_cnt: *mut usize,
    ) -> IoReturn;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    /// The current task port. `mach_task_self()` is a macro over this symbol.
    static mach_task_self_: MachPort;
}

/// A live connection to the `AppleSMC` IOKit service.
#[derive(Debug)]
pub struct IoKitDriver {
    conn: IoConnect,
}

impl IoKitDriver {
    /// Opens the `AppleSMC` service. Reads work unprivileged; writes need root.
    pub fn open() -> Result<Self, SmcError> {
        let mut port: MachPort = MACH_PORT_NULL;
        // SAFETY: `port` is a valid, initialized out-parameter of the right type.
        let rc = unsafe { IOMainPort(MACH_PORT_NULL, &mut port) };
        if rc != 0 {
            return Err(SmcError::Open(rc));
        }

        // SAFETY: the name is a NUL-terminated C string literal that IOKit
        // only reads. The returned dictionary is consumed by
        // `IOServiceGetMatchingServices` below, which releases it.
        let matching = unsafe { IOServiceMatching(c"AppleSMC".as_ptr()) };
        if matching.is_null() {
            return Err(SmcError::ServiceNotFound);
        }

        let mut iter: IoIterator = IO_OBJECT_NULL;
        // SAFETY: `port` is an open main port, `matching` a non-null dictionary
        // this call takes ownership of, and `iter` a valid out-parameter.
        let rc = unsafe { IOServiceGetMatchingServices(port, matching, &mut iter) };
        if rc != 0 {
            return Err(SmcError::Open(rc));
        }

        // SAFETY: `iter` is a valid iterator returned by the call above.
        let device = unsafe { IOIteratorNext(iter) };
        // SAFETY: `iter` is still valid and is not used again after release.
        unsafe { IOObjectRelease(iter) };
        if device == IO_OBJECT_NULL {
            return Err(SmcError::ServiceNotFound);
        }

        let mut conn: IoConnect = 0;
        // SAFETY: `device` is a valid service object, `mach_task_self_` the
        // current task port, and `conn` a valid out-parameter.
        let rc = unsafe { IOServiceOpen(device, mach_task_self_, 0, &mut conn) };
        // SAFETY: the connection holds its own reference, so the service
        // object is released here whether or not the open succeeded.
        unsafe { IOObjectRelease(device) };
        if rc != 0 {
            return Err(SmcError::Open(rc));
        }

        Ok(Self { conn })
    }
}

impl Driver for IoKitDriver {
    fn call(&mut self, input: SmcKeyData) -> Result<SmcKeyData, SmcError> {
        let mut output = SmcKeyData::default();
        let mut output_len = std::mem::size_of::<SmcKeyData>();
        // SAFETY: `self.conn` is an open user client. Both pointers refer to
        // live, correctly aligned 80-byte `SmcKeyData` values, and the
        // lengths passed match `size_of::<SmcKeyData>()`.
        let rc = unsafe {
            IOConnectCallStructMethod(
                self.conn,
                KERNEL_INDEX_SMC,
                std::ptr::addr_of!(input).cast::<c_void>(),
                std::mem::size_of::<SmcKeyData>(),
                std::ptr::addr_of_mut!(output).cast::<c_void>(),
                &mut output_len,
            )
        };
        match rc {
            0 => {}
            K_IO_RETURN_NOT_PRIVILEGED => return Err(SmcError::NotPrivileged),
            other => return Err(SmcError::Io(other)),
        }
        check_result(input.key, &output)?;
        Ok(output)
    }
}

impl Drop for IoKitDriver {
    fn drop(&mut self) {
        // SAFETY: `self.conn` was opened in `open`, is closed once, and is
        // not used afterwards.
        unsafe { IOServiceClose(self.conn) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(result: u8, key: u32) -> SmcKeyData {
        SmcKeyData {
            key,
            result,
            ..SmcKeyData::default()
        }
    }

    /// Regression: `smcctl read CH0B` on a Mac without the key reported
    /// "SMC key  not found on this Mac", with the name missing, because the
    /// SMC leaves the key field of a refused reply at zero.
    #[test]
    fn check_result_names_the_requested_key_when_the_reply_omits_it() {
        let requested = fourcc("CH0B").unwrap();
        let error = check_result(requested, &reply(SMC_KEY_NOT_FOUND, 0)).unwrap_err();
        assert_eq!(error, SmcError::KeyNotFound("CH0B".to_string()));
        assert_eq!(error.to_string(), "SMC key CH0B not found on this Mac");
    }

    #[test]
    fn check_result_prefers_the_key_the_reply_carries() {
        let requested = fourcc("CH0B").unwrap();
        let echoed = fourcc("CH0C").unwrap();
        let error = check_result(requested, &reply(0x85, echoed)).unwrap_err();
        assert_eq!(
            error,
            SmcError::Smc {
                key: "CH0C".to_string(),
                result: 0x85
            }
        );
    }

    #[test]
    fn check_result_accepts_a_zero_result() {
        assert_eq!(check_result(fourcc("BUIC").unwrap(), &reply(0, 0)), Ok(()));
    }
}
