use crate::{
    api_client::CodSpeedAPIClient,
    auth,
    config::CodSpeedConfig,
    local_logger::{CODSPEED_U8_COLOR_CODE, init_local_logger},
    prelude::*,
    run, setup,
};
use clap::{
    Parser, Subcommand,
    builder::{Styles, styling},
};

fn create_styles() -> Styles {
    styling::Styles::styled()
        .header(styling::AnsiColor::Green.on_default() | styling::Effects::BOLD)
        .usage(styling::AnsiColor::Green.on_default() | styling::Effects::BOLD)
        .literal(
            styling::Ansi256Color(CODSPEED_U8_COLOR_CODE).on_default() | styling::Effects::BOLD,
        )
        .placeholder(styling::AnsiColor::Cyan.on_default())
}

#[derive(Parser, Debug)]
#[command(version, about = "The CodSpeed CLI tool", styles = create_styles())]
pub struct Cli {
    /// The URL of the CodSpeed GraphQL API
    #[arg(
        long,
        env = "CODSPEED_API_URL",
        global = true,
        hide = true,
        default_value = "https://gql.codspeed.io/"
    )]
    pub api_url: String,

    /// The OAuth token to use for all requests
    #[arg(long, env = "CODSPEED_OAUTH_TOKEN", global = true, hide = true)]
    pub oauth_token: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run the bench command and upload the results to CodSpeed
    Run(run::RunArgs),
    /// Ingest existing Criterion output and produce a CodSpeed profile folder
    IngestCriterion(run::ingest::IngestArgs),
    /// Manage the CLI authentication state
    Auth(auth::AuthArgs),
    /// Pre-install the codspeed executors
    Setup,
}

pub async fn run() -> Result<()> {
    let cli = Cli::parse();
    let codspeed_config = CodSpeedConfig::load_with_override(cli.oauth_token.as_deref())?;
    let api_client = CodSpeedAPIClient::try_from((&cli, &codspeed_config))?;

    match cli.command {
        Commands::Run(args) => run::run(args, &api_client, &codspeed_config).await?,
        Commands::IngestCriterion(args) => {
            if !args.upload {
                init_local_logger()?;
            }

            // ingest and optionally upload the produced profile folder
            let profile_folder = run::ingest::ingest_criterion(args.clone()).await?;
            if args.upload {
                // Reuse the existing `run` upload flow: construct RunArgs that skip running and point to the profile folder
                let run_args = run::RunArgs {
                    upload_url: None,
                    token: None,
                    repository: None,
                    provider: None,
                    working_directory: None,
                    mode: run::RunnerMode::Walltime,
                    instruments: vec![],
                    mongo_uri_env_name: None,
                    profile_folder: Some(profile_folder),
                    message_format: None,
                    skip_upload: false,
                    skip_run: true,
                    skip_setup: true,
                    perf_run_args: run::PerfRunArgs::new(false, None),
                    command: vec![],
                };

                // Run the uploader path (this will call uploader::upload internally)
                run::run(run_args, &api_client, &codspeed_config).await?;
            }
        }
        Commands::Auth(args) => {
            init_local_logger()?;
            auth::run(args, &api_client).await?;
        }
        Commands::Setup => {
            init_local_logger()?;
            setup::setup().await?;
        }
    }
    Ok(())
}
