//! 与协议和桌面平台无关的会话标题栏语义。

/// 会话连接状态图标。每个变体必须由 UI 映射为不同轮廓，而不只依赖颜色。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionGlyph {
    Connecting,
    WaitingForFrame,
    Connected,
    Disconnecting,
    Failed,
}

/// 占位稳定的能力图标状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapabilityGlyphState {
    Available,
    Unavailable,
}

/// 浮动控制岛允许发出的协议无关低频用户动作。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IslandAction {
    ShowConnectionDetails,
    CancelConnect,
    Disconnect,
    ToggleRemoteAudio,
    OpenClipboard,
    MinimizeWindow,
    ToggleMaximizeWindow,
    CloseWindow,
    ShowSystemMenu,
}

/// 当前客户端平台 shell 已验证的原生窗口能力。
///
/// 协议和应用控制器不得推断这些值；桌面 shell 从平台适配器取得能力后交给 UI。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IslandWindowCapabilities {
    pub minimize: bool,
    pub maximize: bool,
    pub close: bool,
    pub system_menu: bool,
    pub begin_move: bool,
}

impl IslandWindowCapabilities {
    pub const NONE: Self = Self {
        minimize: false,
        maximize: false,
        close: false,
        system_menu: false,
        begin_move: false,
    };

    pub const WINDOWS: Self = Self {
        minimize: true,
        maximize: true,
        close: true,
        system_menu: true,
        begin_move: true,
    };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionTimingSource {
    FramebufferResponse,
    MediaIngressToPresent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionTiming {
    pub source: SessionTimingSource,
    pub milliseconds: u32,
}

/// 连接开始后标题栏所需的全部协议无关状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionChromeModel {
    pub connection: ConnectionGlyph,
    pub diagnostics: Option<String>,
    pub presentation_timing: Option<SessionTiming>,
    pub audio: CapabilityGlyphState,
    pub clipboard: CapabilityGlyphState,
    pub action: Option<IslandAction>,
}

#[cfg(test)]
mod tests {
    use super::IslandWindowCapabilities;

    #[test]
    fn island_window_capabilities_are_shell_data_not_protocol_data() {
        assert_eq!(
            IslandWindowCapabilities::default(),
            IslandWindowCapabilities::NONE
        );
        assert_eq!(
            IslandWindowCapabilities::WINDOWS,
            IslandWindowCapabilities {
                minimize: true,
                maximize: true,
                close: true,
                system_menu: true,
                begin_move: true,
            }
        );
    }
}
