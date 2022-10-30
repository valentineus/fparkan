extern crate libnres;

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
        /// Name of the packed file to extract
        #[arg(short, long)]
        name: Option<String>,
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
        Commands::Extract {
            file,
            force,
            name,
            out,
        } => {
            if debug {
                dbg!(file, force, name, out);
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
