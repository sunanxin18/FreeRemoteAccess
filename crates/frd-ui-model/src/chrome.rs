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

/// 标题栏允许发出的低频会话动作。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionChromeAction {
    Cancel,
    Disconnect,
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
    pub action: Option<SessionChromeAction>,
}
