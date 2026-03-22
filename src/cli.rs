use clap::{Parser, Subcommand};

/// HackArena bootstrap CLI.
#[derive(Parser, Debug)]
#[command(
    name = "hackarena",
    version,
    about = "HackArena installer/bootstrap CLI"
)]
pub struct Cli {
    /// Enable verbose diagnostic output.
    #[arg(long, global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Update installed components in the current project.
    Update {
        #[command(subcommand)]
        component: Option<UpdateSubcommand>,

        /// Do not use cached release metadata (always fetch from network).
        #[arg(long)]
        no_cache: bool,

        /// Allow prerelease versions when stable release is not available.
        #[arg(long)]
        prerelease: bool,
    },

    /// Set active HackArena edition (e.g. `3`, `3.5`) for this project.
    Use { edition: String },

    /// Install components for the active edition.
    Install {
        #[command(subcommand)]
        component: Option<InstallSubcommand>,

        /// Skip installing wrapper.
        #[arg(long)]
        skip_wrapper: bool,

        /// Do not use cached release metadata (always fetch from network).
        #[arg(long)]
        no_cache: bool,

        /// Allow prerelease versions when stable release is not available.
        #[arg(long)]
        prerelease: bool,
    },

    /// Print diagnostics about your setup.
    Doctor {
        /// Do not use cached release metadata (always fetch from network).
        #[arg(long)]
        no_cache: bool,

        /// Allow prerelease versions during latest-version checks.
        #[arg(long)]
        prerelease: bool,
    },

    /// Print active edition, resolved URLs, and paths.
    Status {
        /// Do not use cached release metadata (always fetch from network).
        #[arg(long)]
        no_cache: bool,

        /// Allow prerelease versions during latest-version checks.
        #[arg(long)]
        prerelease: bool,
    },

    /// Run global ha-auth without requiring PATH setup.
    Auth {
        /// Arguments forwarded to ha-auth.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Remove downloaded/installed artifacts (interactive by default).
    Clean {
        /// Remove everything (project + global Paths dirs).
        #[arg(long)]
        all: bool,

        /// Remove project-local files (./backend, ./wrappers, ./.hackarena/*).
        #[arg(long)]
        project: bool,

        /// Remove global files (LocalAppData/AppData under HackArena Paths).
        #[arg(long)]
        global: bool,

        /// Delete everything without asking.
        #[arg(long)]
        force: bool,

        /// Delete only items not modified since install time.
        #[arg(long)]
        save: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum InstallSubcommand {
    /// Install only ha-auth.
    Auth {
        /// Do not use cached release metadata (always fetch from network).
        #[arg(long)]
        no_cache: bool,

        /// Allow prerelease versions when stable release is not available.
        #[arg(long)]
        prerelease: bool,
    },

    /// Install only backend bundle.
    Backend {
        /// Do not use cached release metadata (always fetch from network).
        #[arg(long)]
        no_cache: bool,

        /// Allow prerelease versions when stable release is not available.
        #[arg(long)]
        prerelease: bool,
    },

    /// Install a wrapper bundle by id.
    Wrapper {
        wrapper_id: String,

        /// Do not use cached release metadata (always fetch from network).
        #[arg(long)]
        no_cache: bool,

        /// Allow prerelease versions when stable release is not available.
        #[arg(long)]
        prerelease: bool,

        /// Install from a specific GitHub release tag (e.g. `v0.1.0b1`).
        #[arg(long)]
        tag: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum UpdateSubcommand {
    /// Update only ha-auth.
    Auth {
        /// Do not use cached release metadata (always fetch from network).
        #[arg(long)]
        no_cache: bool,

        /// Allow prerelease versions when stable release is not available.
        #[arg(long)]
        prerelease: bool,
    },

    /// Update only backend.
    Backend {
        /// Do not use cached release metadata (always fetch from network).
        #[arg(long)]
        no_cache: bool,

        /// Allow prerelease versions when stable release is not available.
        #[arg(long)]
        prerelease: bool,
    },

    /// Update an installed wrapper by id.
    Wrapper {
        wrapper_id: String,

        /// Do not use cached release metadata (always fetch from network).
        #[arg(long)]
        no_cache: bool,

        /// Allow prerelease versions when stable release is not available.
        #[arg(long)]
        prerelease: bool,

        /// Update to a specific GitHub release tag (e.g. `v0.1.0b1`).
        #[arg(long)]
        tag: Option<String>,
    },
}
