mod archive;
mod auth_cmd;
mod clean;
mod cli;
mod cmd_hint;
mod config;
mod constants;
mod doctor;
mod download;
mod error;
mod github_releases;
mod install;
mod submission_proto;
mod submit;

use clap::Parser;
use cli::{Cli, Command, InstallSubcommand, LinuxLibcArg, UpdateSubcommand};
use config::Paths;
use error::HackArenaError;
use github_releases::LinuxLibcMode;

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), HackArenaError> {
    let cli = Cli::parse();
    let paths = Paths::discover()?;

    cli.command.execute(&paths, cli.verbose).await?;
    Ok(())
}

impl Command {
    async fn execute(&self, paths: &Paths, verbose: bool) -> Result<(), HackArenaError> {
        match self {
            Command::Update {
                component,
                no_cache,
                prerelease,
                linux_libc,
            } => match component {
                None => {
                    install::update(paths, *no_cache, *prerelease, linux_libc.map(Into::into))
                        .await?
                }
                Some(UpdateSubcommand::Auth {
                    no_cache: sub_no_cache,
                    prerelease: sub_prerelease,
                }) => {
                    install::update_auth(
                        paths,
                        *no_cache || *sub_no_cache,
                        *prerelease || *sub_prerelease,
                        linux_libc.map(Into::into),
                    )
                    .await?
                }
                Some(UpdateSubcommand::Backend {
                    no_cache: sub_no_cache,
                    prerelease: sub_prerelease,
                }) => {
                    install::update_backend(
                        paths,
                        *no_cache || *sub_no_cache,
                        *prerelease || *sub_prerelease,
                        linux_libc.map(Into::into),
                    )
                    .await?
                }
                Some(UpdateSubcommand::Wrapper {
                    wrapper_id,
                    no_cache: sub_no_cache,
                    prerelease: sub_prerelease,
                    tag,
                }) => {
                    install::update_wrapper(
                        paths,
                        wrapper_id,
                        *no_cache || *sub_no_cache,
                        *prerelease || *sub_prerelease,
                        tag.as_deref(),
                        linux_libc.map(Into::into),
                    )
                    .await?
                }
            },
            Command::Use { edition } => install::use_edition(paths, edition).await?,
            Command::Install {
                component,
                skip_wrapper,
                no_cache,
                prerelease,
                linux_libc,
            } => match component {
                None => {
                    install::install(
                        paths,
                        *skip_wrapper,
                        *no_cache,
                        *prerelease,
                        linux_libc.map(Into::into),
                    )
                    .await?
                }
                Some(InstallSubcommand::Auth {
                    no_cache: sub_no_cache,
                    prerelease: sub_prerelease,
                }) => {
                    install::install_auth(
                        paths,
                        *no_cache || *sub_no_cache,
                        *prerelease || *sub_prerelease,
                        linux_libc.map(Into::into),
                    )
                    .await?
                }
                Some(InstallSubcommand::Backend {
                    no_cache: sub_no_cache,
                    prerelease: sub_prerelease,
                }) => {
                    install::install_backend(
                        paths,
                        *no_cache || *sub_no_cache,
                        *prerelease || *sub_prerelease,
                        linux_libc.map(Into::into),
                    )
                    .await?
                }
                Some(InstallSubcommand::Wrapper {
                    wrapper_id,
                    no_cache: sub_no_cache,
                    prerelease: sub_prerelease,
                    tag,
                }) => {
                    install::install_wrapper(
                        paths,
                        wrapper_id.as_deref(),
                        *no_cache || *sub_no_cache,
                        *prerelease || *sub_prerelease,
                        tag.as_deref(),
                        linux_libc.map(Into::into),
                    )
                    .await?
                }
            },
            Command::Doctor {
                no_cache,
                prerelease,
            } => doctor::doctor(paths, *no_cache, *prerelease, verbose).await?,
            Command::Status {
                no_cache,
                prerelease,
            } => doctor::status(paths, *no_cache, *prerelease, verbose).await?,
            Command::Auth { args } => auth_cmd::run_auth(paths, args)?,
            Command::Submit { slot, description } => {
                submit::submit(paths, *slot, description.as_deref()).await?
            }
            Command::Clean {
                all,
                project,
                global,
                force,
                save,
            } => clean::clean(paths, *all, *project, *global, *force, *save).await?,
        }
        Ok(())
    }
}

impl From<LinuxLibcArg> for LinuxLibcMode {
    fn from(value: LinuxLibcArg) -> Self {
        match value {
            LinuxLibcArg::Auto => LinuxLibcMode::Auto,
            LinuxLibcArg::Gnu => LinuxLibcMode::Gnu,
            LinuxLibcArg::Musl => LinuxLibcMode::Musl,
        }
    }
}
