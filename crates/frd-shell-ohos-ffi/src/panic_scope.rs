//! 一次安装的 hook 仅在桥的线程局部受控范围隐藏 payload；范围外委托原 hook。
//! 不临时替换 hook，因此并发 ABI 调用没有反复 take/set 的竞态。桌面应用
//! 不依赖本 crate；即使宿主显式调用此桥，非桥 panic 仍保持其原始 hook 行为。
use std::{cell::Cell, io::Write, sync::Once};

thread_local! {
    static CONTROLLED: Cell<bool> = const { Cell::new(false) };
}

pub(crate) struct Scope {
    previous: bool,
}
impl Scope {
    pub(crate) fn enter() -> Self {
        static INSTALL: Once = Once::new();
        INSTALL.call_once(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                if CONTROLLED.try_with(Cell::get).unwrap_or(false) {
                    // 绝不格式化 payload、位置、线程名称或 PanicHookInfo；输出失败
                    // 也不能在 hook 中再次 panic。固定诊断只说明受控错误发生。
                    let _ = std::io::stderr()
                        .write_all("FreeRemoteDesk OHOS: 已捕获内部错误（panic）\n".as_bytes());
                } else {
                    previous(info);
                }
            }));
        });
        let previous = CONTROLLED.with(|controlled| controlled.replace(true));
        Self { previous }
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        let _ = CONTROLLED.try_with(|controlled| controlled.set(self.previous));
    }
}
