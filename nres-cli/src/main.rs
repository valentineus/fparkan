extern crate libnres;

use std::io::Write;

use clap::{Parser, Subcommand};
use miette::{IntoDiagnostic, Result};

use libnres::reader;

#[derive(Parser, Debug)]
#[command(name = "NRes CLI")]
#[command(about, author, version, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Extract files or a file from the "NRes" file
    #[command(arg_required_else_help = true)]
    Extract {
        /// "NRes" file
        file: String,
        /// Overwrite files
        #[arg(short, long, default_value_t = false, value_name = "TRUE|FALSE")]
        force: bool,
        /// Outbound directory
        #[arg(short, long, value_name = "DIR")]
        out: String,
    },
    /// Print a list of files in the "NRes" file
    Ls {
        /// "NRes" file
        file: String,
    },
}

pub fn main() -> Result<()> {
    let stdout = console::Term::stdout();
    let cli = Cli::parse();

    match cli.command {
        Commands::Extract { file, force, out } => command_extract(file, out, force)?,
        Commands::Ls { file } => command_ls(stdout, file)?,
    }

    Ok(())
}

fn command_extract(file: String, out: String, force: bool) -> Result<()> {
    let file = std::fs::File::open(file).into_diagnostic()?;
    let list = reader::get_list(&file).into_diagnostic()?;
    let bar = indicatif::ProgressBar::new(list.len() as u64);

    for element in list {
        let path = format!("{}/{}", out, element.get_filename());

        if force != true && is_exist_file(&path) {
            let message = format!("File \"{}\" exists. Overwrite it?", path);

            if !dialoguer::Confirm::new()
                .with_prompt(message)
                .interact()
                .into_diagnostic()?
            {
                continue;
            }
        }

        let mut output = std::fs::File::create(path).into_diagnostic()?;
        let mut buffer = reader::get_file(&file, &element).into_diagnostic()?;

        output.write_all(&mut buffer).into_diagnostic()?;
        buffer.clear();
        bar.inc(1);
    }

    bar.finish();

    Ok(())
}

fn command_ls(stdout: console::Term, file: String) -> Result<()> {
    let file = std::fs::File::open(file).into_diagnostic()?;
    let list = reader::get_list(&file).into_diagnostic()?;

    for element in list {
        stdout.write_line(&element.name).into_diagnostic()?;
    }

    Ok(())
}

fn is_exist_file(path: &String) -> bool {
    let metadata = std::path::Path::new(path);
    metadata.exists()
}
