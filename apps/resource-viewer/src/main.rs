use iced::widget::{button, column, container, horizontal_space, row, scrollable, text};
use iced::{application, Element, Length, Task, Theme};
use rfd::FileDialog;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

fn main() -> iced::Result {
    application("Parkan Resource Viewer", update, view)
        .theme(theme)
        .run_with(|| (ViewerApp::default(), Task::none()))
}

fn theme(_state: &ViewerApp) -> Theme {
    Theme::Light
}

#[derive(Debug, Default)]
struct ViewerApp {
    document: Option<DocumentModel>,
    status: String,
}

#[derive(Debug, Clone)]
enum Message {
    OpenRequested,
    SelectNode(Selection),
}

fn update(state: &mut ViewerApp, message: Message) -> Task<Message> {
    match message {
        Message::OpenRequested => {
            if let Some(path) = pick_archive_file() {
                match load_document(&path) {
                    Ok(document) => {
                        state.status =
                            format!("Loaded {} as {}", path.display(), document.format.label());
                        state.document = Some(document);
                    }
                    Err(err) => {
                        state.status = err;
                    }
                }
            }
        }
        Message::SelectNode(selection) => {
            if let Some(document) = state.document.as_mut() {
                document.selected = selection;
            }
        }
    }

    Task::none()
}

fn view(state: &ViewerApp) -> Element<'_, Message> {
    let top_bar = row![
        button("Open archive").on_press(Message::OpenRequested),
        text(status_text(state)).size(14)
    ]
    .spacing(12);

    let content = if let Some(document) = &state.document {
        view_document(document)
    } else {
        container(text("Open an .nres/.rsli/.lib archive to start.").size(16))
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
    };

    container(column![top_bar, content].spacing(12).padding(12))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn status_text(state: &ViewerApp) -> String {
    if state.status.is_empty() {
        String::from("Ready")
    } else {
        state.status.clone()
    }
}

fn view_document(document: &DocumentModel) -> Element<'_, Message> {
    let mut tree = column![text("Archive tree").size(18)].spacing(6);
    for item in &document.tree_rows {
        let indent = horizontal_space().width(Length::Fixed(f32::from(item.depth) * 16.0));

        let line = row![indent, text(&item.label).size(14)].spacing(6);
        if let Some(selection) = item.selection {
            let mut node_button = button(line)
                .width(Length::Fill)
                .on_press(Message::SelectNode(selection));

            if selection == document.selected {
                node_button = node_button.style(button::primary);
            }

            tree = tree.push(node_button);
        } else {
            tree = tree.push(line);
        }
    }

    let (panel_title, fields) = selected_fields(document);
    let mut fields_column = column![text(panel_title).size(18)].spacing(8);

    for field in fields {
        fields_column = fields_column.push(
            row![
                text(&field.key).size(14).width(Length::Fixed(220.0)),
                text(&field.value).size(14).width(Length::Fill)
            ]
            .spacing(12),
        );
    }

    let left = container(scrollable(tree))
        .width(Length::FillPortion(2))
        .height(Length::Fill);

    let right = container(scrollable(fields_column))
        .width(Length::FillPortion(5))
        .height(Length::Fill);

    row![left, right].spacing(12).height(Length::Fill).into()
}

fn selected_fields(document: &DocumentModel) -> (String, &[FieldRow]) {
    match document.selected {
        Selection::Archive => (
            format!(
                "{} fields ({})",
                document.format.label(),
                document.path.display()
            ),
            &document.archive_fields,
        ),
        Selection::Entry(index) => {
            if let Some(entry) = document.entries.get(index) {
                (entry.panel_title.clone(), &entry.fields)
            } else {
                (String::from("Entry"), &[])
            }
        }
    }
}

fn pick_archive_file() -> Option<PathBuf> {
    FileDialog::new()
        .set_title("Open Parkan archive")
        .pick_file()
}

fn load_document(path: &Path) -> Result<DocumentModel, String> {
    let bytes =
        fs::read(path).map_err(|err| format!("Failed to read {}: {err}", path.display()))?;
    let Some(format) = detect_archive_format(&bytes) else {
        return Err(format!(
            "{} is not recognized as NRes/RsLi (unsupported magic).",
            path.display()
        ));
    };

    match format {
        ArchiveFormat::Nres => load_nres_document(path),
        ArchiveFormat::Rsli => load_rsli_document(path),
    }
}

fn detect_archive_format(bytes: &[u8]) -> Option<ArchiveFormat> {
    if bytes.len() >= 4 && &bytes[0..4] == b"NRes" {
        return Some(ArchiveFormat::Nres);
    }

    if bytes.len() >= 2 && &bytes[0..2] == b"NL" {
        return Some(ArchiveFormat::Rsli);
    }

    None
}

fn load_nres_document(path: &Path) -> Result<DocumentModel, String> {
    let archive = nres::Archive::open_path(path)
        .map_err(|err| format!("NRes open failed for {}: {err}", path.display()))?;

    let info = archive.info();
    let mut archive_fields = vec![
        FieldRow::new("format", "NRes"),
        FieldRow::new("file_size", info.file_size.to_string()),
        FieldRow::new("raw_mode", info.raw_mode.to_string()),
    ];

    if let Some(header) = &info.header {
        archive_fields.push(FieldRow::new(
            "magic",
            String::from_utf8_lossy(&header.magic).into_owned(),
        ));
        archive_fields.push(FieldRow::new("version", format_u32_dec_hex(header.version)));
        archive_fields.push(FieldRow::new("entry_count", header.entry_count.to_string()));
        archive_fields.push(FieldRow::new(
            "total_size",
            format!("{} (0x{:08X})", header.total_size, header.total_size),
        ));
        archive_fields.push(FieldRow::new(
            "directory_offset",
            header.directory_offset.to_string(),
        ));
        archive_fields.push(FieldRow::new(
            "directory_size",
            header.directory_size.to_string(),
        ));
    }

    let mut entries = Vec::new();
    for entry in archive.entries_inspect() {
        let meta = entry.meta;
        let mut fields = vec![
            FieldRow::new("id", entry.id.0.to_string()),
            FieldRow::new("name", meta.name.clone()),
            FieldRow::new("type_id", format_u32_dec_hex(meta.kind)),
            FieldRow::new("attr1", format_u32_dec_hex(meta.attr1)),
            FieldRow::new("attr2", format_u32_dec_hex(meta.attr2)),
            FieldRow::new("attr3", format_u32_dec_hex(meta.attr3)),
            FieldRow::new("data_offset", meta.data_offset.to_string()),
            FieldRow::new("data_size", meta.data_size.to_string()),
            FieldRow::new("sort_index", meta.sort_index.to_string()),
            FieldRow::new("name_raw_hex", bytes_as_hex(entry.name_raw)),
            FieldRow::new("name_raw_ascii", bytes_as_ascii(entry.name_raw)),
        ];

        fields.push(FieldRow::new("find_key", meta.name.to_ascii_lowercase()));

        entries.push(EntryView {
            full_name: meta.name.clone(),
            panel_title: format!("NRes entry #{}: {}", entry.id.0, meta.name),
            fields,
        });
    }

    let tree_rows = build_tree_rows(&entries);

    Ok(DocumentModel {
        path: path.to_path_buf(),
        format: ArchiveFormat::Nres,
        archive_fields,
        entries,
        tree_rows,
        selected: Selection::Archive,
    })
}

fn load_rsli_document(path: &Path) -> Result<DocumentModel, String> {
    let library = rsli::Library::open_path(path)
        .map_err(|err| format!("RsLi open failed for {}: {err}", path.display()))?;

    let header = library.header();
    let mut archive_fields = vec![
        FieldRow::new("format", "RsLi"),
        FieldRow::new("magic", String::from_utf8_lossy(&header.magic).into_owned()),
        FieldRow::new(
            "reserved",
            format!("{} (0x{:02X})", header.reserved, header.reserved),
        ),
        FieldRow::new(
            "version",
            format!("{} (0x{:02X})", header.version, header.version),
        ),
        FieldRow::new("entry_count", header.entry_count.to_string()),
        FieldRow::new("presorted_flag", format!("0x{:04X}", header.presorted_flag)),
        FieldRow::new("xor_seed", format!("0x{:08X}", header.xor_seed)),
        FieldRow::new("header_raw_hex", bytes_as_hex(&header.raw)),
    ];

    if let Some(ao) = library.ao_trailer() {
        archive_fields.push(FieldRow::new("ao_trailer", "present"));
        archive_fields.push(FieldRow::new("ao_overlay", ao.overlay.to_string()));
        archive_fields.push(FieldRow::new("ao_raw_hex", bytes_as_hex(&ao.raw)));
    } else {
        archive_fields.push(FieldRow::new("ao_trailer", "absent"));
    }

    let mut entries = Vec::new();
    for entry in library.entries_inspect() {
        let meta = entry.meta;
        let method_raw = (meta.flags as u16 as u32) & 0x1E0;

        let fields = vec![
            FieldRow::new("id", entry.id.0.to_string()),
            FieldRow::new("name", meta.name.clone()),
            FieldRow::new(
                "flags",
                format!("{} (0x{:04X})", meta.flags, meta.flags as u16),
            ),
            FieldRow::new("method", format!("{:?}", meta.method)),
            FieldRow::new("method_raw", format!("0x{:03X}", method_raw)),
            FieldRow::new("packed_size", meta.packed_size.to_string()),
            FieldRow::new("unpacked_size", meta.unpacked_size.to_string()),
            FieldRow::new("data_offset_effective", meta.data_offset.to_string()),
            FieldRow::new("data_offset_raw", entry.data_offset_raw.to_string()),
            FieldRow::new("sort_to_original", entry.sort_to_original.to_string()),
            FieldRow::new("name_raw_hex", bytes_as_hex(entry.name_raw)),
            FieldRow::new("name_raw_ascii", bytes_as_ascii(entry.name_raw)),
            FieldRow::new("service_tail_hex", bytes_as_hex(entry.service_tail)),
            FieldRow::new("service_tail_ascii", bytes_as_ascii(entry.service_tail)),
        ];

        entries.push(EntryView {
            full_name: meta.name.clone(),
            panel_title: format!("RsLi entry #{}: {}", entry.id.0, meta.name),
            fields,
        });
    }

    let tree_rows = build_tree_rows(&entries);

    Ok(DocumentModel {
        path: path.to_path_buf(),
        format: ArchiveFormat::Rsli,
        archive_fields,
        entries,
        tree_rows,
        selected: Selection::Archive,
    })
}

fn build_tree_rows(entries: &[EntryView]) -> Vec<TreeRow> {
    let mut root = FolderNode::default();
    for (index, entry) in entries.iter().enumerate() {
        insert_tree_path(&mut root, &entry.full_name, index);
    }

    let mut rows = vec![TreeRow {
        depth: 0,
        label: String::from("[Archive fields]"),
        selection: Some(Selection::Archive),
    }];

    flatten_tree(&root, 0, &mut rows);
    rows
}

fn insert_tree_path(root: &mut FolderNode, full_name: &str, entry_index: usize) {
    let mut parts: Vec<&str> = full_name
        .split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .collect();

    if parts.is_empty() {
        parts.push(full_name);
    }

    if parts.len() == 1 {
        root.files.push((parts[0].to_string(), entry_index));
        return;
    }

    let file_name = parts.pop().unwrap_or(full_name);
    let mut node = root;
    for part in parts {
        node = node.folders.entry(part.to_string()).or_default();
    }

    node.files.push((file_name.to_string(), entry_index));
}

fn flatten_tree(node: &FolderNode, depth: u16, out: &mut Vec<TreeRow>) {
    for (folder_name, folder_node) in &node.folders {
        out.push(TreeRow {
            depth,
            label: format!("{folder_name}/"),
            selection: None,
        });
        flatten_tree(folder_node, depth.saturating_add(1), out);
    }

    let mut files = node.files.clone();
    files.sort_by(|left, right| left.0.cmp(&right.0));

    for (name, index) in files {
        out.push(TreeRow {
            depth,
            label: name,
            selection: Some(Selection::Entry(index)),
        });
    }
}

fn bytes_as_hex(bytes: &[u8]) -> String {
    let mut out = String::new();
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 {
            out.push(' ');
        }
        let _ = write!(&mut out, "{byte:02X}");
    }
    out
}

fn bytes_as_ascii(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| {
            if byte.is_ascii_graphic() || *byte == b' ' {
                char::from(*byte)
            } else {
                '.'
            }
        })
        .collect()
}

fn format_u32_dec_hex(value: u32) -> String {
    format!("{} (0x{:08X})", value, value)
}

#[derive(Debug, Clone)]
struct DocumentModel {
    path: PathBuf,
    format: ArchiveFormat,
    archive_fields: Vec<FieldRow>,
    entries: Vec<EntryView>,
    tree_rows: Vec<TreeRow>,
    selected: Selection,
}

#[derive(Debug, Clone, Copy)]
enum ArchiveFormat {
    Nres,
    Rsli,
}

impl ArchiveFormat {
    fn label(self) -> &'static str {
        match self {
            Self::Nres => "NRes",
            Self::Rsli => "RsLi",
        }
    }
}

#[derive(Debug, Clone)]
struct EntryView {
    full_name: String,
    panel_title: String,
    fields: Vec<FieldRow>,
}

#[derive(Debug, Clone)]
struct FieldRow {
    key: String,
    value: String,
}

impl FieldRow {
    fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }
}

#[derive(Debug, Clone)]
struct TreeRow {
    depth: u16,
    label: String,
    selection: Option<Selection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Selection {
    Archive,
    Entry(usize),
}

#[derive(Default, Debug)]
struct FolderNode {
    folders: BTreeMap<String, FolderNode>,
    files: Vec<(String, usize)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_builds_nested_paths() {
        let entries = vec![
            EntryView {
                full_name: String::from("textures/ui/hud.texm"),
                panel_title: String::new(),
                fields: vec![],
            },
            EntryView {
                full_name: String::from("textures/world/ground.texm"),
                panel_title: String::new(),
                fields: vec![],
            },
            EntryView {
                full_name: String::from("root_file.msh"),
                panel_title: String::new(),
                fields: vec![],
            },
        ];

        let rows = build_tree_rows(&entries);
        assert!(rows.iter().any(|row| row.label == "textures/"));
        assert!(rows.iter().any(|row| row.label == "ui/"));
        assert!(rows.iter().any(|row| row.label == "hud.texm"));
        assert!(rows.iter().any(|row| row.label == "root_file.msh"));
    }
}
