use clap::{Parser, Subcommand};

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

fn main() {
    let _cli = Cli::parse();
}
