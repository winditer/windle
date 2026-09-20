//! Smart Uninstall — remove an app together with everything it left behind.
//!
//! The wire types, the request validation and the commands are the same
//! everywhere; where installed programs are found and how they are taken away
//! is not, so each platform has its own implementation in a sibling module.

use serde::Serialize;

use super::{CleanOutcome, RiskLevel};
use crate::utils::{history, Result, WindleError};

#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod imp;
#[cfg(target_os = "windows")]
#[path = "windows.rs"]
mod imp;

/// A name shorter than this is too generic to attribute leftovers by ("Go",
/// "Vim"), so those never match by name alone.
const MIN_NAME_MATCH_LEN: usize = 4;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledApp {
    pub id: String,
    pub name: String,
    pub bundle_id: Option<String>,
    pub version: Option<String>,
    pub path: String,
    pub bundle_size: u64,
    pub icon_path: Option<String>,
    pub installed_at: Option<u64>,
    pub last_used_at: Option<u64>,
    pub is_system: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LeftoverKind {
    Preferences,
    ApplicationSupport,
    Caches,
    Logs,
    SavedState,
    Containers,
    LaunchAgent,
    Receipt,
    Other,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppLeftover {
    pub path: String,
    pub kind: LeftoverKind,
    pub size: u64,
    pub risk: RiskLevel,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UninstallPlan {
    pub app: InstalledApp,
    pub leftovers: Vec<AppLeftover>,
    pub total_size: u64,
    pub requires_elevation: bool,
}

/// Every installed app.
#[tauri::command]
pub async fn list_apps() -> Result<Vec<InstalledApp>> {
    Ok(imp::installed_apps())
}

/// Collect the app plus every leftover we can attribute to it.
#[tauri::command]
pub async fn build_uninstall_plan(app_id: String) -> Result<UninstallPlan> {
    let app = imp::find_app(&app_id)?;
    Ok(imp::plan_for(app))
}

/// Execute a plan. `paths` names the app and the leftovers to take with it —
/// the app itself is always removed, whether or not it was requested.
#[tauri::command]
pub async fn uninstall_app(app_id: String, paths: Vec<String>) -> Result<CleanOutcome> {
    let app = imp::find_app(&app_id)?;

    if app.is_system {
        return Err(WindleError::Protected(app.path));
    }

    // Rebuild the plan and only act on paths it actually contains, so a stale
    // or tampered request cannot turn into an arbitrary delete.
    let plan = imp::plan_for(app);
    let (ordered, rejected) = requested_paths(&plan, paths);

    // Removal can take minutes — an uninstaller gets to run on Windows — so it
    // must not block a runtime thread.
    let result = tauri::async_runtime::spawn_blocking(move || {
        let mut outcome = CleanOutcome::default();

        for path in rejected {
            outcome.fail(path, "not part of this uninstall plan");
        }

        imp::remove_selected(&plan, &ordered, &mut outcome);
        outcome
    })
    .await;

    let outcome = match result {
        Ok(outcome) => outcome,
        Err(error) => {
            return Err(WindleError::Command {
                command: "uninstall".into(),
                message: error.to_string(),
            })
        }
    };

    history::record_outcome(history::Operation::Uninstall, &outcome);

    Ok(outcome)
}

/// Resolve a removal request against `plan`: the requested leftovers plus the
/// app itself, which is never optional — a request that only lists leftovers
/// must not leave the app behind. Requested paths the plan does not cover are
/// returned separately, to be reported as failures instead of removed.
fn requested_paths(plan: &UninstallPlan, requested: Vec<String>) -> (Vec<String>, Vec<String>) {
    let allowed: Vec<&str> = std::iter::once(plan.app.path.as_str())
        .chain(plan.leftovers.iter().map(|leftover| leftover.path.as_str()))
        .collect();

    let mut rejected = Vec::new();
    let mut selected: Vec<String> = requested
        .into_iter()
        .filter(|path| {
            if allowed.contains(&path.as_str()) {
                true
            } else {
                rejected.push(path.clone());
                false
            }
        })
        .collect();

    if !selected.iter().any(|path| path == &plan.app.path) {
        selected.push(plan.app.path.clone());
    }

    // The app goes last in this list — an interrupted macOS run leaves it
    // visible rather than half-gone — while each platform's removal reorders
    // the steps as its own tools require.
    selected.sort_by_key(|path| path == &plan.app.path);
    (selected, rejected)
}

/// Leftovers whose owning app is already gone.
#[tauri::command]
pub async fn find_orphaned_leftovers() -> Result<Vec<UninstallPlan>> {
    Ok(imp::find_orphaned_leftovers())
}

/// Cheap count for the dashboard — no sizing involved.
pub fn installed_app_count() -> usize {
    imp::installed_app_count()
}

/// Lowercased names of the installed apps, used by the installer scan to flag
/// an installer as already applied. macOS reads bundle names, Windows registry
/// display names.
pub fn installed_names() -> Vec<String> {
    imp::installed_names()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_fixture() -> UninstallPlan {
        UninstallPlan {
            app: InstalledApp {
                id: "/Applications/Acme.app".into(),
                name: "Acme".into(),
                bundle_id: Some("com.acme.Acme".into()),
                version: Some("1.0".into()),
                path: "/Applications/Acme.app".into(),
                bundle_size: 1_000,
                icon_path: None,
                installed_at: None,
                last_used_at: None,
                is_system: false,
            },
            leftovers: vec![AppLeftover {
                path: "/Users/test/Library/Preferences/com.acme.Acme.plist".into(),
                kind: LeftoverKind::Preferences,
                size: 100,
                risk: RiskLevel::Safe,
            }],
            total_size: 1_100,
            requires_elevation: false,
        }
    }

    #[test]
    fn the_app_is_removed_even_when_only_leftovers_are_requested() {
        let plan = plan_fixture();

        let (selected, rejected) = requested_paths(
            &plan,
            vec!["/Users/test/Library/Preferences/com.acme.Acme.plist".into()],
        );

        assert!(rejected.is_empty());
        assert_eq!(
            selected,
            vec![
                "/Users/test/Library/Preferences/com.acme.Acme.plist".to_string(),
                plan.app.path.clone(),
            ]
        );
    }

    #[test]
    fn an_empty_request_still_removes_the_app() {
        let plan = plan_fixture();

        let (selected, rejected) = requested_paths(&plan, vec![]);

        assert!(rejected.is_empty());
        assert_eq!(selected, vec![plan.app.path.clone()]);
    }

    #[test]
    fn the_app_is_not_removed_twice() {
        let plan = plan_fixture();

        let (selected, _) = requested_paths(&plan, vec![plan.app.path.clone()]);

        assert_eq!(selected, vec![plan.app.path.clone()]);
    }

    #[test]
    fn paths_outside_the_plan_are_rejected() {
        let plan = plan_fixture();

        let (selected, rejected) = requested_paths(&plan, vec!["/etc/hosts".into()]);

        assert_eq!(rejected, vec!["/etc/hosts".to_string()]);
        assert_eq!(selected, vec![plan.app.path.clone()]);
    }
}
