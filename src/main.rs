use anyhow::Result;
use clap::Parser;
use herdr_agent_quota::cli::{Cli, Command};

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Refresh {
            provider,
            force,
            json,
        } => herdr_agent_quota::refresh::run(&provider.providers(), force, json),
        Command::Watch {
            provider,
            interval_seconds,
        } => herdr_agent_quota::refresh::watch(&provider.providers(), interval_seconds),
        Command::Startup { provider } => herdr_agent_quota::refresh::startup(&provider.providers()),
        Command::Event => herdr_agent_quota::refresh::event(),
        Command::Focus => herdr_agent_quota::refresh::focus(),
        Command::Dashboard => herdr_agent_quota::dashboard::run(),
        Command::Settings => herdr_agent_quota::settings::run(),
        Command::Configure {
            check,
            apply,
            uninstall,
            agent,
            watch_interval_seconds,
            sidebar_layout,
            quota_percent,
            row_gap,
            fields,
            brand_colors,
            agent_order,
            low_quota_alert,
        } => {
            let result = herdr_agent_quota::configure::run(
                check,
                apply,
                uninstall,
                &herdr_agent_quota::cli::AgentSelection::from_args_or_env(&agent),
                herdr_agent_quota::cli::ConfigureOptions {
                    watch_interval_seconds,
                    sidebar_layout,
                    quota_percent,
                    row_gap,
                    fields,
                    brand_colors,
                    agent_order,
                    low_quota_alert,
                },
            );
            // The plugin's configure action used to chain
            // `&& herdr server reload-config` through a shell. The command
            // is platform-neutral argv now, so the reload lives here and
            // only fires under a Herdr-provided binary.
            if result.is_ok() && (apply || uninstall) {
                herdr_agent_quota::herdr::reload_server_config()?;
            }
            result
        }
        Command::OpenSettings => herdr_agent_quota::herdr::open_settings_pane(),
        Command::ClaudeStatusline => herdr_agent_quota::configure::claude::run_statusline_hook(),
        Command::AgyStatusline => herdr_agent_quota::configure::agy::run_statusline_hook(),
    }
}
