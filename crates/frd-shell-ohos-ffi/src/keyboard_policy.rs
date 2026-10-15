//! OHOS 应用侧有限实体键 backend；资格不代表设备、布局或锁定态已验证。
use frd_core::{InputEvent, KeyState};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum OhosKeyboardQualification {
    #[default]
    Unavailable,
    ArkUiLimited,
}

// USB HID 中立范围；0x32/0x64 的国际布局和 Pause 尚未获得共同 consumer 资格。
pub(crate) fn qualified_physical_hid(hid: u16) -> bool {
    matches!(hid, 0x04..=0x31 | 0x33..=0x47 | 0x49..=0x63 | 0x65 | 0xe0..=0xe2 | 0xe4..=0xe6)
}

impl OhosKeyboardQualification {
    pub(crate) fn installed(self) -> bool {
        self == Self::ArkUiLimited
    }
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::ArkUiLimited => "arkui_limited",
        }
    }
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::Unavailable => "未安装实体键 backend",
            Self::ArkUiLimited => "ArkUI 有限实体键 backend；设备、布局和锁定态待实测",
        }
    }
    pub(crate) fn configure(self, controller: &mut frd_app::AppController) {
        controller.set_platform_capabilities(frd_platform_api::PlatformCapabilities {
            physical_key_input: self.installed(),
            ..Default::default()
        });
        controller.set_product_policy(frd_app::ProductPolicy {
            physical_key_input: self.installed(),
            ..Default::default()
        });
    }
    // Up 不以当前 backend/资格拒绝：Owner 必须先清理真实来源 ledger。
    pub(crate) fn validate_new_down(self, event: &InputEvent) -> Result<(), i32> {
        if let InputEvent::PhysicalKey {
            code,
            state: KeyState::Pressed,
            ..
        } = event
        {
            if !self.installed() {
                return Err(crate::UNSUPPORTED_PLATFORM);
            }
            if !qualified_physical_hid(code.usb_hid_usage()) {
                return Err(crate::INVALID_ARGUMENT);
            }
        }
        Ok(())
    }
}
