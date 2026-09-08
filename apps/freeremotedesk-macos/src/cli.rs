use std::time::Duration;

use clap::{Parser, ValueEnum};
use frd_core::{CredentialProviderId, ProtocolId, TargetSystem};
use frd_shell_desktop::TestTextureOptions;
use frd_ui_model::LaunchOptions;

#[derive(Debug, Parser)]
#[command(
    name = "freeremotedesk-macos",
    about = "FreeRemoteDesk macOS 单窗口客户端"
)]
pub(crate) struct Cli {
    #[arg(long, value_enum)]
    target: Option<TargetArgument>,
    #[arg(long)]
    address: Option<String>,
    #[arg(long)]
    port: Option<u16>,
    #[arg(long)]
    protocol: Option<String>,
    #[arg(long)]
    username_provider: Option<String>,
    #[arg(long)]
    password_provider: Option<String>,
    #[arg(long)]
    connect: bool,
    #[arg(
        long,
        value_enum,
        help = "启用默认关闭的 RDP EGFX 验证实验（需要签名 FFmpeg 后端）"
    )]
    pub(crate) rdp_egfx_experiment: Option<RdpEgfxExperiment>,
    #[arg(long, hide = true)]
    test_texture: bool,
    #[arg(long, hide = true)]
    pub(crate) verify_codec_bundle: bool,
    #[arg(long, hide = true, requires = "test_texture")]
    test_texture_exit_after_ms: Option<u64>,
    #[arg(long, hide = true, requires = "test_texture")]
    test_texture_resize_after_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum RdpEgfxExperiment {
    Avc420,
    Avc444,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum TargetArgument {
    Macos,
    Windows,
    Linux,
    Custom,
}

impl Cli {
    pub(crate) fn launch_options(&self) -> Result<LaunchOptions, &'static str> {
        Ok(LaunchOptions {
            target_system: self.target.map(TargetSystem::from),
            address: self.address.clone(),
            port: self.port,
            protocol: self
                .protocol
                .as_ref()
                .map(|value| ProtocolId::new(value.clone()).ok_or("protocol_id_invalid"))
                .transpose()?,
            username_provider: provider_id(self.username_provider.as_ref())?,
            password_provider: provider_id(self.password_provider.as_ref())?,
            connect_when_complete: self.connect,
        })
    }

    pub(crate) fn test_texture_options(&self) -> Option<TestTextureOptions> {
        self.test_texture.then(|| TestTextureOptions {
            exit_after: self.test_texture_exit_after_ms.map(Duration::from_millis),
            resize_after: self.test_texture_resize_after_ms.map(Duration::from_millis),
        })
    }
}

impl From<TargetArgument> for TargetSystem {
    fn from(value: TargetArgument) -> Self {
        match value {
            TargetArgument::Macos => Self::MacOs,
            TargetArgument::Windows => Self::Windows,
            TargetArgument::Linux => Self::Linux,
            TargetArgument::Custom => Self::Custom,
        }
    }
}

fn provider_id(value: Option<&String>) -> Result<Option<CredentialProviderId>, &'static str> {
    value
        .map(|value| {
            CredentialProviderId::new(value.clone()).ok_or("credential_provider_id_invalid")
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use clap::Parser;
    use frd_app::{AppLaunch, AppPage};
    use frd_core::{CredentialProviderId, SecretBuffer, TargetSystem};
    use frd_platform_api::{CredentialProvider, PlatformError};
    use frd_protocol_api::{ProtocolCatalog, ProtocolId};

    use super::Cli;

    struct TestProvider;

    impl CredentialProvider for TestProvider {
        fn load_username(&self, _: &CredentialProviderId) -> Result<String, PlatformError> {
            Ok("test-user".to_owned())
        }

        fn load_password(&self, _: &CredentialProviderId) -> Result<SecretBuffer, PlatformError> {
            Ok(SecretBuffer::new(vec![0x41]))
        }
    }

    fn catalog() -> ProtocolCatalog {
        ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()])
    }

    #[test]
    fn no_arguments_launch_the_single_connection_form() {
        let cli = Cli::try_parse_from(["freeremotedesk-macos"]).unwrap();
        let launch = AppLaunch::new(cli.launch_options().unwrap(), &TestProvider, &catalog());

        assert!(matches!(
            launch.controller().page(),
            AppPage::ConnectionForm(_)
        ));
    }

    #[test]
    fn partial_arguments_prefill_the_same_connection_form() {
        let cli = Cli::try_parse_from([
            "freeremotedesk-macos",
            "--target",
            "macos",
            "--address",
            "mac.invalid",
        ])
        .unwrap();
        let launch = AppLaunch::new(cli.launch_options().unwrap(), &TestProvider, &catalog());
        let AppPage::ConnectionForm(form) = launch.controller().page() else {
            panic!("partial CLI values must stay on the one connection form");
        };

        assert_eq!(form.draft.target_system, Some(TargetSystem::MacOs));
        assert_eq!(form.draft.address, "mac.invalid");
    }

    #[test]
    fn complete_values_without_connect_only_prefill() {
        let cli = complete_cli(false);
        let mut launch = AppLaunch::new(cli.launch_options().unwrap(), &TestProvider, &catalog());

        assert!(launch.take_connect_intent().is_none());
        assert!(matches!(
            launch.controller().page(),
            AppPage::ConnectionForm(_)
        ));
    }

    #[test]
    fn complete_values_with_connect_request_exactly_one_connection() {
        let cli = complete_cli(true);
        let mut launch = AppLaunch::new(cli.launch_options().unwrap(), &TestProvider, &catalog());

        assert!(launch.take_connect_intent().is_some());
        assert!(launch.take_connect_intent().is_none());
    }

    #[test]
    fn windows_rdp_launch_uses_shared_validated_connect_intent() {
        let cli = Cli::try_parse_from([
            "freeremotedesk-macos",
            "--target",
            "windows",
            "--address",
            "windows.invalid",
            "--port",
            "3389",
            "--protocol",
            "rdp",
            "--username-provider",
            "environment",
            "--password-provider",
            "environment",
            "--connect",
        ])
        .unwrap();
        let catalog = ProtocolCatalog::new([ProtocolId::new("rdp").unwrap()]);
        let mut launch = AppLaunch::new(cli.launch_options().unwrap(), &TestProvider, &catalog);
        assert!(launch.take_connect_intent().is_some());
        assert!(launch.take_connect_intent().is_none());
    }

    #[test]
    fn literal_password_option_does_not_exist() {
        let error =
            Cli::try_parse_from(["freeremotedesk-macos", "--password", "must-not-enter-argv"])
                .expect_err("clap must reject every literal password argument");

        assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
    }

    #[test]
    fn offline_test_texture_resize_hook_requires_the_single_test_window() {
        let error = Cli::try_parse_from([
            "freeremotedesk-macos",
            "--test-texture-resize-after-ms",
            "20",
        ])
        .expect_err("resize hook cannot launch outside offline texture mode");
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );

        let cli = Cli::try_parse_from([
            "freeremotedesk-macos",
            "--test-texture",
            "--test-texture-resize-after-ms",
            "20",
            "--test-texture-exit-after-ms",
            "80",
        ])
        .expect("one offline window may own resize and exit deadlines");
        let options = cli
            .test_texture_options()
            .expect("offline mode options exist");
        assert_eq!(options.resize_after, Some(Duration::from_millis(20)));
        assert_eq!(options.exit_after, Some(Duration::from_millis(80)));
    }

    #[test]
    fn egfx_experiment_is_default_off_and_accepts_only_exact_modes() {
        assert!(Cli::try_parse_from(["app"])
            .unwrap()
            .rdp_egfx_experiment
            .is_none());
        for (argument, mode) in [
            ("avc420", super::RdpEgfxExperiment::Avc420),
            ("avc444", super::RdpEgfxExperiment::Avc444),
        ] {
            let cli = Cli::try_parse_from(["app", "--rdp-egfx-experiment", argument]).unwrap();
            assert_eq!(cli.rdp_egfx_experiment, Some(mode));
            assert_eq!(
                cli.launch_options().unwrap(),
                Cli::try_parse_from(["app"])
                    .unwrap()
                    .launch_options()
                    .unwrap()
            );
        }
        for argument in ["hevc", "auto", "legacy"] {
            assert!(Cli::try_parse_from(["app", "--rdp-egfx-experiment", argument]).is_err());
        }
        assert!(Cli::try_parse_from(["app", "--rdp-egfx-experiment"]).is_err());
    }

    fn complete_cli(connect: bool) -> Cli {
        let mut args = vec![
            "freeremotedesk-macos",
            "--target",
            "macos",
            "--address",
            "mac.invalid",
            "--port",
            "5900",
            "--protocol",
            "apple-hpss-mvs",
            "--username-provider",
            "environment",
            "--password-provider",
            "environment",
        ];
        if connect {
            args.push("--connect");
        }
        Cli::try_parse_from(args).unwrap()
    }
}
