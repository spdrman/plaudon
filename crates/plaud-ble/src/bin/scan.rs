//! `plaud-ble-scan`: the opt-in A2.3 read-only scanner.
//!
//! Build and run explicitly; it is never part of a default build or of CI:
//!
//! ```text
//! cargo run -p plaudon-plaud-ble --features hardware-scan --bin plaud-ble-scan -- 8
//! ```
//!
//! On macOS the binary must be run from a bundle carrying a Bluetooth usage
//! description, or the operating system denies the radio. That denial is
//! reported, not swallowed.

use std::time::Duration;

use plaudon_plaud_ble::discovery::DiscoveryPolicy;
use plaudon_plaud_ble::scanner;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(8);

    match scanner::scan_once(&DiscoveryPolicy::default(), Duration::from_secs(seconds)).await {
        Ok(report) => {
            println!("adapter: {}", report.adapter);
            println!("advertisements observed: {}", report.observed.len());
            for advertisement in &report.observed {
                println!(
                    "  {} name={:?} services={:04x?} manufacturers={:04x?} rssi={:?}",
                    advertisement.peripheral_id,
                    advertisement.local_name,
                    advertisement.service_ids,
                    advertisement.manufacturer_ids,
                    advertisement.rssi
                );
            }
            println!("candidates: {}", report.candidates.len());
            for candidate in &report.candidates {
                println!(
                    "  {} name={:?}",
                    candidate.peripheral_id, candidate.advertised_name
                );
            }
            println!();
            println!("No control frame was written. This scan cannot mutate a device.");
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("scan failed: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
