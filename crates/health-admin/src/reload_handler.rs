//! Reload handler: executes a named plugin replacement with a 2-second deadline.

use std::time::Duration;

use crate::PluginReloader;
use crate::readiness::HealthAdminError;

const RELOAD_DEADLINE: Duration = Duration::from_secs(2);

/// Execute a plugin reload within the 2-second deadline.
///
/// The preparation step (instantiation, validation, resource setup) is bounded
/// by the deadline. Once preparation returns a commit callback, the commit
/// performs only the atomic publication and cannot be cancelled by a timeout.
pub async fn execute_reload_with_deadline(
    reloader: &dyn PluginReloader,
    plugin_name: &str,
) -> Result<(), HealthAdminError> {
    let commit = match tokio::time::timeout(RELOAD_DEADLINE, reloader.prepare(plugin_name)).await {
        Ok(result) => result?,
        Err(_elapsed) => {
            return Err(HealthAdminError::ReloadFailed(format!(
                "Reload preparation for '{plugin_name}' did not complete within {RELOAD_DEADLINE:?}"
            )));
        }
    };
    commit();
    Ok(())
}
