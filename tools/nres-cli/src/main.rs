extern crate core;
extern crate libnres;

use std::io::Write;

use clap::{Parser, Subcommand};
use miette::{IntoDiagnostic, Result};

#[derive(Parser, Debug)]
#[command(name = "NRes CLI")]
#[command(about, author, version, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Check if the "NRes" file can be extract
    Check {
        /// "NRes" file
        file: String,
    },
    /// Print debugging information on the "NRes" file
    #[command(arg_required_else_help = true)]
    Debug {
        /// "NRes" file
        file: String,
        /// Filter results by file name
        #[arg(long)]
        name: Option<String>,
    },
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
    #[command(arg_required_else_help = true)]
    Ls {
        /// "NRes" file
        file: String,
    },
}

pub fn main() -> Result<()> {
    let stdout = console::Term::stdout();
    let cli = Cli::parse();

    match cli.command {
        Commands::Check { file } => command_check(stdout, file)?,
        Commands::Debug { file, name } => command_debug(stdout, file, name)?,
        Commands::Extract { file, force, out } => command_extract(stdout, file, out, force)?,
        Commands::Ls { file } => command_ls(stdout, file)?,
    }

    Ok(())
}

fn command_check(_stdout: console::Term, file: String) -> Result<()> {
    let file = std::fs::File::open(file).into_diagnostic()?;
    let list = libnres::reader::get_list(&file).into_diagnostic()?;
    let tmp = tempdir::TempDir::new("nres").into_diagnostic()?;
    let bar = indicatif::ProgressBar::new(list.len() as u64);

    bar.set_style(get_bar_style()?);

    for element in list {
        bar.set_message(element.get_filename());

        let path = tmp.path().join(element.get_filename());
        let mut output = std::fs::File::create(path).into_diagnostic()?;
        let mut buffer = libnres::reader::get_file(&file, &element).into_diagnostic()?;

        output.write_all(&buffer).into_diagnostic()?;
        buffer.clear();
        bar.inc(1);
    }

    bar.finish();

    Ok(())
}

fn command_debug(stdout: console::Term, file: String, name: Option<String>) -> Result<()> {
    let file = std::fs::File::open(file).into_diagnostic()?;
    let mut list = libnres::reader::get_list(&file).into_diagnostic()?;

    let mut total_files_size: u32 = 0;
    let mut total_files_gap: u32 = 0;
    let mut total_files: u32 = 0;

    for (index, item) in list.iter().enumerate() {
        total_files_size += item.size;
        total_files += 1;
        let mut gap = 0;

        if index > 1 {
            let previous_item = &list[index - 1];
            gap = item.position - (previous_item.position + previous_item.size);
        }

        total_files_gap += gap;
    }

    if let Some(name) = name {
        list.retain(|item| item.name.contains(&name));
    };

    for (index, item) in list.iter().enumerate() {
        let mut gap = 0;

        if index > 1 {
            let previous_item = &list[index - 1];
            gap = item.position - (previous_item.position + previous_item.size);
        }

        let text = format!("Index: {};\nGap: {};\nItem: {:#?};\n", index, gap, item);
        stdout.write_line(&text).into_diagnostic()?;
    }

    let text = format!(
        "Total files: {};\nTotal files gap: {} (bytes);\nTotal files size: {} (bytes);",
        total_files, total_files_gap, total_files_size
    );

    stdout.write_line(&text).into_diagnostic()?;

    Ok(())
}

fn command_extract(_stdout: console::Term, file: String, out: String, force: bool) -> Result<()> {
    let file = std::fs::File::open(file).into_diagnostic()?;
    let list = libnres::reader::get_list(&file).into_diagnostic()?;
    let bar = indicatif::ProgressBar::new(list.len() as u64);

    bar.set_style(get_bar_style()?);

    for element in list {
        bar.set_message(element.get_filename());

        let path = format!("{}/{}", out, element.get_filename());

        if !force && is_exist_file(&path) {
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
        let mut buffer = libnres::reader::get_file(&file, &element).into_diagnostic()?;

        output.write_all(&buffer).into_diagnostic()?;
        buffer.clear();
        bar.inc(1);
    }

    bar.finish();

    Ok(())
}

fn command_ls(stdout: console::Term, file: String) -> Result<()> {
    let file = std::fs::File::open(file).into_diagnostic()?;
    let list = libnres::reader::get_list(&file).into_diagnostic()?;

    for element in list {
        stdout.write_line(&element.name).into_diagnostic()?;
    }

    Ok(())
}

fn get_bar_style() -> Result<indicatif::ProgressStyle> {
    Ok(
        indicatif::ProgressStyle::with_template("[{bar:32}] {pos:>7}/{len:7} {msg}")
            .into_diagnostic()?
            .progress_chars("=>-"),
    )
}

fn is_exist_file(path: &String) -> bool {
    let metadata = std::path::Path::new(path);
    metadata.exists()
}
