//! Keeping the Mac awake while an agent works: a round in flight, a slot
//! being created or torn down, something running in the background.
//!
//! Claude Code holds a sleep inhibitor only in its own terminal UI, not
//! headless as Berth runs it, so Berth holds one: a user-initiated activity,
//! which keeps the Mac from sleeping when idle and Berth out of App Nap while
//! its windows can't be seen (the screen locked). The display still sleeps
//! and the screen still locks; a laptop's closed lid still sleeps it. Ends
//! with the process. Setting `keep_awake` "off" lets the Mac sleep.

use std::sync::Mutex;
use std::sync::mpsc::{Sender, channel};

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};

use super::{Shared, lock};

/// The activity, held on a thread of its own (its token stays where it
/// began); `set` says whether it should be held.
pub(super) struct Awake {
    tx: Sender<bool>,
    held: Mutex<bool>,
}

impl Awake {
    pub(super) fn start() -> Self {
        let (tx, rx) = channel::<bool>();
        let spawned = std::thread::Builder::new()
            .name("keep-awake".into())
            .spawn(move || {
                let mut activity = None;
                for on in rx {
                    match (on, activity.take()) {
                        (true, None) => activity = Some(begin()),
                        (false, Some(a)) => end(&a),
                        (_, kept) => activity = kept,
                    }
                }
                if let Some(a) = activity {
                    end(&a);
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "keep-awake thread");
        }
        Self {
            tx,
            held: Mutex::new(false),
        }
    }

    fn set(&self, on: bool) {
        let mut held = lock(&self.held);
        if *held != on {
            *held = on;
            tracing::info!(on, "keep the Mac awake");
            let _ = self.tx.send(on);
        }
    }
}

fn begin() -> Retained<ProtocolObject<dyn NSObjectProtocol>> {
    autoreleasepool(|_| {
        NSProcessInfo::processInfo().beginActivityWithOptions_reason(
            NSActivityOptions::UserInitiated,
            &NSString::from_str("Agents are working"),
        )
    })
}

fn end(activity: &ProtocolObject<dyn NSObjectProtocol>) {
    autoreleasepool(|_| {
        // SAFETY: `activity` is the token `begin` got from this process's
        // NSProcessInfo, and it is ended once.
        #[allow(unsafe_code)]
        unsafe {
            NSProcessInfo::processInfo().endActivity(activity);
        }
    });
}

impl Shared {
    /// Hold the activity while an agent works anywhere, unless the user let
    /// the Mac sleep. Call after anything that can start or end that work.
    pub(super) fn keep_awake(&self) {
        let (off, working) = {
            let core = self.core();
            (
                core.store.setting("keep_awake").ok().flatten().as_deref() == Some("off"),
                core.store.agents_working().unwrap_or(false),
            )
        };
        let background = !lock(&self.background).is_empty();
        self.awake.set(!off && (working || background));
    }
}
