mod cli;

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use frd_app::{AppLaunch, ProductPolicy};
use frd_media_api::{AudioOutput, AudioOutputError};
use frd_platform_api::PlatformCapabilities;
use frd_platform_windows::{
    DpapiServerIdentityStore, EnvironmentCredentialProvider, WindowsAudioOutput,
    WindowsSingleInstanceGuard,
};
use frd_protocol_api::{ProtocolCatalog, ProtocolFactory};
use frd_protocol_apple::AppleProtocolFactory;
use frd_shell_desktop::{AudioOutputFactory, DesktopApplication, DesktopUserEvent, FatalReport};
use winit::event_loop::{ControlFlow, EventLoop};

use crate::cli::Cli;

struct WindowsAudioFactory;

impl AudioOutputFactory for WindowsAudioFactory {
    fn open(&self) -> Result<Box<dyn AudioOutput>, AudioOutputError> {
        WindowsAudioOutput::open_default().map(|output| Box::new(output) as Box<dyn AudioOutput>)
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let _single_instance =
        WindowsSingleInstanceGuard::acquire_for_product("freeremotedesk-windows-product")
            .context("无法取得 FreeRemoteDesk 单实例锁")?;
    let event_loop = EventLoop::<DesktopUserEvent>::with_user_event()
        .build()
        .context("无法创建 Windows 事件循环")?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();

    if let Some(options) = cli.test_texture_options() {
        let mut application = DesktopApplication::new_test_texture(proxy, options);
        let run_result = event_loop.run_app(&mut application);
        return finish_run_result(
            run_result,
            application.runner_result(),
            "离线测试纹理事件循环失败",
        );
    }

    let factory = Arc::new(AppleProtocolFactory) as Arc<dyn ProtocolFactory>;
    let catalog = ProtocolCatalog::new([factory.descriptor().id]);
    let provider = EnvironmentCredentialProvider;
    let launch_options = cli
        .launch_options()
        .map_err(anyhow::Error::msg)
        .context("命令行连接预填参数无效")?;
    let mut launch = AppLaunch::new(launch_options, &provider, &catalog);
    launch
        .controller_mut()
        .set_platform_capabilities(PlatformCapabilities {
            dynamic_resolution: true,
            clipboard_read: false,
            clipboard_write: false,
            remote_audio: true,
            text_input: true,
        });
    launch.controller_mut().set_product_policy(ProductPolicy {
        dynamic_resolution: true,
        clipboard_read: false,
        clipboard_write: false,
        remote_audio: true,
        text_input: true,
    });
    let store = Arc::new(
        DpapiServerIdentityStore::current_user_default()
            .map_err(|_| anyhow::anyhow!("无法初始化当前用户服务器身份存储"))?,
    );
    let mut application = DesktopApplication::new_product(
        launch,
        [factory],
        store,
        Arc::new(WindowsAudioFactory),
        proxy,
    );
    let run_result = event_loop.run_app(&mut application);
    finish_run_result(
        run_result,
        application.runner_result(),
        "Windows 客户端事件循环失败",
    )
}

fn finish_run_result<E>(
    event_loop_result: std::result::Result<(), E>,
    application_result: std::result::Result<(), FatalReport>,
    event_loop_context: &'static str,
) -> Result<()>
where
    E: std::error::Error + Send + Sync + 'static,
{
    application_result.map_err(anyhow::Error::new)?;
    event_loop_result.context(event_loop_context)
}

#[cfg(test)]
mod tests {
    use frd_shell_desktop::{FatalComponent, FatalOperation, FatalReason, FatalReport};

    use super::finish_run_result;

    #[test]
    fn latched_fatal_makes_a_successful_event_loop_return_an_error() {
        let fatal = FatalReport::internal(
            FatalComponent::Application,
            FatalOperation::Launch,
            FatalReason::InvalidState,
        );

        let error = finish_run_result(
            Ok::<(), std::io::Error>(()),
            Err(fatal.clone()),
            "event loop failed",
        )
        .expect_err("fatal latch must become a nonzero main result");

        assert_eq!(error.downcast_ref::<FatalReport>(), Some(&fatal));
        assert_eq!(error.to_string(), fatal.to_string());
    }
}
