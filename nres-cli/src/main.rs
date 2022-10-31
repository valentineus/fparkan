extern crate libnres;

use std::io::Write;

use clap::{Parser, Subcommand};
use console::Term;
use miette::{IntoDiagnostic, Result};

use libnres::reader;

#[derive(Parser, Debug)]
#[command(name = "NRes CLI")]
#[command(about, author, version, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
    /// Print debugging information
    #[arg(short, long, default_value_t = false)]
    debug: bool,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Extract files or a file from the "NRes" file
    #[command(arg_required_else_help = true)]
    Extract {
        /// "NRes" file
        file: String,
        /// Overwrite files
        #[arg(short, long, value_name = "TRUE|FALSE")]
        force: Option<bool>,
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
    let _stderr = Term::stderr();
    let stdout = Term::stdout();

    let cli = Cli::parse();
    let debug = cli.debug;

    match cli.command {
        //region Command "EXTRACT"
        Commands::Extract { file, force, out } => {
            let file = std::fs::File::open(file).into_diagnostic()?;
            let list = reader::get_list(&file).into_diagnostic()?;

            for element in list {
                let path = out.to_string()
                    + "/"
                    + &element.name.to_string()
                    + "."
                    + &element.extension.to_string();

                let mut output = std::fs::File::create(path).into_diagnostic()?;
                let mut buffer = reader::get_file(&file, &element).into_diagnostic()?;
                output.write_all(&mut buffer).into_diagnostic()?;
                buffer.clear();
            }
        } //endregion

        //region Command "LS"
        Commands::Ls { file } => {
            let file = std::fs::File::open(file).into_diagnostic()?;
            let list = reader::get_list(&file).into_diagnostic()?;

            for element in list {
                stdout.write_line(&element.name).into_diagnostic()?;
            }
        } //endregion
    }

    Ok(())
}
