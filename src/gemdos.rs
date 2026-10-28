//! GEMDOS (Atari TOS) FAT12 disk-image writer.
//!
//! Reference: Jean Louis-Guérin, "Atari Hard Disk File System Reference
//! Guide" (2014).
//!
//! Layout (all sizes in 512-byte sectors):
//!
//! ```text
//! sector 0:                                boot sector + BPB + checksum
//! sectors 1..(1 + 2*spf):                  primary + secondary FAT
//! sectors (1 + 2*spf)..data_start:         root directory (112 entries)
//! sectors data_start..end:                 cluster data area
//! ```
//!
//! Atari quirks vs. PC FAT12:
//! - BPB fields, including bytes-per-sector, are little-endian as on PC.
//! - Boot sector ends with a BE16 word-sum checksum: the sum of all 256
//!   big-endian 16-bit words at offsets 0..511 must equal `0x1234`.
//! - 24-bit serial number at offsets 0x08..0x0A.
//!
//! `dos_time`/`dos_date` use **local** time, matching DOS convention.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::time::SystemTime;

const SECTOR_SIZE: usize = 512;
const SECTORS_PER_CLUSTER: u32 = 2;
const CLUSTER_SIZE: usize = SECTOR_SIZE * SECTORS_PER_CLUSTER as usize;
const NUM_FATS: u32 = 2;
const ROOT_DIR_ENTRIES: u32 = 112;
const MEDIA_BYTE: u8 = 0xF9; // 720 KB floppy
const FAT12_MAX_CLUSTERS: u32 = 4084;
const FAT_EOC: u16 = 0xFFF;
const DIRENT_SIZE: usize = 32;

const ATTR_DIR: u8 = 0x10;
const ATTR_FILE: u8 = 0x20;

/// Stats reported after writing.
pub struct Stats {
    pub total_clusters: u32,
}

pub struct Gemdos {
    image: Vec<u8>,
    total_sectors: u32,
    sectors_per_fat: u32,
    /// First sector of the cluster data area.
    data_start: u32,
    total_clusters: u32,
    /// In-memory FAT (16-bit entries; only low 12 bits used for FAT12).
    fat: Vec<u16>,
    next_cluster: u32,
    /// Bytes used in the root directory so far.
    root_dir_used: u32,
}

fn put_be16(buf: &mut [u8], off: usize, v: u16) {
    buf[off..off + 2].copy_from_slice(&v.to_be_bytes());
}
fn put_le16(buf: &mut [u8], off: usize, v: u16) {
    buf[off..off + 2].copy_from_slice(&v.to_le_bytes());
}
fn put_le32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

impl Gemdos {
    pub fn new(size_kb: u32) -> anyhow::Result<Self> {
        if size_kb == 0 {
            anyhow::bail!("size must be positive");
        }
        let bytes = u64::from(size_kb) * 1024;
        if bytes > u64::from(u32::MAX) {
            anyhow::bail!("image size exceeds 4 GB");
        }
        let image_size = bytes as u32;
        let total_sectors = image_size / SECTOR_SIZE as u32;

        let root_dir_sectors = (ROOT_DIR_ENTRIES * 32).div_ceil(SECTOR_SIZE as u32);
        let reserved_sectors: u32 = 1; // boot
        if total_sectors <= reserved_sectors + root_dir_sectors + NUM_FATS + 2 {
            anyhow::bail!("filesystem too small");
        }
        let data_sectors = total_sectors - reserved_sectors - root_dir_sectors;

        // Iteratively size the FAT: more clusters → bigger FAT → fewer
        // clusters. Each FAT12 entry takes 1.5 bytes.
        let mut sectors_per_fat: u32 = 1;
        let total_clusters: u32 = loop {
            let avail = data_sectors - NUM_FATS * sectors_per_fat;
            let clusters = avail / SECTORS_PER_CLUSTER;
            let fat_entries_needed = clusters + 2;
            let fat_bytes = (fat_entries_needed * 3).div_ceil(2);
            let spf_needed = fat_bytes.div_ceil(SECTOR_SIZE as u32);
            if spf_needed <= sectors_per_fat {
                break clusters.min(FAT12_MAX_CLUSTERS);
            }
            sectors_per_fat = spf_needed;
        };
        let data_start = reserved_sectors + NUM_FATS * sectors_per_fat + root_dir_sectors;

        let mut fat = vec![0u16; (total_clusters + 2) as usize];
        // Reserved entries
        fat[0] = 0xF00 | u16::from(MEDIA_BYTE);
        fat[1] = FAT_EOC;

        let mut g = Self {
            image: vec![0u8; image_size as usize],
            total_sectors,
            sectors_per_fat,
            data_start,
            total_clusters,
            fat,
            next_cluster: 2,
            root_dir_used: 0,
        };

        g.write_boot_sector();
        Ok(g)
    }

    fn write_boot_sector(&mut self) {
        // 68000 BRA.S to skip the BPB (offset +0x38 from the byte after the opcode).
        self.image[0x00] = 0x60;
        self.image[0x01] = 0x38;
        self.image[0x02..0x08].copy_from_slice(b"GEMDOS");

        // 24-bit serial at 0x08..0x0A (big-endian Atari convention).
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let serial = (now as u32) & 0xFF_FFFF;
        self.image[0x08] = ((serial >> 16) & 0xFF) as u8;
        self.image[0x09] = ((serial >> 8) & 0xFF) as u8;
        self.image[0x0A] = (serial & 0xFF) as u8;

        // BPB fields use little-endian byte order.
        put_le16(&mut self.image, 0x0B, SECTOR_SIZE as u16);
        self.image[0x0D] = SECTORS_PER_CLUSTER as u8; // SPC
        put_le16(&mut self.image, 0x0E, 1); // reserved sectors
        self.image[0x10] = NUM_FATS as u8;
        put_le16(&mut self.image, 0x11, ROOT_DIR_ENTRIES as u16);
        put_le16(&mut self.image, 0x13, self.total_sectors as u16);
        self.image[0x15] = MEDIA_BYTE;
        put_le16(&mut self.image, 0x16, self.sectors_per_fat as u16);
        put_le16(&mut self.image, 0x18, 9); // sectors per track (9 for 720 KB)
        put_le16(&mut self.image, 0x1A, 2); // heads
        put_le16(&mut self.image, 0x1C, 0); // hidden
    }

    fn cluster_offset(&self, cluster: u32) -> usize {
        ((self.data_start + (cluster - 2) * SECTORS_PER_CLUSTER) as usize) * SECTOR_SIZE
    }

    fn alloc_clusters(&mut self, count: u32) -> anyhow::Result<u32> {
        if count == 0 {
            anyhow::bail!("alloc_clusters called with count=0");
        }
        if self.next_cluster + count > self.total_clusters + 2 {
            anyhow::bail!("no free clusters");
        }
        let first = self.next_cluster;
        for i in 0..count - 1 {
            self.fat[(first + i) as usize] = (first + i + 1) as u16;
        }
        self.fat[(first + count - 1) as usize] = FAT_EOC;
        self.next_cluster += count;
        Ok(first)
    }

    pub fn populate_root(&mut self, src: &Path) -> anyhow::Result<()> {
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let path = entry.path();
            let raw_name = entry.file_name();
            let name = raw_name
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("non-UTF8 filename: {:?}", raw_name))?;
            validate_user_name(name)?;

            let meta = fs::symlink_metadata(&path)?;
            let mtime = meta_mtime(&meta);

            if meta.is_dir() {
                let clust = self.alloc_clusters(1)?;
                self.zero_cluster(clust);
                self.add_root_entry(name, ATTR_DIR, clust, 0, mtime)?;

                // dot and dotdot live in the new cluster. ".."'s cluster
                // is 0 because the parent is the root directory.
                let mut sub_used: u32 = 0;
                self.add_subdir_entry(clust, &mut sub_used, ".", ATTR_DIR, clust, 0, mtime)?;
                self.add_subdir_entry(clust, &mut sub_used, "..", ATTR_DIR, 0, 0, mtime)?;
                self.populate_subdir(clust, &mut sub_used, &path)?;
            } else if meta.is_file() {
                let size = u32::try_from(meta.len())
                    .map_err(|_| anyhow::anyhow!("file exceeds 4 GB: {}", path.display()))?;
                let clust = self.write_file(&path, size)?;
                self.add_root_entry(name, ATTR_FILE, clust, size, mtime)?;
            }
            // Symlinks, devices, etc. are skipped.
        }
        Ok(())
    }

    fn populate_subdir(
        &mut self,
        dir_cluster: u32,
        dir_used: &mut u32,
        src: &Path,
    ) -> anyhow::Result<()> {
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let path = entry.path();
            let raw_name = entry.file_name();
            let name = raw_name
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("non-UTF8 filename: {:?}", raw_name))?;
            validate_user_name(name)?;

            let meta = fs::symlink_metadata(&path)?;
            let mtime = meta_mtime(&meta);

            if meta.is_dir() {
                let clust = self.alloc_clusters(1)?;
                self.zero_cluster(clust);
                self.add_subdir_entry(dir_cluster, dir_used, name, ATTR_DIR, clust, 0, mtime)?;

                let mut sub_used: u32 = 0;
                self.add_subdir_entry(clust, &mut sub_used, ".", ATTR_DIR, clust, 0, mtime)?;
                self.add_subdir_entry(clust, &mut sub_used, "..", ATTR_DIR, dir_cluster, 0, mtime)?;
                self.populate_subdir(clust, &mut sub_used, &path)?;
            } else if meta.is_file() {
                let size = u32::try_from(meta.len())
                    .map_err(|_| anyhow::anyhow!("file exceeds 4 GB: {}", path.display()))?;
                let clust = self.write_file(&path, size)?;
                self.add_subdir_entry(dir_cluster, dir_used, name, ATTR_FILE, clust, size, mtime)?;
            }
        }
        Ok(())
    }

    fn write_file(&mut self, path: &Path, size: u32) -> anyhow::Result<u32> {
        if size == 0 {
            return Ok(0);
        }
        let num_clusters = size.div_ceil(CLUSTER_SIZE as u32);
        let first = self.alloc_clusters(num_clusters)?;

        let mut f = File::open(path)?;
        let mut remaining = size as usize;
        let mut buf = vec![0u8; CLUSTER_SIZE];
        for i in 0..num_clusters {
            let off = self.cluster_offset(first + i);
            let chunk = remaining.min(CLUSTER_SIZE);
            f.read_exact(&mut buf[..chunk])?;
            self.image[off..off + chunk].copy_from_slice(&buf[..chunk]);
            remaining -= chunk;
        }
        Ok(first)
    }

    fn zero_cluster(&mut self, cluster: u32) {
        let off = self.cluster_offset(cluster);
        for byte in self.image[off..off + CLUSTER_SIZE].iter_mut() {
            *byte = 0;
        }
    }

    fn root_dir_offset(&self) -> usize {
        ((1 + NUM_FATS * self.sectors_per_fat) as usize) * SECTOR_SIZE
    }

    fn root_has_name(&self, name83: &[u8; 11]) -> bool {
        let base = self.root_dir_offset();
        let mut off = 0u32;
        while off < self.root_dir_used {
            let ent = &self.image[base + off as usize..base + off as usize + DIRENT_SIZE];
            if ent[0] != 0x00 && ent[0] != 0xE5 && &ent[..11] == name83 {
                return true;
            }
            off += DIRENT_SIZE as u32;
        }
        false
    }

    fn subdir_has_name(&self, dir_cluster: u32, dir_used: u32, name83: &[u8; 11]) -> bool {
        let mut cur = dir_cluster;
        let mut consumed: u32 = 0;
        while consumed < dir_used {
            let mut in_this = dir_used - consumed;
            if in_this > CLUSTER_SIZE as u32 {
                in_this = CLUSTER_SIZE as u32;
            }
            let base = self.cluster_offset(cur);
            let mut off = 0u32;
            while off < in_this {
                let ent = &self.image[base + off as usize..base + off as usize + DIRENT_SIZE];
                if ent[0] != 0x00 && ent[0] != 0xE5 && &ent[..11] == name83 {
                    return true;
                }
                off += DIRENT_SIZE as u32;
            }
            consumed += in_this;
            if consumed < dir_used {
                cur = self.fat[cur as usize] as u32;
            }
        }
        false
    }

    fn add_root_entry(
        &mut self,
        name: &str,
        attr: u8,
        cluster: u32,
        size: u32,
        mtime: i64,
    ) -> anyhow::Result<()> {
        let max_bytes = ROOT_DIR_ENTRIES * DIRENT_SIZE as u32;
        if self.root_dir_used + DIRENT_SIZE as u32 > max_bytes {
            anyhow::bail!("root directory full");
        }

        let name83 = name_to_83(name);
        if self.root_has_name(&name83) {
            anyhow::bail!("duplicate 8.3 name in root: {name}");
        }

        let (dt, dd) = dos_time_date(mtime);
        let off = self.root_dir_offset() + self.root_dir_used as usize;
        let ent = &mut self.image[off..off + DIRENT_SIZE];
        ent[..11].copy_from_slice(&name83);
        ent[0x0B] = attr;
        put_le16(ent, 0x16, dt);
        put_le16(ent, 0x18, dd);
        put_le16(ent, 0x1A, cluster as u16);
        put_le32(ent, 0x1C, size);

        self.root_dir_used += DIRENT_SIZE as u32;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)] // mirrors the C signature
    fn add_subdir_entry(
        &mut self,
        dir_cluster: u32,
        dir_used: &mut u32,
        name: &str,
        attr: u8,
        cluster: u32,
        size: u32,
        mtime: i64,
    ) -> anyhow::Result<()> {
        let name83 = name_to_83(name);
        if self.subdir_has_name(dir_cluster, *dir_used, &name83) {
            anyhow::bail!("duplicate 8.3 name in subdirectory: {name}");
        }

        // Walk the cluster chain, extending it if the offset overruns.
        let mut cur = dir_cluster;
        let mut offset_in_dir = *dir_used;
        while offset_in_dir >= CLUSTER_SIZE as u32 {
            if self.fat[cur as usize] >= 0xFF8 {
                let new_clust = self.alloc_clusters(1)?;
                self.fat[cur as usize] = new_clust as u16;
                self.zero_cluster(new_clust);
            }
            cur = self.fat[cur as usize] as u32;
            offset_in_dir -= CLUSTER_SIZE as u32;
        }

        let (dt, dd) = dos_time_date(mtime);
        let off = self.cluster_offset(cur) + offset_in_dir as usize;
        let ent = &mut self.image[off..off + DIRENT_SIZE];
        ent[..11].copy_from_slice(&name83);
        ent[0x0B] = attr;
        put_le16(ent, 0x16, dt);
        put_le16(ent, 0x18, dd);
        put_le16(ent, 0x1A, cluster as u16);
        put_le32(ent, 0x1C, size);

        *dir_used += DIRENT_SIZE as u32;
        Ok(())
    }

    pub fn write_to(mut self, path: &Path) -> anyhow::Result<Stats> {
        // Write both FAT copies, then patch the boot-sector checksum.
        self.write_fat(1);
        self.write_fat(1 + self.sectors_per_fat);
        self.fix_checksum();

        let mut f = File::create(path)?;
        f.write_all(&self.image)?;
        f.sync_all()?;

        Ok(Stats {
            total_clusters: self.total_clusters,
        })
    }

    fn write_fat(&mut self, fat_start_sector: u32) {
        let base = (fat_start_sector as usize) * SECTOR_SIZE;
        let entries = self.total_clusters + 2;
        let mut i = 0u32;
        while i < entries {
            let a = u32::from(self.fat[i as usize]);
            let b = if i + 1 < entries {
                u32::from(self.fat[(i + 1) as usize])
            } else {
                0
            };
            let byte_off = base + (i as usize / 2) * 3;
            self.image[byte_off] = (a & 0xFF) as u8;
            self.image[byte_off + 1] = (((a >> 8) & 0x0F) | ((b & 0x0F) << 4)) as u8;
            self.image[byte_off + 2] = ((b >> 4) & 0xFF) as u8;
            i += 2;
        }
    }

    /// Sum of all 256 BE16 words in the boot sector must equal `0x1234`.
    /// Set the checksum word at offset `0x1FE` to whatever makes the total
    /// come out right.
    fn fix_checksum(&mut self) {
        let mut sum: u16 = 0;
        let mut i = 0usize;
        while i < 510 {
            sum = sum.wrapping_add(((self.image[i] as u16) << 8) | (self.image[i + 1] as u16));
            i += 2;
        }
        let needed = 0x1234u16.wrapping_sub(sum);
        put_be16(&mut self.image, 0x1FE, needed);
    }
}

// ---------- Filename validation and 8.3 conversion ----------

/// FAT 8.3 disallows control chars, space, and these punctuation chars.
fn illegal_83_char(c: u8) -> bool {
    if c < 0x20 || c == 0x7f {
        return true;
    }
    matches!(
        c,
        b' ' | b'"'
            | b'*'
            | b'+'
            | b','
            | b'/'
            | b':'
            | b';'
            | b'<'
            | b'='
            | b'>'
            | b'?'
            | b'['
            | b'\\'
            | b']'
            | b'|'
    )
}

fn validate_user_name(name: &str) -> anyhow::Result<()> {
    if name.starts_with('.') {
        anyhow::bail!("name starts with dot (no basename): {name}");
    }
    let mut dots = 0;
    for &c in name.as_bytes() {
        if illegal_83_char(c) {
            anyhow::bail!("illegal character 0x{c:02x} in name: {name}");
        }
        if c == b'.' {
            dots += 1;
            if dots > 1 {
                anyhow::bail!("multiple dots in name: {name}");
            }
        }
    }
    Ok(())
}

/// Convert a filename to 8.3 form. "." and ".." are passed through with
/// literal dot bytes (FAT-style readers expect them). Long basenames or
/// extensions are truncated with a stderr warning.
fn name_to_83(name: &str) -> [u8; 11] {
    let mut out = [b' '; 11];
    if name == "." {
        out[0] = b'.';
        return out;
    }
    if name == ".." {
        out[0] = b'.';
        out[1] = b'.';
        return out;
    }

    let bytes = name.as_bytes();
    let dot = bytes.iter().rposition(|&b| b == b'.');
    let (base, ext): (&[u8], &[u8]) = match dot {
        Some(d) => (&bytes[..d], &bytes[d + 1..]),
        None => (bytes, &[]),
    };

    let base_len = if base.len() > 8 {
        eprintln!("mkfs-gemdos: warning: basename truncated to 8 chars: {name}");
        8
    } else {
        base.len()
    };
    for (i, &c) in base[..base_len].iter().enumerate() {
        out[i] = c.to_ascii_uppercase();
    }

    let ext_len = if ext.len() > 3 {
        eprintln!("mkfs-gemdos: warning: extension truncated to 3 chars: {name}");
        3
    } else {
        ext.len()
    };
    for (i, &c) in ext[..ext_len].iter().enumerate() {
        out[8 + i] = c.to_ascii_uppercase();
    }
    out
}

// ---------- Time helpers ----------

fn meta_mtime(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(unix)]
fn dos_time_date(epoch: i64) -> (u16, u16) {
    use std::mem::MaybeUninit;
    let t = epoch as libc::time_t;
    let mut tm = MaybeUninit::<libc::tm>::uninit();
    let r = unsafe { libc::localtime_r(&t, tm.as_mut_ptr()) };
    if r.is_null() {
        return dos_fallback();
    }
    let tm = unsafe { tm.assume_init() };
    let dt = ((tm.tm_hour as u16) << 11) | ((tm.tm_min as u16) << 5) | ((tm.tm_sec as u16) / 2);
    let year = (tm.tm_year - 80).max(0) as u16;
    let dd = (year << 9) | (((tm.tm_mon + 1) as u16) << 5) | (tm.tm_mday as u16);
    (dt, dd)
}

#[cfg(not(unix))]
fn dos_time_date(_epoch: i64) -> (u16, u16) {
    // Without local time available, fall back to a fixed value (1980-01-01 00:00).
    dos_fallback()
}

fn dos_fallback() -> (u16, u16) {
    (0, (1 << 5) | 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn read_be16(buf: &[u8], off: usize) -> u16 {
        u16::from_be_bytes(buf[off..off + 2].try_into().unwrap())
    }

    #[test]
    fn empty_image_has_correct_boot_sector() {
        let dir = TempDir::new().unwrap();
        let img = dir.path().join("test.st");
        Gemdos::new(720).unwrap().write_to(&img).unwrap();

        let bytes = fs::read(&img).unwrap();
        assert_eq!(bytes.len(), 720 * 1024);
        // BRA.S 0x60 0x38
        assert_eq!(bytes[0], 0x60);
        assert_eq!(bytes[1], 0x38);
        assert_eq!(&bytes[2..8], b"GEMDOS");
        // Readers interpret the BPB in little-endian byte order.
        assert_eq!(&bytes[0x0B..0x0D], &[0x00, 0x02]);

        // Boot checksum: sum of 256 BE16 words must equal 0x1234.
        let mut sum = 0u16;
        let mut i = 0;
        while i < 512 {
            sum = sum.wrapping_add(read_be16(&bytes, i));
            i += 2;
        }
        assert_eq!(sum, 0x1234);
    }

    #[test]
    fn populate_writes_files() {
        let src = TempDir::new().unwrap();
        let mut f = File::create(src.path().join("hi.txt")).unwrap();
        f.write_all(b"hello").unwrap();
        drop(f);
        fs::create_dir(src.path().join("sub")).unwrap();
        File::create(src.path().join("sub").join("nest.txt")).unwrap();

        let dst = TempDir::new().unwrap();
        let img = dst.path().join("out.st");
        let mut g = Gemdos::new(720).unwrap();
        g.populate_root(src.path()).unwrap();
        let stats = g.write_to(&img).unwrap();
        assert!(stats.total_clusters > 0);
    }

    #[test]
    fn duplicate_8_3_in_root_is_rejected() {
        let src = TempDir::new().unwrap();
        File::create(src.path().join("longername1.txt")).unwrap();
        File::create(src.path().join("longername2.txt")).unwrap();
        let mut g = Gemdos::new(720).unwrap();
        let err = g.populate_root(src.path()).unwrap_err();
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn illegal_char_is_rejected() {
        let src = TempDir::new().unwrap();
        // ':' is illegal in 8.3
        File::create(src.path().join("a:b.txt")).unwrap();
        let mut g = Gemdos::new(720).unwrap();
        let err = g.populate_root(src.path()).unwrap_err();
        assert!(err.to_string().contains("illegal character"));
    }

    #[test]
    fn dot_dot_in_subdir_uses_literal_dots() {
        let src = TempDir::new().unwrap();
        fs::create_dir(src.path().join("sub")).unwrap();
        let dst = TempDir::new().unwrap();
        let img = dst.path().join("out.st");
        let mut g = Gemdos::new(720).unwrap();
        g.populate_root(src.path()).unwrap();
        g.write_to(&img).unwrap();

        let bytes = fs::read(&img).unwrap();
        // Read the actual layout from the BPB rather than hardcoding it.
        let spf = u16::from_le_bytes(bytes[0x16..0x18].try_into().unwrap()) as u32;
        let root_dir_sector = 1 + NUM_FATS * spf;
        let root_dir_sectors = (ROOT_DIR_ENTRIES * 32).div_ceil(SECTOR_SIZE as u32);
        let data_start = root_dir_sector + root_dir_sectors;
        let root_off = (root_dir_sector as usize) * SECTOR_SIZE;
        assert_eq!(&bytes[root_off..root_off + 11], b"SUB        ");
        let sub_clust =
            u16::from_le_bytes(bytes[root_off + 0x1A..root_off + 0x1C].try_into().unwrap()) as u32;
        let cluster_off =
            ((data_start + (sub_clust - 2) * SECTORS_PER_CLUSTER) as usize) * SECTOR_SIZE;
        // First entry is "."
        assert_eq!(bytes[cluster_off], b'.');
        for &b in &bytes[cluster_off + 1..cluster_off + 11] {
            assert_eq!(b, b' ');
        }
        // Second entry is ".."
        assert_eq!(bytes[cluster_off + 32], b'.');
        assert_eq!(bytes[cluster_off + 33], b'.');
        for &b in &bytes[cluster_off + 34..cluster_off + 11 + 32] {
            assert_eq!(b, b' ');
        }
    }
}
