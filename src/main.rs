use anyhow::Result;
use clap::Parser;
use herdr_agent_quota::cli::{Cli, Command};

fn main() -> Result<()> {
    herdr_agent_quota::identity::adopt_alias_plugin_dirs();
    let cli = Cli::parse();
    match cli.command {
        Command::Refresh {
            provider,
            force,
            json,
            keychain_approve,
        } => {
            if keychain_approve {
                let providers = provider.providers();
                let muse = providers.contains(&herdr_agent_quota::model::Provider::Muse);
                let cursor = providers.contains(&herdr_agent_quota::model::Provider::Cursor);
                if !muse && !cursor {
                    anyhow::bail!(
                        "--keychain-approve only applies to muse or cursor; run `refresh --provider cursor --keychain-approve`"
                    );
                }
                if muse {
                    herdr_agent_quota::providers::muse::set_keychain_approve_attempt();
                }
                if cursor {
                    herdr_agent_quota::providers::cursor::set_keychain_approve_attempt();
                }
                return herdr_agent_quota::refresh::run(&providers, force, json);
            }
            herdr_agent_quota::refresh::run(&provider.providers(), force, json)
        }
        Command::Watch {
            provider,
            interval_seconds,
            defer,
        } => herdr_agent_quota::refresh::watch(&provider.providers(), interval_seconds, defer),
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
            // The plugin's configure/uninstall actions used to chain
            // `&& herdr server reload-config [&& ... startup]` through `sh`.
            // The manifest is platform-neutral argv now, so the follow-ups
            // live here and only fire under a Herdr-provided binary.
            if result.is_ok() && (apply || uninstall) {
                herdr_agent_quota::herdr::reload_server_config()?;
                if apply && std::env::var_os("HERDR_PLUGIN_STATE_DIR").is_some() {
                    herdr_agent_quota::refresh::startup(
                        &herdr_agent_quota::cli::ProviderSelection::All.providers(),
                    )?;
                }
            }
            result
        }
        Command::OpenSettings => herdr_agent_quota::herdr::open_settings_pane(),
        Command::ClaudeStatusline { state_dir } => {
            use_state_dir(state_dir);
            herdr_agent_quota::configure::claude::run_statusline_hook()
        }
        Command::AgyStatusline { state_dir } => {
            use_state_dir(state_dir);
            herdr_agent_quota::configure::agy::run_statusline_hook()
        }
        Command::CursorHooks => herdr_agent_quota::configure::cursor::run_hook(),
    }
}

/// Make an explicit `--state-dir` the cache root for this process.
///
/// `CacheStore::from_env` and the chained collector both read the variable,
/// so setting it once here keeps every consumer on the same directory. Runs
/// before any thread is spawned.
fn use_state_dir(state_dir: Option<std::path::PathBuf>) {
    if let Some(state_dir) = state_dir.filter(|dir| !dir.as_os_str().is_empty()) {
        std::env::set_var("HERDR_PLUGIN_STATE_DIR", state_dir);
    }
}
