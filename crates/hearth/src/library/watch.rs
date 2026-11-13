//! Watches the games folders, so a game copied in shows up without anyone pressing Rescan.

use std::{path::Path, time::Duration};

use notify::{RecursiveMode, Watcher};
use tokio::sync::mpsc;

use crate::state::AppState;

/// Runs until shutdown. Follows the settings: the folder list and the "Watch for changes" switch.
///
/// ponytail: only the top of each games folder is watched. A file changed deep inside a game
/// is picked up by the next full scan, not at once. Watching every subfolder would need one
/// watch per directory, which large libraries run out of on Linux.
pub async fn watcher(state: AppState) {
    let mut settings = state.watch_settings();
    loop {
        let current = settings.borrow_and_update().clone();
        let (events, mut fired) = mpsc::unbounded_channel::<()>();
        // Dropping the watcher at the end of each round stops the watches it holds.
        let mut watching = None;
        if current.watch_folders && state.setup_complete() {
            let made = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                if event.is_ok() {
                    let _ = events.send(());
                }
            });
            match made {
                Ok(mut watcher) => {
                    for folder in &current.games_folders {
                        if let Err(err) = watcher.watch(Path::new(folder), RecursiveMode::NonRecursive) {
                            tracing::warn!(folder, error = %err, "could not watch a games folder");
                        }
                    }
                    watching = Some(watcher);
                }
                Err(err) => tracing::warn!(error = %err, "watching for changes is not available here"),
            }
        }

        loop {
            tokio::select! {
                _ = state.shutdown.cancelled() => return,
                changed = settings.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    break;
                }
                Some(()) = fired.recv(), if watching.is_some() => {
                    // Copying a game fires many events. Wait until the folder has been quiet for a moment.
                    loop {
                        tokio::select! {
                            _ = tokio::time::sleep(Duration::from_secs(2)) => break,
                            more = fired.recv() => if more.is_none() { break },
                        }
                    }
                    state.library.request_scan();
                }
            }
        }
    }
}
