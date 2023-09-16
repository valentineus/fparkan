# Library for NRes files (Deprecated)

Library for viewing and retrieving game resources of the game **"Parkan: Iron Strategy"**.
All versions of the game are supported: Demo, IS, IS: Part 1, IS: Part 2.
Supports files with `lib`, `trf`, `rlb` extensions.

The files `gamefont.rlb` and `sprites.lib` are not supported.
This files have an unknown signature.

## Example

Example of extracting game resources:

```rust
fn main() {
    let file = std::fs::File::open("./voices.lib").unwrap();
    // Extracting the list of files
    let list = libnres::reader::get_list(&file).unwrap();

    for element in list {
        // Extracting the contents of the file
        let data = libnres::reader::get_file(&file, &element).unwrap();
    }
}
```
