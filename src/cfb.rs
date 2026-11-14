//! Reading the Compound File Binary container used by `.doc` and `.xls`.
//!
//! Both legacy formats are OLE2 documents: a FAT-style file system with
//! sectors, a directory of streams and a small "mini stream" for short data.
//! Only what the readers need is implemented here: opening the container and
//! pulling named streams out of it.

const SIGNATURE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
/// Streams shorter than this live in the mini stream.
const MINI_CUTOFF: usize = 4096;
const DIRECTORY_ENTRY: usize = 128;
const NO_STREAM: u32 = 0xFFFF_FFFF;

/// One entry of the compound file's directory.
struct Entry {
    name: String,
    kind: u8,
    start: u32,
    size: u64,
}

/// A parsed compound file.
pub struct CompoundFile {
    data: Vec<u8>,
    sector_size: usize,
    mini_sector_size: usize,
    fat: Vec<u32>,
    mini_fat: Vec<u32>,
    entries: Vec<Entry>,
    /// Contents of the root entry, which is where the mini stream lives.
    mini_stream: Vec<u8>,
}

impl CompoundFile {
    pub fn open(data: Vec<u8>) -> Result<Self, String> {
        if data.len() < 512 || data[..8] != SIGNATURE {
            return Err(String::from("文件不是有效的复合文档"));
        }
        let sector_shift = u16::from_le_bytes([data[0x1E], data[0x1F]]) as u32;
        let mini_shift = u16::from_le_bytes([data[0x20], data[0x21]]) as u32;
        if !(7..=16).contains(&sector_shift) || mini_shift >= sector_shift {
            return Err(String::from("复合文档的扇区大小无效"));
        }
        let sector_size = 1usize << sector_shift;
        let mini_sector_size = 1usize << mini_shift;

        let fat_sectors = read_u32(&data, 0x2C) as usize;
        let directory_start = read_u32(&data, 0x30);
        let mini_fat_start = read_u32(&data, 0x3C);
        let mini_fat_sectors = read_u32(&data, 0x40) as usize;
        let difat_start = read_u32(&data, 0x44);
        let difat_sectors = read_u32(&data, 0x48) as usize;

        let mut file = CompoundFile {
            sector_size,
            mini_sector_size,
            data,
            fat: Vec::new(),
            mini_fat: Vec::new(),
            entries: Vec::new(),
            mini_stream: Vec::new(),
        };

        // FAT sector numbers: 109 inline, then a chain of DIFAT sectors.
        let mut fat_sector_ids: Vec<u32> = (0..109)
            .map(|index| read_u32(&file.data, 0x4C + index * 4))
            .filter(|sector| *sector != NO_STREAM)
            .collect();
        let mut difat = difat_start;
        let per_sector = sector_size / 4;
        let mut guard = 0usize;
        while difat != NO_STREAM && difat != 0xFFFF_FFFE && guard <= difat_sectors {
            guard += 1;
            let Some(sector) = sector_of(&file.data, sector_size, difat) else {
                break;
            };
            let mut ids = Vec::with_capacity(per_sector);
            for index in 0..per_sector - 1 {
                ids.push(read_u32(sector, index * 4));
            }
            for id in ids {
                if id != NO_STREAM {
                    fat_sector_ids.push(id);
                }
            }
            difat = read_u32(sector, (per_sector - 1) * 4);
        }
        if fat_sectors > 0 && fat_sector_ids.len() > fat_sectors {
            fat_sector_ids.truncate(fat_sectors);
        }
        for id in fat_sector_ids {
            let Some(sector) = sector_of(&file.data, sector_size, id) else {
                continue;
            };
            for index in 0..per_sector {
                file.fat.push(read_u32(sector, index * 4));
            }
        }

        // Directory entries.
        let directory = file.chain(directory_start, None);
        for chunk in directory.chunks_exact(DIRECTORY_ENTRY) {
            let name_len = u16::from_le_bytes([chunk[0x40], chunk[0x41]]) as usize;
            let name_len = name_len.saturating_sub(2).min(64);
            let name: Vec<u16> = (0..name_len / 2)
                .map(|index| u16::from_le_bytes([chunk[index * 2], chunk[index * 2 + 1]]))
                .take_while(|unit| *unit != 0)
                .collect();
            let size = u64::from_le_bytes([
                chunk[0x78],
                chunk[0x79],
                chunk[0x7A],
                chunk[0x7B],
                chunk[0x7C],
                chunk[0x7D],
                chunk[0x7E],
                chunk[0x7F],
            ]);
            file.entries.push(Entry {
                name: String::from_utf16_lossy(&name),
                kind: chunk[0x42],
                start: read_u32(chunk, 0x74),
                size,
            });
        }

        // The root entry (type 5) holds the mini stream.
        if let Some(root) = file.entries.iter().find(|entry| entry.kind == 5) {
            let size = root.size as usize;
            let start = root.start;
            file.mini_stream = file.chain(start, Some(size));
        }

        // Mini FAT: the sector chain is stored in ordinary sectors.
        if mini_fat_sectors > 0 {
            let mini_fat = file.chain(mini_fat_start, None);
            file.mini_fat = mini_fat
                .chunks_exact(4)
                .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                .collect();
        }

        Ok(file)
    }

    /// The named stream, if the file has one.
    pub fn stream(&self, name: &str) -> Option<Vec<u8>> {
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.kind == 2 && entry.name.eq_ignore_ascii_case(name))?;
        Some(self.read_entry(entry))
    }

    pub fn has(&self, name: &str) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.kind == 2 && entry.name.eq_ignore_ascii_case(name))
    }

    fn read_entry(&self, entry: &Entry) -> Vec<u8> {
        let size = entry.size as usize;
        if entry.start == NO_STREAM || size == 0 {
            return Vec::new();
        }
        if size < MINI_CUTOFF {
            self.mini_chain(entry.start, size)
        } else {
            self.chain(entry.start, Some(size))
        }
    }

    /// Reads a chain of ordinary sectors.
    fn chain(&self, start: u32, size: Option<usize>) -> Vec<u8> {
        let mut out = Vec::with_capacity(size.unwrap_or(0));
        let mut sector = start;
        let mut guard = 0usize;
        while sector != NO_STREAM && sector != 0xFFFF_FFFE {
            guard += 1;
            if guard > self.fat.len() + 1 {
                break;
            }
            let Some(bytes) = self.sector(sector) else {
                break;
            };
            out.extend_from_slice(bytes);
            let Some(next) = self.fat.get(sector as usize) else {
                break;
            };
            sector = *next;
            if size.is_some_and(|size| out.len() >= size) {
                break;
            }
        }
        if let Some(size) = size {
            out.truncate(size);
        }
        out
    }

    /// Reads a chain of mini sectors out of the root entry.
    fn mini_chain(&self, start: u32, size: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(size);
        let mut sector = start;
        let mut guard = 0usize;
        while sector != NO_STREAM && sector != 0xFFFF_FFFE {
            guard += 1;
            if guard > self.mini_fat.len() + 1 {
                break;
            }
            let offset = sector as usize * self.mini_sector_size;
            let Some(bytes) = self.mini_stream.get(offset..) else {
                break;
            };
            let take = bytes.len().min(self.mini_sector_size);
            out.extend_from_slice(&bytes[..take]);
            let Some(next) = self.mini_fat.get(sector as usize) else {
                break;
            };
            sector = *next;
            if out.len() >= size {
                break;
            }
        }
        out.truncate(size);
        out
    }

    fn sector(&self, index: u32) -> Option<&[u8]> {
        sector_of(&self.data, self.sector_size, index)
    }
}

fn sector_of(data: &[u8], sector_size: usize, index: u32) -> Option<&[u8]> {
    let offset = 512 + index as usize * sector_size;
    let end = offset.checked_add(sector_size)?;
    data.get(offset..end)
}

fn read_u32(data: &[u8], offset: usize) -> u32 {
    let bytes = data.get(offset..offset + 4).unwrap_or(&[0xFF; 4]);
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}
