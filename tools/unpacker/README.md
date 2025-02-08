# NRes Game Resource Unpacker

At the moment, this is a demonstration of the NRes game resource unpacking algorithm in action.
It unpacks 100% of the NRes game resources for the game "Parkan: Iron Strategy".
The unpacked resources can be packed again using the [packer](../packer) utility and replace the original game files.

__Attention!__
This is a test version of the utility.
It overwrites existing files without asking.

## Building

To build the tools, you need to run the following command in the root directory:

```bash
cargo build --release
```

## Running

You can run the utility with the following command:

```bash
./target/release/unpacker /path/to/file.ex /path/to/output
```

- `/path/to/file.ex`: This is the file containing the game resources that will be unpacked.
- `/path/to/output`: This is the directory where the unpacked files will be placed.

## How it Works

The structure describing the packed game resources is not fully understood yet.
Therefore, the utility saves unpacked files in the format `file_name.file_index` because some files have the same name.

Additionally, an `index.json` file is created, which is important for re-packing the files.
This file lists all the fields that game resources have in their packed form.
It is essential to preserve the file index for the game to function correctly, as the game engine looks for the necessary files by index.

Files can be replaced and packed back using the [packer](../packer).
The newly obtained game resource files are correctly processed by the game engine.
For example, sounds and 3D models of warbots' weapons were successfully replaced.