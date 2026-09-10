//! Live-device checks. These need a real Mac, so they are ignored by default.
//!
//! Run them with `cargo test -p smc -- --ignored`. Only unprivileged reads
//! run here; writes need root and are covered by the daemon.

use smc::{IoKitDriver, Smc, ALL_KEYS, KEY_BUIC};

#[test]
#[ignore = "needs a real Mac with an AppleSMC service"]
fn opens_probes_and_reads_the_battery() {
    let mut smc = Smc::new(IoKitDriver::open().expect("open AppleSMC"));

    let present: Vec<&&str> = ALL_KEYS.iter().filter(|key| smc.has_key(key)).collect();
    println!("mode: {:?}", smc.charge_control_mode());
    println!("present keys: {present:?}");
    assert!(!present.is_empty(), "the probe found no known SMC key");

    let percent = smc.battery_percent().expect("read BUIC");
    println!("battery: {percent}%");
    assert!((1..=100).contains(&percent), "BUIC returned {percent}");

    assert!(smc.has_key(KEY_BUIC));
    println!("plugged in: {:?}", smc.is_plugged_in());
}
