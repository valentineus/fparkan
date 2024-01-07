# NRes Game Resource Packer

At the moment, this is a demonstration of the NRes game resource packing algorithm in action.
It packs 100% of the NRes game resources for the game "Parkan: Iron Strategy".
The hash sums of the resulting files match the original game files.

__Attention!__
This is a test version of the utility. It overwrites the specified final file without asking.

## Building

To build the tools, you need to run the following command in the root directory:

```bash
cargo build --release
```

## Running

You can run the utility with the following command:

```bash
./target/release/packer /path/to/unpack /path/to/file.ex
```

- `/path/to/unpack`: This is the directory with the resources unpacked by the [unpacker](../unpacker) utility.
- `/path/to/file.ex`: This is the final file that will be created.
