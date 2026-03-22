mod archive;
mod auth_cmd;
mod clean;
mod cli;
mod config;
mod constants;
mod doctor;
mod download;
mod error;
mod github_releases;
mod install;

use clap::Parser;
use cli::{Cli, Command, InstallSubcommand, UpdateSubcommand};
use config::Paths;
use error::HackArenaError;

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
            } => match component {
                None => install::update(paths, *no_cache, *prerelease).await?,
                Some(UpdateSubcommand::Auth {
                    no_cache: sub_no_cache,
                    prerelease: sub_prerelease,
                }) => {
                    install::update_auth(
                        paths,
                        *no_cache || *sub_no_cache,
                        *prerelease || *sub_prerelease,
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
            } => match component {
                None => install::install(paths, *skip_wrapper, *no_cache, *prerelease).await?,
                Some(InstallSubcommand::Auth {
                    no_cache: sub_no_cache,
                    prerelease: sub_prerelease,
                }) => {
                    install::install_auth(
                        paths,
                        *no_cache || *sub_no_cache,
                        *prerelease || *sub_prerelease,
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
                        wrapper_id,
                        *no_cache || *sub_no_cache,
                        *prerelease || *sub_prerelease,
                        tag.as_deref(),
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
