/**
 * @Author: Danson zheng
 * @Date: 2026-10-24
 * @Description: 开发智能体注册表。统一描述工具标识、展示名称、强调色、进程关键字、
 * 安装标记与日志适配器；新增工具时只需补一条描述和对应解析器。
 */

use serde::Serialize;

/// 工具数据适配器。Token 源与状态源分开判断：
/// Qoder CN / Trae / Trae CN 只能读到会话状态，OpenClaw/Gemini/ChatGPT/DeepSeek
/// 目前没有本地 token 记录，这些工具仍然显示运行状态，但不会伪造 token 数据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterKind {
    ClaudeCode,
    CodexCli,
    Zcode,
    WorkBuddy,
    CodeBuddy,
    Grok,
    Kimi,
    Qoder,
    QoderCn,
    Hermes,
    Trae,
    StatusOnly,
    Unsupported,
}

impl AdapterKind {
    /// 是否存在可解析的 token 记录
    pub fn has_token_source(self) -> bool {
        matches!(
            self,
            Self::ClaudeCode
                | Self::CodexCli
                | Self::Zcode
                | Self::WorkBuddy
                | Self::CodeBuddy
                | Self::Grok
                | Self::Kimi
                | Self::Qoder
                | Self::Hermes
        )
    }

    /// 是否能从本地记录判断工作中/需要确认/停止
    pub fn has_status_source(self) -> bool {
        !matches!(self, Self::Unsupported | Self::StatusOnly)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct ToolId(&'static str);

impl ToolId {
    pub const CLAUDE_CODE: Self = Self("claudeCode");
    pub const CODEX_CLI: Self = Self("codexCli");
    pub const ZCODE: Self = Self("zcode");
    pub const WORK_BUDDY: Self = Self("workBuddy");
    pub const CODE_BUDDY: Self = Self("codeBuddy");
    pub const TRAE: Self = Self("trae");
    pub const TRAE_CN: Self = Self("traeCn");
    pub const QODER: Self = Self("qoder");
    pub const QODER_CN: Self = Self("qoderCn");
    pub const DEEPSEEK_HARNESS: Self = Self("deepseekHarness");
    pub const GROK: Self = Self("grok");
    pub const OPEN_CLAW: Self = Self("openClaw");
    pub const HERMES: Self = Self("hermes");
    pub const GEMINI_CLI: Self = Self("geminiCli");
    pub const KIMI_CODE: Self = Self("kimiCode");
    pub const CHATGPT: Self = Self("chatgpt");

    pub const ALL: [Self; 16] = [
        Self::CLAUDE_CODE,
        Self::CODEX_CLI,
        Self::ZCODE,
        Self::WORK_BUDDY,
        Self::CODE_BUDDY,
        Self::TRAE,
        Self::TRAE_CN,
        Self::QODER,
        Self::QODER_CN,
        Self::GROK,
        Self::OPEN_CLAW,
        Self::HERMES,
        Self::GEMINI_CLI,
        Self::KIMI_CODE,
        Self::CHATGPT,
        Self::DEEPSEEK_HARNESS,
    ];

    pub fn id(self) -> &'static str {
        self.0
    }

    pub fn descriptor(self) -> &'static ToolDescriptor {
        TOOLS
            .iter()
            .find(|descriptor| descriptor.id == self)
            .unwrap_or(&UNKNOWN_TOOL)
    }

    pub fn display_name(self) -> &'static str {
        self.descriptor().display_name
    }

    pub fn short_name(self) -> &'static str {
        self.descriptor().short_name
    }

    pub fn accent(self) -> &'static str {
        self.descriptor().accent
    }

    pub fn adapter(self) -> AdapterKind {
        self.descriptor().adapter
    }

    pub fn is_supported(self) -> bool {
        self.adapter().has_token_source()
    }

    pub fn supports_status(self) -> bool {
        self.adapter().has_status_source()
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tool| tool.0 == id)
    }
}

impl std::fmt::Display for ToolId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

#[derive(Debug)]
pub struct ToolDescriptor {
    pub id: ToolId,
    pub display_name: &'static str,
    pub short_name: &'static str,
    pub accent: &'static str,
    /// 小写进程名或路径片段，用于判断工具是否正在运行
    pub process_keywords: &'static [&'static str],
    /// PATH 中的可执行文件名
    pub command_names: &'static [&'static str],
    /// macOS 应用包名称
    pub app_bundles: &'static [&'static str],
    /// 相对用户主目录的数据目录，存在即视为安装或使用过
    pub data_dirs: &'static [&'static str],
    pub adapter: AdapterKind,
}

pub static TOOLS: [ToolDescriptor; 16] = [
    ToolDescriptor {
        id: ToolId::CLAUDE_CODE,
        display_name: "Claude Code",
        short_name: "Claude",
        accent: "#e2926d",
        process_keywords: &["claude"],
        command_names: &["claude"],
        app_bundles: &[],
        data_dirs: &[".claude"],
        adapter: AdapterKind::ClaudeCode,
    },
    ToolDescriptor {
        id: ToolId::CODEX_CLI,
        display_name: "Codex CLI",
        short_name: "Codex",
        accent: "#78a9ff",
        process_keywords: &["codex"],
        command_names: &["codex"],
        app_bundles: &[],
        data_dirs: &[".codex"],
        adapter: AdapterKind::CodexCli,
    },
    ToolDescriptor {
        id: ToolId::ZCODE,
        display_name: "ZCode",
        short_name: "ZCode",
        accent: "#22d3ee",
        process_keywords: &["zcode"],
        command_names: &["zcode"],
        app_bundles: &["ZCode.app"],
        data_dirs: &[".zcode"],
        adapter: AdapterKind::Zcode,
    },
    ToolDescriptor {
        id: ToolId::WORK_BUDDY,
        display_name: "WorkBuddy",
        short_name: "WorkBuddy",
        accent: "#34d399",
        process_keywords: &["workbuddy"],
        command_names: &["workbuddy"],
        app_bundles: &["WorkBuddy.app", "WorkBuddy AI.app"],
        data_dirs: &[".workbuddy"],
        adapter: AdapterKind::WorkBuddy,
    },
    ToolDescriptor {
        id: ToolId::CODE_BUDDY,
        display_name: "CodeBuddy",
        short_name: "CodeBuddy",
        accent: "#f59e0b",
        process_keywords: &["codebuddy"],
        command_names: &["codebuddy"],
        app_bundles: &["CodeBuddy.app", "CodeBuddy CN.app"],
        data_dirs: &[".codebuddy"],
        adapter: AdapterKind::CodeBuddy,
    },
    ToolDescriptor {
        id: ToolId::TRAE,
        display_name: "Trae",
        short_name: "Trae",
        accent: "#60a5fa",
        process_keywords: &["trae"],
        command_names: &["trae"],
        app_bundles: &["Trae.app"],
        data_dirs: &[".trae"],
        adapter: AdapterKind::Trae,
    },
    ToolDescriptor {
        id: ToolId::TRAE_CN,
        display_name: "Trae CN",
        short_name: "Trae CN",
        accent: "#3b82f6",
        process_keywords: &["trae cn"],
        command_names: &["trae"],
        app_bundles: &["Trae CN.app"],
        data_dirs: &[".trae-cn"],
        adapter: AdapterKind::Trae,
    },
    ToolDescriptor {
        id: ToolId::QODER,
        display_name: "Qoder",
        short_name: "Qoder",
        accent: "#c084fc",
        process_keywords: &["qoder"],
        command_names: &["qoder"],
        app_bundles: &["Qoder.app", "Qoder IDE.app"],
        data_dirs: &[".qoder"],
        adapter: AdapterKind::Qoder,
    },
    ToolDescriptor {
        id: ToolId::QODER_CN,
        display_name: "Qoder CN",
        short_name: "Qoder CN",
        accent: "#a78bfa",
        process_keywords: &["qoder cn", "qodercn"],
        command_names: &["qoder"],
        app_bundles: &["Qoder CN.app"],
        data_dirs: &[".qoder-cn"],
        adapter: AdapterKind::QoderCn,
    },
    ToolDescriptor {
        id: ToolId::GROK,
        display_name: "Grok",
        short_name: "Grok",
        accent: "#94a3b8",
        process_keywords: &["grok"],
        command_names: &["grok"],
        app_bundles: &[],
        data_dirs: &[".grok"],
        adapter: AdapterKind::Grok,
    },
    ToolDescriptor {
        id: ToolId::OPEN_CLAW,
        display_name: "OpenClaw",
        short_name: "OpenClaw",
        accent: "#fb7185",
        process_keywords: &["openclaw"],
        command_names: &["openclaw"],
        app_bundles: &[],
        data_dirs: &[".openclaw"],
        adapter: AdapterKind::StatusOnly,
    },
    ToolDescriptor {
        id: ToolId::HERMES,
        display_name: "Hermes",
        short_name: "Hermes",
        accent: "#fbbf24",
        process_keywords: &["hermes"],
        command_names: &["hermes"],
        app_bundles: &[],
        data_dirs: &[".hermes"],
        adapter: AdapterKind::Hermes,
    },
    ToolDescriptor {
        id: ToolId::GEMINI_CLI,
        display_name: "Gemini CLI",
        short_name: "Gemini",
        accent: "#818cf8",
        process_keywords: &["gemini"],
        command_names: &["gemini"],
        app_bundles: &[],
        data_dirs: &[".gemini"],
        adapter: AdapterKind::StatusOnly,
    },
    ToolDescriptor {
        id: ToolId::KIMI_CODE,
        display_name: "Kimi Code",
        short_name: "Kimi",
        accent: "#f472b6",
        process_keywords: &["kimi"],
        command_names: &["kimi"],
        app_bundles: &[],
        data_dirs: &[".kimi-code"],
        adapter: AdapterKind::Kimi,
    },
    ToolDescriptor {
        id: ToolId::CHATGPT,
        display_name: "ChatGPT",
        short_name: "ChatGPT",
        accent: "#10a37f",
        process_keywords: &["chatgpt"],
        command_names: &["chatgpt"],
        app_bundles: &["ChatGPT.app"],
        data_dirs: &[],
        adapter: AdapterKind::StatusOnly,
    },
    ToolDescriptor {
        id: ToolId::DEEPSEEK_HARNESS,
        display_name: "DeepSeek Harness",
        short_name: "DeepSeek",
        accent: "#4d6bfe",
        process_keywords: &["deepseek harness", "deepseek"],
        command_names: &["deepseek"],
        app_bundles: &["DeepSeek Harness.app"],
        data_dirs: &[
            "Library/Application Support/@deepseek-ai/dsh-desktop",
            "AppData/Roaming/@deepseek-ai/dsh-desktop",
            ".config/@deepseek-ai/dsh-desktop",
        ],
        adapter: AdapterKind::StatusOnly,
    },
];

static UNKNOWN_TOOL: ToolDescriptor = ToolDescriptor {
    id: ToolId::CHATGPT,
    display_name: "未知工具",
    short_name: "未知",
    accent: "#94a3b8",
    process_keywords: &[],
    command_names: &[],
    app_bundles: &[],
    data_dirs: &[],
    adapter: AdapterKind::Unsupported,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_ids_are_unique_and_ordered() {
        let mut ids = std::collections::HashSet::new();
        for tool in ToolId::ALL {
            assert!(ids.insert(tool), "工具标识重复: {}", tool.id());
            assert_eq!(ToolId::from_id(tool.id()), Some(tool));
        }
        assert_eq!(ids.len(), TOOLS.len());
        assert!(ToolId::ALL.len() <= TOOLS.len());
    }

    #[test]
    fn adapters_expose_token_and_status_capabilities_separately() {
        assert!(ToolId::CLAUDE_CODE.is_supported());
        assert!(ToolId::CODEX_CLI.is_supported());
        assert!(ToolId::ZCODE.is_supported());
        assert!(ToolId::WORK_BUDDY.is_supported());
        assert!(ToolId::CODE_BUDDY.is_supported());
        assert!(ToolId::QODER.is_supported());
        assert!(ToolId::HERMES.is_supported());
        assert!(ToolId::KIMI_CODE.is_supported());
        assert!(ToolId::GROK.is_supported());

        assert!(!ToolId::TRAE.is_supported());
        assert!(ToolId::TRAE.supports_status());
        assert!(!ToolId::TRAE_CN.is_supported());
        assert!(ToolId::TRAE_CN.supports_status());
        assert!(!ToolId::QODER_CN.is_supported());
        assert!(ToolId::QODER_CN.supports_status());
        assert!(!ToolId::OPEN_CLAW.is_supported());
        assert!(!ToolId::OPEN_CLAW.supports_status());
        assert!(!ToolId::CHATGPT.is_supported());
        assert!(!ToolId::DEEPSEEK_HARNESS.is_supported());
        assert!(!ToolId::DEEPSEEK_HARNESS.supports_status());
    }

    #[test]
    fn unknown_id_is_rejected() {
        assert_eq!(ToolId::from_id("unknown"), None);
    }
}
