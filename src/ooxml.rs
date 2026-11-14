//! The container and XML plumbing shared by the Word and Excel readers.
//!
//! Both formats are ZIP archives of XML parts, so this module knows just enough
//! about ZIP to pull parts into memory and a few helpers for reading
//! namespace-prefixed OOXML attributes.

use std::collections::HashMap;
use std::io::Read as _;

use flate2::read::DeflateDecoder;
use quick_xml::Reader;
use quick_xml::escape::unescape;
use quick_xml::events::{BytesStart, Event};

/// Resolved content of an `.docx` or `.xlsx` package.
pub struct Package {
    entries: HashMap<String, Vec<u8>>,
}

impl Package {
    pub fn open(bytes: &[u8]) -> Result<Self, String> {
        let entries = read_zip(bytes)?;
        if entries.is_empty() {
            return Err(String::from("无法读取这个 Office 文档：压缩包是空的"));
        }
        Ok(Self { entries })
    }

    pub fn has(&self, path: &str) -> bool {
        self.find(path).is_some()
    }

    pub fn bytes(&self, path: &str) -> Option<&[u8]> {
        self.find(path).map(Vec::as_slice)
    }

    pub fn xml(&self, path: &str) -> Option<String> {
        self.find(path).and_then(|data| decode_xml(data))
    }

    /// Relationship id to part path for `part`, resolved against its folder.
    pub fn rels(&self, part: &str) -> HashMap<String, String> {
        let Some((folder, file)) = part.rsplit_once('/') else {
            return HashMap::new();
        };
        let Some(xml) = self.xml(&format!("{folder}/_rels/{file}.rels")) else {
            return HashMap::new();
        };

        let mut map = HashMap::new();
        let mut reader = reader(&xml);
        loop {
            match reader.read_event() {
                Ok(Event::Start(event)) | Ok(Event::Empty(event)) => {
                    if name_of(&event) != b"Relationship" {
                        continue;
                    }
                    let Some(id) = attr(&event, "Id") else {
                        continue;
                    };
                    let target = attr(&event, "TargetMode").filter(|mode| mode == "External");
                    if target.is_some() {
                        continue;
                    }
                    if let Some(path) = attr(&event, "Target") {
                        map.insert(id, resolve_path(folder, &path));
                    }
                }
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
        }
        map
    }

    fn find(&self, path: &str) -> Option<&Vec<u8>> {
        let path = path.trim_start_matches('/');
        if let Some(data) = self.entries.get(path) {
            return Some(data);
        }
        // Office itself is case sensitive here, but documents written by other
        // tools occasionally differ in case.
        self.entries
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(path))
            .map(|(_, data)| data)
    }
}

/// A reader with the settings the readers below expect.
pub fn reader(xml: &str) -> Reader<&[u8]> {
    let mut reader = Reader::from_str(xml);
    let config = reader.config_mut();
    config.trim_text_start = false;
    config.trim_text_end = false;
    // Empty elements are reported as `Empty`, which every parser handles.
    config.expand_empty_elements = false;
    reader
}

pub fn decode_xml(data: &[u8]) -> Option<String> {
    // `xml` declares its encoding; Office writes UTF-8, and UTF-16 shows up
    // only in hand-written files.
    if data.starts_with(&[0xFF, 0xFE]) || data.starts_with(&[0xFE, 0xFF]) {
        return None;
    }
    let data = data.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(data);
    String::from_utf8(data.to_vec()).ok()
}

/// The local part of a possibly namespace-prefixed XML name.
pub fn local_name(name: &[u8]) -> &[u8] {
    match name.iter().position(|byte| *byte == b':') {
        Some(index) => &name[index + 1..],
        None => name,
    }
}

pub fn name_of<'a>(event: &'a BytesStart<'a>) -> &'a [u8] {
    local_name(event.name().into_inner())
}

pub fn end_name<'a>(event: &'a quick_xml::events::BytesEnd<'a>) -> &'a [u8] {
    local_name(event.name().into_inner())
}

/// An attribute value by local name, with entities unescaped.
#[allow(deprecated)]
pub fn attr(event: &BytesStart<'_>, name: &str) -> Option<String> {
    for attribute in event.attributes().flatten() {
        if local_name(attribute.key.as_ref()) != name.as_bytes() {
            continue;
        }
        return attribute
            .unescape_value()
            .ok()
            .map(|value| value.into_owned());
    }
    None
}

pub fn attr_f32(event: &BytesStart<'_>, name: &str) -> Option<f32> {
    attr(event, name)?.trim().parse().ok()
}

/// `RRGGBB` or `AARRGGBB` as written by Office, as an `0xRRGGBB` value.
pub fn parse_hex_color(value: &str) -> Option<u32> {
    let value = value.trim().trim_start_matches('#');
    if value.eq_ignore_ascii_case("auto") {
        return None;
    }
    match value.len() {
        8 => u32::from_str_radix(&value[2..], 16).ok(),
        6 => u32::from_str_radix(value, 16).ok(),
        _ => None,
    }
}

// --------------------------------------------------------------- element tree

/// A minimal element tree, used for the small parts of a package.
pub struct Node {
    pub name: String,
    attrs: Vec<(String, String)>,
    pub text: String,
    pub children: Vec<Node>,
}

impl Node {
    pub fn attr(&self, name: &str) -> Option<String> {
        self.attrs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    }

    pub fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|child| child.name == name)
    }

    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
        self.children.iter().filter(move |child| child.name == name)
    }

    pub fn descendant(&self, name: &str) -> Option<&Node> {
        for child in &self.children {
            if child.name == name {
                return Some(child);
            }
            if let Some(found) = child.descendant(name) {
                return Some(found);
            }
        }
        None
    }
}

pub fn parse_tree(xml: &str) -> Result<Node, String> {
    let mut reader = reader(xml);
    let mut stack = vec![Node {
        name: String::new(),
        attrs: Vec::new(),
        text: String::new(),
        children: Vec::new(),
    }];

    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => stack.push(node_of(&event)),
            Ok(Event::Empty(event)) => {
                let node = node_of(&event);
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(node);
                }
            }
            Ok(Event::End(_)) => {
                if stack.len() > 1 {
                    let node = stack.pop().expect("stack is not empty");
                    if let Some(parent) = stack.last_mut() {
                        parent.children.push(node);
                    }
                }
            }
            Ok(Event::Text(event)) => {
                if let Some(node) = stack.last_mut() {
                    node.text.push_str(&text_of(&event));
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }

    Ok(stack.remove(0))
}

/// The document element of a part, which is what every caller wants to walk.
pub fn root_of(node: Node) -> Node {
    if node.children.len() == 1 && node.text.trim().is_empty() {
        let mut children = node.children;
        children.remove(0)
    } else {
        node
    }
}

fn node_of(event: &BytesStart<'_>) -> Node {
    let mut attrs = Vec::new();
    for attribute in event.attributes().flatten() {
        let key = String::from_utf8_lossy(local_name(attribute.key.as_ref())).into_owned();
        #[allow(deprecated)]
        let value = attribute
            .unescape_value()
            .map(|value| value.into_owned())
            .unwrap_or_default();
        attrs.push((key, value));
    }

    Node {
        name: String::from_utf8_lossy(name_of(event)).into_owned(),
        attrs,
        text: String::new(),
        children: Vec::new(),
    }
}

/// Text of a text event, with XML entities resolved.
pub fn text_of(event: &quick_xml::events::BytesText<'_>) -> String {
    event
        .decode()
        .ok()
        .map(|text| {
            unescape(&text)
                .map(|text| text.into_owned())
                .unwrap_or_else(|_| text.into_owned())
        })
        .unwrap_or_default()
}

fn resolve_path(folder: &str, target: &str) -> String {
    if let Some(target) = target.strip_prefix('/') {
        return target.to_string();
    }

    let mut parts = folder
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    for segment in target.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn read_zip(bytes: &[u8]) -> Result<HashMap<String, Vec<u8>>, String> {
    let end = find_end_of_central_directory(bytes)
        .ok_or_else(|| String::from("不是有效的 Office 文档：找不到 ZIP 目录"))?;
    let count = read_u16(bytes, end + 10) as usize;
    let mut position = read_u32(bytes, end + 16) as usize;
    let mut entries = HashMap::new();

    for _ in 0..count {
        if position + 46 > bytes.len() || read_u32(bytes, position) != 0x0201_4b50 {
            break;
        }

        let method = read_u16(bytes, position + 10);
        let compressed = read_u32(bytes, position + 20) as usize;
        let name_len = read_u16(bytes, position + 28) as usize;
        let extra_len = read_u16(bytes, position + 30) as usize;
        let comment_len = read_u16(bytes, position + 32) as usize;
        let local = read_u32(bytes, position + 42) as usize;
        if position + 46 + name_len > bytes.len() {
            break;
        }
        // Some writers use backslashes as entry separators; OPC paths are
        // always slash separated.
        let name = String::from_utf8_lossy(&bytes[position + 46..position + 46 + name_len])
            .replace('\\', "/");
        position += 46 + name_len + extra_len + comment_len;

        // Directories and ZIP64 entries (over 4 GB) are of no interest here.
        if name.ends_with('/') || compressed == u32::MAX as usize || local + 30 > bytes.len() {
            continue;
        }

        let local_name_len = read_u16(bytes, local + 26) as usize;
        let local_extra_len = read_u16(bytes, local + 28) as usize;
        let start = local + 30 + local_name_len + local_extra_len;
        let stop = start.saturating_add(compressed).min(bytes.len());
        if start >= stop {
            continue;
        }
        let raw = &bytes[start..stop];

        let data = match method {
            0 => raw.to_vec(),
            8 => {
                let mut out = Vec::new();
                if DeflateDecoder::new(raw).read_to_end(&mut out).is_err() {
                    continue;
                }
                out
            }
            // Other methods (bzip2, LZMA) never appear in documents Office writes.
            _ => continue,
        };

        entries.insert(name, data);
    }

    Ok(entries)
}

fn find_end_of_central_directory(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < 22 {
        return None;
    }
    let earliest = bytes.len().saturating_sub(22 + 65_535);
    (earliest..=bytes.len() - 22)
        .rev()
        .find(|start| bytes[*start..*start + 4] == [0x50, 0x4b, 0x05, 0x06])
}
