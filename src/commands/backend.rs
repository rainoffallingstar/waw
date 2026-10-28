//! `backends` and `backend <action>`: inspect and edit backend selection.
//!
//! These are the only commands that persist config, so they take the loaded
//! config mutably and save it themselves.

use super::super::*;

pub(crate) fn handle_backend_command(
    command: &Subcommand,
    loaded_config: &mut LoadedConfig,
    dry_run: bool,
    json: bool,
    assume_yes: bool,
    auto_elevate: bool,
) -> Result<ExitCode, String> {
    match command {
        Subcommand::Backends
        | Subcommand::Backend {
            action: BackendAction::List,
        } => {
            if json {
                println!("{}", render_backend_statuses_json(&loaded_config.config));
            } else {
                print_backend_statuses(&loaded_config.config);
            }
            Ok(ExitCode::SUCCESS)
        }
        Subcommand::Backend {
            action: BackendAction::Enable { backend },
        } => {
            let mut preview = loaded_config.config.clone();
            if dry_run {
                if !json {
                    println!("dry-run: would enable backend: {backend}");
                }
            } else {
                loaded_config.config.set_backend_enabled(*backend, true);
                loaded_config.save()?;
                if !json {
                    println!("Enabled backend: {backend}");
                }
            }
            preview.set_backend_enabled(*backend, true);
            if json {
                println!("{}", render_backend_status_json(*backend, &preview));
            } else {
                print_backend_status(*backend, &preview);
            }
            Ok(ExitCode::SUCCESS)
        }
        Subcommand::Backend {
            action: BackendAction::Disable { backend },
        } => {
            let mut preview = loaded_config.config.clone();
            if dry_run {
                if !json {
                    println!("dry-run: would disable backend: {backend}");
                }
            } else {
                loaded_config.config.set_backend_enabled(*backend, false);
                loaded_config.save()?;
                if !json {
                    println!("Disabled backend: {backend}");
                }
            }
            preview.set_backend_enabled(*backend, false);
            if json {
                println!("{}", render_backend_status_json(*backend, &preview));
            } else {
                print_backend_status(*backend, &preview);
            }
            Ok(ExitCode::SUCCESS)
        }
        Subcommand::Backend {
            action: BackendAction::Install { backend, enable },
        } => {
            if !backend.supported_on_host() {
                if json {
                    println!(
                        "{}",
                        render_backend_install_json(
                            *backend,
                            false,
                            *enable,
                            dry_run,
                            "unsupported",
                            None
                        )
                    );
                } else {
                    println!("Backend {backend} is not supported on this host.");
                    println!("Hint: {}", backend.install_hint());
                }
                return Ok(ExitCode::SUCCESS);
            }

            if !backend.supports_bootstrap_install_on_host() {
                if json {
                    println!(
                        "{}",
                        render_backend_install_json(
                            *backend,
                            false,
                            *enable,
                            dry_run,
                            "bootstrap_unsupported",
                            None
                        )
                    );
                } else {
                    println!(
                        "Automatic bootstrap install for {backend} is not supported on this host."
                    );
                    println!("Hint: {}", backend.install_hint());
                }
                return Ok(ExitCode::SUCCESS);
            }

            if backend.is_available() {
                if json {
                    println!(
                        "{}",
                        render_backend_install_json(
                            *backend,
                            true,
                            *enable,
                            dry_run,
                            "already_available",
                            None
                        )
                    );
                } else {
                    println!("Backend {backend} is already available.");
                }
            } else if let Some(invocation) = backend.install_invocation(assume_yes) {
                if !json {
                    println!("Bootstrap command for {backend}:");
                }
                execute_invocations(
                    vec![invocation],
                    dry_run,
                    false,
                    &RuntimeSettings {
                        assume_yes,
                        auto_elevate,
                        command_timeout: timeout_from_seconds(
                            loaded_config.config.command_timeout_seconds,
                        ),
                        deep_search: false,
                        config: &loaded_config.config,
                    },
                )?;
                if json {
                    println!(
                        "{}",
                        render_backend_install_json(
                            *backend,
                            false,
                            *enable,
                            dry_run,
                            "bootstrap_requested",
                            backend.install_invocation(assume_yes).as_ref()
                        )
                    );
                }
            } else if json {
                println!(
                    "{}",
                    render_backend_install_json(
                        *backend,
                        false,
                        *enable,
                        dry_run,
                        "no_bootstrap",
                        None
                    )
                );
            } else {
                println!("Automatic install is not implemented for {backend}.");
                println!("Hint: {}", backend.install_hint());
            }

            if *enable {
                if dry_run {
                    if !json {
                        println!("dry-run: would enable backend after install request: {backend}");
                    }
                } else {
                    loaded_config.config.set_backend_enabled(*backend, true);
                    loaded_config.save()?;
                    if !json {
                        println!("Enabled backend after install request: {backend}");
                    }
                }
            }

            Ok(ExitCode::SUCCESS)
        }
        Subcommand::Backend {
            action: BackendAction::Default { backend },
        } => {
            let mut preview = loaded_config.config.clone();
            match backend {
                Some(backend) => {
                    if !preview.is_backend_enabled(*backend) {
                        preview.set_backend_enabled(*backend, true);
                    }
                    preview.backend = Some(*backend);
                    if dry_run {
                        if !json {
                            println!("dry-run: would set default backend: {backend}");
                        }
                    } else {
                        if !loaded_config.config.is_backend_enabled(*backend) {
                            loaded_config.config.set_backend_enabled(*backend, true);
                        }
                        loaded_config.config.backend = Some(*backend);
                        loaded_config.save()?;
                        if !json {
                            println!("Set default backend: {backend}");
                        }
                    }
                }
                None => {
                    preview.backend = None;
                    if dry_run {
                        if !json {
                            println!(
                                "dry-run: would clear explicit default backend and return to auto detection"
                            );
                        }
                    } else {
                        loaded_config.config.backend = None;
                        loaded_config.save()?;
                        if !json {
                            println!("Cleared explicit default backend. Auto detection is active.");
                        }
                    }
                }
            }
            if json {
                println!("{}", render_backend_statuses_json(&preview));
            } else {
                print_backend_statuses(&preview);
            }

            Ok(ExitCode::SUCCESS)
        }
        _ => Ok(ExitCode::SUCCESS),
    }
}
