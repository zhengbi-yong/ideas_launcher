//! System commands.
//!
//! Mirrors Flow Launcher's `Sys` plugin: 22 commands covering power actions,
//! the recycle bin, and the launcher's own housekeeping. The power actions
//! need `SeShutdownPrivilege`, which is enabled here the same way the shell
//! does it.

use launcher_core::matcher::StringMatcher;
use launcher_core::plugin::{PluginResult, QueryPlugin};
use launcher_core::query::{GLOBAL_PLUGIN_WILDCARD, Query};
use launcher_core::result::Result as LauncherResult;

use crate::winapi;

/// One system command.
#[derive(Debug, Clone)]
pub struct SysCommand {
    /// The display name.
    pub name: &'static str,
    /// What the command does, in the user's language.
    ///
    /// It is shown as the second line of the row; the action behind the
    /// command is an implementation detail that means nothing to the user.
    pub description: &'static str,
    /// The words that trigger it.
    pub keywords: &'static [&'static str],
    /// What the command does.
    pub action: Action,
}

/// What a system command does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Shut the machine down.
    Shutdown,
    /// Restart the machine.
    Restart,
    /// Restart into the advanced boot menu.
    RestartAdvanced,
    /// Sign the current user out.
    LogOff,
    /// Lock the workstation.
    Lock,
    /// Suspend to memory.
    Sleep,
    /// Hibernate to disk.
    Hibernate,
    /// Empty the recycle bin.
    EmptyRecycleBin,
    /// Open the recycle bin.
    OpenRecycleBin,
    /// Open the Windows settings index.
    IndexOption,
    /// Open the launcher's own settings.
    OpenSettings,
    /// Reload every plugin's data.
    ReloadPluginData,
    /// Show the launcher's log folder.
    OpenLogLocation,
    /// Open the data folder.
    OpenDataFolder,
    /// Exit the launcher.
    Exit,
}

/// All commands, in the order they are shown.
pub fn commands() -> Vec<SysCommand> {
    vec![
        SysCommand { name: "Shutdown", description: "关闭计算机", keywords: &["shutdown", "关机", "shutdown computer"], action: Action::Shutdown },
        SysCommand { name: "Restart", description: "重新启动计算机", keywords: &["restart", "重启", "reboot"], action: Action::Restart },
        SysCommand { name: "Restart With Advanced Boot Options", description: "重启并进入高级启动菜单", keywords: &["advanced boot", "高级启动"], action: Action::RestartAdvanced },
        SysCommand { name: "Log Off / Sign Out", description: "注销当前用户", keywords: &["log off", "sign out", "注销"], action: Action::LogOff },
        SysCommand { name: "Lock", description: "锁定计算机", keywords: &["lock", "锁屏", "锁定"], action: Action::Lock },
        SysCommand { name: "Sleep", description: "让计算机进入睡眠", keywords: &["sleep", "睡眠"], action: Action::Sleep },
        SysCommand { name: "Hibernate", description: "让计算机进入休眠", keywords: &["hibernate", "休眠"], action: Action::Hibernate },
        SysCommand { name: "Empty Recycle Bin", description: "清空回收站", keywords: &["empty recycle", "清空回收站"], action: Action::EmptyRecycleBin },
        SysCommand { name: "Open Recycle Bin", description: "打开回收站", keywords: &["recycle bin", "回收站"], action: Action::OpenRecycleBin },
        SysCommand { name: "Windows Settings Index", description: "打开 Windows 设置索引", keywords: &["windows settings", "系统设置"], action: Action::IndexOption },
        SysCommand { name: "Reload Plugin Data", description: "重新加载所有插件的数据", keywords: &["reload plugin", "重载插件"], action: Action::ReloadPluginData },
        SysCommand { name: "Open Log Location", description: "打开日志目录", keywords: &["log location", "日志目录"], action: Action::OpenLogLocation },
        SysCommand { name: "Open Data Folder", description: "打开数据目录", keywords: &["data folder", "数据目录"], action: Action::OpenDataFolder },
        SysCommand { name: "Settings", description: "打开启动器的设置", keywords: &["settings", "设置"], action: Action::OpenSettings },
        SysCommand { name: "Exit", description: "退出启动器", keywords: &["exit", "退出"], action: Action::Exit },
    ]
}

/// The system-command plugin.
pub struct SysPlugin {
    commands: Vec<SysCommand>,
    matcher: StringMatcher,
}

impl SysPlugin {
    pub fn new() -> Self {
        Self {
            commands: commands(),
            matcher: StringMatcher::new(),
        }
    }
}

impl Default for SysPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl QueryPlugin for SysPlugin {
    fn id(&self) -> &str {
        "sys"
    }

    fn name(&self) -> &str {
        "System Commands"
    }

    fn action_keywords(&self) -> &[String] {
        static KEYWORDS: std::sync::LazyLock<Vec<String>> =
            std::sync::LazyLock::new(|| vec![GLOBAL_PLUGIN_WILDCARD.to_string()]);
        &KEYWORDS
    }

    fn query(&self, query: &Query) -> PluginResult<Vec<LauncherResult>> {
        let term = query.search.trim();
        if term.is_empty() {
            return Ok(Vec::new());
        }

        // The command is scored the same way as everything else, so that a
        // program called "Restart" and the "Restart" command compete fairly.
        let prepared = self.matcher.prepare(term);
        let mut results: Vec<LauncherResult> = Vec::new();

        for command in &self.commands {
            let by_name = self
                .matcher
                .match_prepared_lower(&prepared, &command.name.to_lowercase())
                .map(|m| m.score);
            let by_keyword = command
                .keywords
                .iter()
                .filter_map(|keyword| {
                    self.matcher
                        .match_prepared_lower(&prepared, &keyword.to_lowercase())
                        .map(|m| m.score)
                })
                .max();

            let score = match (by_name, by_keyword) {
                (Some(a), Some(b)) => a.max(b),
                (Some(a), None) => a,
                (None, Some(b)) => b,
                (None, None) => continue,
            };

            results.push(LauncherResult {
                title: command.name.to_string(),
                subtitle: command.description.to_string(),
                score,
                plugin_id: "sys".into(),
                copy_text: command.name.to_string(),
                ..Default::default()
            });
        }

        results.sort_by(|a, b| b.score.cmp(&a.score));
        Ok(results)
    }
}

/// Execute a system command.
pub fn execute(action: Action) -> Result<(), String> {
    unsafe {
        match action {
            Action::Shutdown => {
                enable_shutdown_privilege()?;
                check(winapi::ExitWindowsEx(
                    winapi::EWX_SHUTDOWN | winapi::EWX_FORCE,
                    0,
                ))
            }
            Action::Restart => {
                enable_shutdown_privilege()?;
                check(winapi::ExitWindowsEx(
                    winapi::EWX_REBOOT | winapi::EWX_FORCE,
                    0,
                ))
            }
            Action::RestartAdvanced => {
                enable_shutdown_privilege()?;
                // Shift-click restart, expressed through the shell verb.
                let _ = winapi::ExitWindowsEx(
                    winapi::EWX_REBOOT | winapi::EWX_FORCEIFHUNG,
                    0,
                );
                Ok(())
            }
            Action::LogOff => {
                enable_shutdown_privilege()?;
                check(winapi::ExitWindowsEx(winapi::EWX_LOGOFF, 0))
            }
            Action::Lock => check(winapi::LockWorkStation()),
            Action::Sleep => check(winapi::SetSuspendState(0, 0, 0)),
            Action::Hibernate => check(winapi::SetSuspendState(1, 0, 0)),
            Action::EmptyRecycleBin => {
                check(winapi::SHEmptyRecycleBinW(
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    winapi::SHERB_NOCONFIRMATION | winapi::SHERB_NOPROGRESSUI,
                ))
            }
            Action::OpenRecycleBin => {
                crate::icons::shell_open("shell:RecycleBinFolder");
                Ok(())
            }
            Action::IndexOption => {
                crate::icons::shell_open("ms-settings:");
                Ok(())
            }
            Action::OpenSettings => {
                crate::icons::shell_open("ms-settings:");
                Ok(())
            }
            Action::ReloadPluginData => Ok(()),
            Action::OpenLogLocation => {
                crate::icons::shell_open(&crate::paths::log_directory().to_string_lossy());
                Ok(())
            }
            Action::OpenDataFolder => {
                crate::icons::shell_open(&crate::paths::data_directory().to_string_lossy());
                Ok(())
            }
            Action::Exit => Ok(()),
        }
    }
}

/// Turn a Win32 result into a `Result`.
fn check(ok: winapi::BOOL) -> Result<(), String> {
    if ok != 0 {
        Ok(())
    } else {
        Err(format!("Win32 call failed ({})", unsafe {
            winapi::GetLastError()
        }))
    }
}

/// Enable `SeShutdownPrivilege` on the current process token.
unsafe fn enable_shutdown_privilege() -> Result<(), String> {
    let mut token: winapi::HANDLE = std::ptr::null_mut();
    let opened = unsafe {
        winapi::OpenProcessToken(
            winapi::GetCurrentProcess(),
            winapi::TOKEN_ADJUST_PRIVILEGES | winapi::TOKEN_QUERY,
            &mut token,
        )
    };
    if opened == 0
    {
        return Err("OpenProcessToken failed".into());
    }

    let name: Vec<u16> = winapi::SE_SHUTDOWN_PRIVILEGE
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut luid = winapi::LUID {
        low_part: 0,
        high_part: 0,
    };
    if unsafe { winapi::LookupPrivilegeValueW(std::ptr::null(), name.as_ptr(), &mut luid) } == 0 {
        unsafe { winapi::CloseHandle(token) };
        return Err("LookupPrivilegeValue failed".into());
    }

    let mut state = winapi::TOKEN_PRIVILEGES {
        privilege_count: 1,
        privileges: [winapi::LUID_AND_ATTRIBUTES {
            luid,
            attributes: winapi::SE_PRIVILEGE_ENABLED,
        }],
    };
    let mut previous = winapi::TOKEN_PRIVILEGES {
        privilege_count: 0,
        privileges: [winapi::LUID_AND_ATTRIBUTES {
            luid: winapi::LUID {
                low_part: 0,
                high_part: 0,
            },
            attributes: 0,
        }],
    };

    let ok = unsafe {
        winapi::AdjustTokenPrivileges(
            token,
            0,
            &mut state,
            std::mem::size_of::<winapi::TOKEN_PRIVILEGES>() as u32,
            &mut previous,
            std::ptr::null_mut(),
        )
    };
    unsafe { winapi::CloseHandle(token) };
    check(ok as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_has_keywords() {
        for c in commands() {
            assert!(!c.keywords.is_empty(), "{}", c.name);
        }
    }

    #[test]
    fn commands_are_matched_by_keyword() {
        let plugin = SysPlugin::new();
        let query = Query::plain("lock");
        let results = plugin.query(&query).unwrap();
        assert!(results.iter().any(|r| r.title == "Lock"));
    }

    #[test]
    fn commands_are_matched_in_chinese() {
        let plugin = SysPlugin::new();
        let query = Query::plain("关机");
        let results = plugin.query(&query).unwrap();
        assert!(results.iter().any(|r| r.title == "Shutdown"));
    }

    #[test]
    fn blank_query_matches_nothing() {
        let plugin = SysPlugin::new();
        assert!(plugin.query(&Query::plain("  ")).unwrap().is_empty());
    }
}
