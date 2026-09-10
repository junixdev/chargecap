//! In-memory [`Driver`] for tests.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use crate::driver::{
    check_result, fourcc, fourcc_str, Driver, SmcKeyData, CMD_READ_BYTES, CMD_READ_KEY_INFO,
    CMD_WRITE_BYTES, MAX_DATA_LEN, SMC_KEY_NOT_FOUND,
};
use crate::SmcError;

#[derive(Debug, Default)]
struct MockState {
    /// FourCC -> (data type FourCC, current bytes).
    keys: BTreeMap<u32, (u32, Vec<u8>)>,
    /// Every write, in order, as `(key, bytes)`.
    writes: Vec<(String, Vec<u8>)>,
    /// Keys whose stored bytes a write records but does not change.
    sticky: BTreeSet<u32>,
}

/// A fake SMC backed by a map. Clones share one state, so a test can keep a
/// handle after moving the driver into [`crate::Smc`].
#[derive(Debug, Clone, Default)]
pub struct MockDriver {
    state: Rc<RefCell<MockState>>,
}

impl MockDriver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a key with its current bytes. The data type follows the length.
    pub fn seed(&self, key: &str, bytes: &[u8]) -> &Self {
        let data_type = match bytes.len() {
            1 => "ui8 ",
            2 => "ui16",
            4 => "ui32",
            _ => "hex_",
        };
        self.seed_typed(key, data_type, bytes)
    }

    /// Adds a key that accepts a write but keeps returning `bytes`.
    ///
    /// Models a key the firmware owns, such as `ACLC`: writing `0x00` hands
    /// the MagSafe LED back to the system, and the next read reports the
    /// colour the system chose, never the byte that was written.
    pub fn seed_sticky(&self, key: &str, bytes: &[u8]) -> &Self {
        self.seed(key, bytes);
        let key = fourcc(key).expect("4-character key");
        self.state.borrow_mut().sticky.insert(key);
        self
    }

    /// Adds a key with an explicit data type FourCC.
    pub fn seed_typed(&self, key: &str, data_type: &str, bytes: &[u8]) -> &Self {
        let key = fourcc(key).expect("4-character key");
        let data_type = fourcc(data_type).expect("4-character type");
        self.state
            .borrow_mut()
            .keys
            .insert(key, (data_type, bytes.to_vec()));
        self
    }

    /// Returns every write so far, in order.
    pub fn writes(&self) -> Vec<(String, Vec<u8>)> {
        self.state.borrow().writes.clone()
    }

    /// Forgets the recorded writes. Stored values stay.
    pub fn clear_writes(&self) {
        self.state.borrow_mut().writes.clear();
    }

    /// Returns the current bytes of a key.
    pub fn value(&self, key: &str) -> Option<Vec<u8>> {
        let key = fourcc(key).ok()?;
        self.state.borrow().keys.get(&key).map(|(_, v)| v.clone())
    }
}

impl Driver for MockDriver {
    fn call(&mut self, input: SmcKeyData) -> Result<SmcKeyData, SmcError> {
        let mut output = SmcKeyData {
            key: input.key,
            data8: input.data8,
            ..SmcKeyData::default()
        };
        let mut state = self.state.borrow_mut();
        let Some((data_type, value)) = state.keys.get(&input.key).cloned() else {
            output.result = SMC_KEY_NOT_FOUND;
            check_result(&output)?;
            unreachable!("check_result rejects a nonzero result");
        };

        match input.data8 {
            CMD_READ_KEY_INFO => {
                output.key_info.data_size = value.len() as u32;
                output.key_info.data_type = data_type;
            }
            CMD_READ_BYTES => {
                let len = value.len().min(MAX_DATA_LEN);
                output.key_info.data_size = len as u32;
                output.bytes[..len].copy_from_slice(&value[..len]);
            }
            CMD_WRITE_BYTES => {
                let len = (input.key_info.data_size as usize).min(MAX_DATA_LEN);
                let written = input.bytes[..len].to_vec();
                if !state.sticky.contains(&input.key) {
                    state
                        .keys
                        .insert(input.key, (data_type, written.clone()))
                        .expect("key was present");
                }
                state.writes.push((fourcc_str(input.key), written));
            }
            other => {
                output.result = other;
                check_result(&output)?;
            }
        }
        Ok(output)
    }
}
