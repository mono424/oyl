//! Signing in with the device flow (docs/sync-protocol.md, Auth): the app
//! shows a code, the user approves it in the browser, the app collects its
//! key.

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::api::{Account, Client, DevicePoll, DeviceStart};
use crate::error::Result;

/// How a sign-in ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Finished {
    /// Approved: the key (store it in the Keychain) and its account.
    Approved {
        /// The sync key.
        token: String,
        /// Whose it is.
        account: Account,
    },
    /// The user declined in the browser.
    Denied,
    /// The code expired before it was approved: start again.
    Expired,
    /// [`wait`] was told to stop.
    Cancelled,
}

/// Starts a sign-in of this Mac, called `device_name` in the browser.
pub fn start(client: &Client, device_name: &str) -> Result<DeviceStart> {
    client.start_device(device_name)
}

/// Polls until the sign-in `start` is approved, declined or expired, or
/// `cancel` is set (checked several times a second). Waits at least the
/// interval the server asks for between polls, longer after "slow down".
/// Blocking.
pub fn wait(client: &Client, start: &DeviceStart, cancel: &AtomicBool) -> Result<Finished> {
    wait_with(client, start, cancel, Duration::from_secs(1))
}

/// [`wait`] with another unit for the server's intervals (tests poll in
/// milliseconds).
pub fn wait_with(
    client: &Client,
    start: &DeviceStart,
    cancel: &AtomicBool,
    second: Duration,
) -> Result<Finished> {
    let deadline = Instant::now() + second * u32::try_from(start.expires_in).unwrap_or(600);
    let mut interval = start.interval.max(1);
    loop {
        if !sleep(second * u32::try_from(interval).unwrap_or(5), cancel) {
            return Ok(Finished::Cancelled);
        }
        if Instant::now() > deadline {
            return Ok(Finished::Expired);
        }
        match client.poll_device(&start.device_code) {
            Ok(DevicePoll::Approved { token, account }) => {
                return Ok(Finished::Approved { token, account });
            }
            Ok(DevicePoll::Pending { interval: next }) => interval = next.max(interval),
            Ok(DevicePoll::SlowDown { interval: next }) => interval = next.max(interval) + 1,
            Ok(DevicePoll::Denied) => return Ok(Finished::Denied),
            Ok(DevicePoll::Expired) => return Ok(Finished::Expired),
            // A dropped connection while waiting is not the end: try again.
            Err(err) if err.is_transient() => {
                log::info!("sync: polling the sign-in failed, trying again: {err}");
            }
            Err(err) => return Err(err),
        }
    }
}

/// Sleeps `duration` unless `cancel` is set first; whether it slept.
fn sleep(duration: Duration, cancel: &AtomicBool) -> bool {
    let until = Instant::now() + duration;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let now = Instant::now();
        if now >= until {
            return true;
        }
        thread::sleep((until - now).min(Duration::from_millis(100)));
    }
}
