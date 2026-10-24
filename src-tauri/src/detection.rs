/**
 * @Author: Danson zheng
 * @Date: 2026-10-24
 * @Description: 本机开发智能体检测。安装状态通过 PATH 命令、macOS 应用包和数据目录判断，
 * 运行状态通过进程列表与注册表中的关键字匹配判断；只读取进程命令行，不读取会话内容。
 */

use crate::tools::ToolId;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 进程扫描间隔；避免每秒执行 ps 带来额外开销
pub const DETECTION_INTERVAL_MS: i64 = 2_000;

#[derive(Debug, Clone)]
pub struct ToolDetector {
    home: Option<PathBuf>,
    path_dirs: Vec<PathBuf>,
    app_dirs: Vec<PathBuf>,
}

impl Default for ToolDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolDetector {
    pub fn new() -> Self {
        let home = default_home_dir();
        let path_dirs = env::var_os("PATH")
            .map(|path| env::split_paths(&path).collect())
            .unwrap_or_default();
        let app_dirs = default_app_dirs(home.as_deref());
        Self {
            home,
            path_dirs,
            app_dirs,
        }
    }

    /// 显式注入运行环境，便于测试；生产代码使用 new()
    #[cfg(test)]
    pub fn with_environment(
        home: Option<PathBuf>,
        path_dirs: Vec<PathBuf>,
        app_dirs: Vec<PathBuf>,
    ) -> Self {
        Self {
            home,
            path_dirs,
            app_dirs,
        }
    }

    pub fn installed_tools(&self) -> Vec<ToolId> {
        ToolId::ALL
            .into_iter()
            .filter(|tool| self.is_installed(*tool))
            .collect()
    }

    pub fn running_tools(&self) -> Vec<ToolId> {
        match process_listing() {
            Ok(listing) => self.running_from_listing(&listing),
            Err(error) => {
                log::warn!("【工具检测】读取进程列表失败 error={error}");
                Vec::new()
            }
        }
    }

    /// 从进程列表文本匹配工具；只检查可执行文件名或解释器脚本路径，
    /// 不匹配普通参数，避免终端里出现关键字就误判工具在运行。
    pub fn running_from_listing(&self, listing: &str) -> Vec<ToolId> {
        let mut matched = std::collections::HashSet::new();

        for line in listing.lines() {
            let tokens: Vec<&str> = line.split_whitespace().collect();
            let Some(executable) = tokens.first() else {
                continue;
            };

            let mut candidates = vec![*executable];
            if is_script_interpreter(executable) {
                candidates.extend(
                    tokens
                        .iter()
                        .skip(1)
                        .take(2)
                        .copied()
                        .filter(|token| looks_like_script_path(token)),
                );
            }

            for candidate in candidates {
                for tool in ToolId::ALL {
                    if matched.contains(&tool) {
                        continue;
                    }
                    if process_matches(tool, candidate) {
                        matched.insert(tool);
                    }
                }
            }
        }

        ToolId::ALL
            .into_iter()
            .filter(|tool| matched.contains(tool))
            .collect()
    }

    fn is_installed(&self, tool: ToolId) -> bool {
        let descriptor = tool.descriptor();
        descriptor
            .command_names
            .iter()
            .any(|name| self.command_exists(name))
            || descriptor
                .data_dirs
                .iter()
                .any(|directory| self.home.as_deref().is_some_and(|home| home.join(directory).is_dir()))
            || descriptor
                .app_bundles
                .iter()
                .any(|bundle| self.app_dirs.iter().any(|dir| dir.join(bundle).is_dir()))
    }

    /// 在 PATH 中查找可执行文件；Windows 额外尝试常见可执行扩展名
    fn command_exists(&self, name: &str) -> bool {
        const WINDOWS_EXTENSIONS: [&str; 5] = [".exe", ".cmd", ".bat", ".ps1", ""];
        const UNIX_EXTENSIONS: [&str; 1] = [""];
        let extensions: &[&str] = if cfg!(windows) {
            &WINDOWS_EXTENSIONS
        } else {
            &UNIX_EXTENSIONS
        };

        self.path_dirs.iter().any(|dir| {
            extensions.iter().any(|extension| {
                let candidate = dir.join(format!("{name}{extension}"));
                is_executable_file(&candidate)
            })
        })
    }
}

/// 安装检测结果用于界面提示，这里统一转换名称，避免在日志里打印内部 id
pub fn tool_names(tools: &[ToolId]) -> Vec<&'static str> {
    tools.iter().map(|tool| tool.display_name()).collect()
}

/// 解释器本身不能代表工具，需要继续检查其后紧跟的脚本路径
fn is_script_interpreter(executable: &str) -> bool {
    let name = executable
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(executable)
        .to_lowercase();
    matches!(
        name.as_str(),
        "node" | "node.exe" | "bun" | "bun.exe" | "deno" | "deno.exe" | "python" | "python3"
            | "python.exe"
    )
}

fn looks_like_script_path(token: &str) -> bool {
    if token.starts_with('-') {
        return false;
    }
    token.contains('/')
        || token.contains('\\')
        || [".js", ".mjs", ".cjs", ".ts", ".py"]
            .iter()
            .any(|extension| token.ends_with(extension))
}

/// 只对可执行文件名做边界匹配；应用包路径按 .app 目录匹配
fn process_matches(tool: ToolId, candidate: &str) -> bool {
    let lower = candidate.to_lowercase();
    let descriptor = tool.descriptor();

    if descriptor.app_bundles.iter().any(|bundle| {
        lower.contains(&format!("{}/", bundle.to_lowercase()))
            || lower.contains(&format!("{}\\", bundle.to_lowercase()))
    }) {
        return true;
    }

    let basename = lower
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(lower.as_str());
    let segments: Vec<&str> = lower
        .split(['/', '\\'])
        .filter(|segment| !segment.is_empty())
        .collect();

    descriptor.process_keywords.iter().any(|keyword| {
        matches_keyword(basename.trim_start_matches('.'), keyword)
            || segments
                .iter()
                .any(|segment| matches_keyword(segment.trim_start_matches('.'), keyword))
    })
}

/// 关键字按词边界匹配，避免 claude 命中 claudette 之类的无关进程
fn matches_keyword(text: &str, keyword: &str) -> bool {
    text == keyword
        || text
            .strip_prefix(keyword)
            .is_some_and(|rest| rest.chars().next().is_some_and(|next| !next.is_alphanumeric()))
}

fn default_home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn default_app_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    #[cfg(target_os = "macos")]
    {
        dirs.push(PathBuf::from("/Applications"));
        if let Some(home) = home {
            dirs.push(home.join("Applications"));
        }
    }
    #[cfg(target_os = "windows")]
    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        dirs.push(PathBuf::from(local_app_data).join("Programs"));
    }
    let _ = home;
    dirs
}

fn is_executable_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return path
            .metadata()
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false);
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn process_listing() -> Result<String, std::io::Error> {
    #[cfg(target_os = "windows")]
    let output = Command::new("tasklist")
        .args(["/FO", "CSV", "/NH"])
        .output()?;

    #[cfg(not(target_os = "windows"))]
    let output = Command::new("ps").args(["-axo", "command="]).output()?;

    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "进程列表命令退出码异常 status={}",
            output.status
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir(name: &str) -> PathBuf {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = env::temp_dir().join(format!(
            "token-tracker-detection-{}-{}-{}",
            std::process::id(),
            name,
            unique
        ));
        fs::create_dir_all(&path).expect("测试目录应可创建");
        path
    }

    #[test]
    fn detects_commands_data_dirs_and_app_bundles() {
        let home = temp_dir("home");
        let bin = temp_dir("bin");
        let apps = temp_dir("apps");

        let command = bin.join("claude");
        fs::write(&command, "#!/bin/sh\n").expect("测试命令应可写入");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&command, fs::Permissions::from_mode(0o755))
                .expect("测试命令应可执行");
        }

        fs::create_dir_all(home.join(".codex")).expect("Codex 数据目录应可创建");
        fs::create_dir_all(apps.join("Qoder.app")).expect("Qoder 应用包应可创建");

        let detector = ToolDetector::with_environment(
            Some(home.clone()),
            vec![bin],
            vec![apps],
        );
        let installed = detector.installed_tools();

        assert!(installed.contains(&ToolId::CLAUDE_CODE));
        assert!(installed.contains(&ToolId::CODEX_CLI));
        assert!(installed.contains(&ToolId::QODER));
        assert!(!installed.contains(&ToolId::TRAE));

        fs::remove_dir_all(&home).expect("测试目录应可删除");
    }

    #[test]
    fn non_executable_path_entry_is_not_installed() {
        let home = temp_dir("home-plain");
        let bin = temp_dir("bin-plain");
        let command = bin.join("codex");
        fs::write(&command, "plain file").expect("测试文件应可写入");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&command, fs::Permissions::from_mode(0o644))
                .expect("测试文件权限应可设置");
        }

        let detector = ToolDetector::with_environment(Some(home.clone()), vec![bin], vec![]);
        assert!(!detector.installed_tools().contains(&ToolId::CODEX_CLI));

        fs::remove_dir_all(&home).expect("测试目录应可删除");
    }

    #[test]
    fn running_listing_matches_registry_keywords() {
        let detector = ToolDetector::with_environment(None, Vec::new(), Vec::new());
        let listing = concat!(
            "/Applications/Qoder.app/Contents/MacOS/Qoder --type=renderer\n",
            "node /usr/local/bin/claude --resume\n",
            "/opt/homebrew/bin/zcode-cli run\n",
            "/usr/bin/sleep 600\n"
        );

        let running = detector.running_from_listing(listing);
        assert!(running.contains(&ToolId::QODER));
        assert!(running.contains(&ToolId::CLAUDE_CODE));
        assert!(running.contains(&ToolId::ZCODE));
        assert_eq!(running.len(), 3);
    }

    #[test]
    fn keywords_in_shell_arguments_do_not_count() {
        let detector = ToolDetector::with_environment(None, Vec::new(), Vec::new());
        let listing = "/bin/zsh -c grep claude codex zcode qoder gemini kimi\n";
        assert!(detector.running_from_listing(listing).is_empty());
    }

    #[test]
    fn node_script_directory_is_used_for_cli_tools() {
        let detector = ToolDetector::with_environment(None, Vec::new(), Vec::new());
        let listing = concat!(
            "/opt/homebrew/bin/node /usr/local/lib/node_modules/gemini-cli/dist/index.js --flag\n",
            "/usr/bin/python3 /Users/test/.openclaw/bin/runner.py\n"
        );

        let running = detector.running_from_listing(listing);
        assert!(running.contains(&ToolId::GEMINI_CLI));
        assert!(running.contains(&ToolId::OPEN_CLAW));
    }

    #[test]
    fn helper_processes_match_their_app() {
        let detector = ToolDetector::with_environment(None, Vec::new(), Vec::new());
        let listing = concat!(
            "/Applications/WorkBuddy.app/Contents/MacOS/Electron\n",
            "/Applications/Trae CN.app/Contents/MacOS/Electron\n",
            "/Applications/ZCode.app/Contents/Frameworks/ZCode Helper.app/Contents/MacOS/ZCode Helper --type=gpu-process\n"
        );

        let running = detector.running_from_listing(listing);
        assert!(running.contains(&ToolId::WORK_BUDDY));
        assert!(running.contains(&ToolId::TRAE));
        assert!(running.contains(&ToolId::ZCODE));
    }

    #[test]
    fn empty_listing_yields_no_running_tools() {
        let detector = ToolDetector::with_environment(None, Vec::new(), Vec::new());
        assert!(detector.running_from_listing("").is_empty());
        assert!(detector
            .running_from_listing("WindowServer\nloginwindow\n")
            .is_empty());
    }
}
