// On-disk PE parsing only. Ordinal presence is evidence of an export, never of its ABI.
#[derive(Debug, PartialEq)]
pub struct Export {
    pub ordinal: u32,
    pub rva: u32,
    pub names: Vec<String>,
    pub forwarder: Option<String>,
}

#[derive(Debug)]
pub struct Image {
    pub machine: u16,
    pub exports: Vec<Export>,
}

fn bytes(data: &[u8], offset: usize, length: usize) -> Result<&[u8], String> {
    let end = offset.checked_add(length).ok_or("offset overflow")?;
    data.get(offset..end)
        .ok_or_else(|| "truncated PE data".into())
}

fn u16_at(data: &[u8], offset: usize) -> Result<u16, String> {
    Ok(u16::from_le_bytes(
        bytes(data, offset, 2)?.try_into().unwrap(),
    ))
}

fn u32_at(data: &[u8], offset: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(
        bytes(data, offset, 4)?.try_into().unwrap(),
    ))
}

struct Section {
    rva: u32,
    raw_size: u32,
    raw_offset: u32,
}

struct Pe<'a> {
    data: &'a [u8],
    headers_size: u32,
    sections: Vec<Section>,
}

impl Pe<'_> {
    fn at(&self, rva: u32) -> Result<&[u8], String> {
        if rva < self.headers_size {
            return bytes(self.data, rva as usize, (self.headers_size - rva) as usize);
        }
        for section in &self.sections {
            if let Some(delta) = rva.checked_sub(section.rva)
                && delta < section.raw_size
            {
                let offset = section
                    .raw_offset
                    .checked_add(delta)
                    .ok_or("raw offset overflow")?;
                return bytes(
                    self.data,
                    offset as usize,
                    (section.raw_size - delta) as usize,
                );
            }
        }
        Err(format!(
            "unmapped RVA 0x{rva:x} (virtual-only bytes are not file data)"
        ))
    }

    fn string(&self, rva: u32) -> Result<String, String> {
        let data = self.at(rva)?;
        // Bound repeated/aliased names too, not just total input file size.
        let data = &data[..data.len().min(1024)];
        let length = data
            .iter()
            .position(|&b| b == 0)
            .ok_or("unterminated export string or 1024-byte probe limit exceeded")?;
        String::from_utf8(data[..length].to_vec()).map_err(|e| e.to_string())
    }
}

pub fn parse(data: &[u8]) -> Result<Image, String> {
    if bytes(data, 0, 2)? != b"MZ" {
        return Err("missing DOS signature".into());
    }
    let nt = u32_at(data, 0x3c)? as usize;
    if bytes(data, nt, 4)? != b"PE\0\0" {
        return Err("missing PE signature".into());
    }
    // Slice the entire NT header before doing fixed-offset reads.
    let coff = bytes(data, nt, 24)?;
    let machine = u16_at(coff, 4)?;
    let count = u16_at(coff, 6)? as usize;
    if count > 96 {
        return Err("section count exceeds PE limit".into());
    }
    let optional_size = u16_at(coff, 20)? as usize;
    let optional = bytes(data, nt + 24, optional_size)?;
    let (directory_count_offset, directory_offset) = match u16_at(optional, 0)? {
        0x10b => (92, 96),   // PE32
        0x20b => (108, 112), // PE32+
        _ => return Err("unsupported optional-header magic".into()),
    };
    let headers_size = u32_at(optional, 60)?;
    let mut image = Image {
        machine,
        exports: vec![],
    };
    if u32_at(optional, directory_count_offset)? == 0 {
        return Ok(image);
    }
    let export_rva = u32_at(optional, directory_offset)?;
    let export_size = u32_at(optional, directory_offset + 4)?;
    if export_rva == 0 && export_size == 0 {
        return Ok(image);
    }
    if export_rva == 0 || export_size < 40 {
        return Err("invalid export directory".into());
    }
    let export_end = export_rva
        .checked_add(export_size)
        .ok_or("export directory overflow")?;
    let table = bytes(data, nt + 24 + optional_size, count * 40)?;
    let mut pe = Pe {
        data,
        headers_size,
        sections: vec![],
    };
    for i in 0..count {
        let section = &table[i * 40..(i + 1) * 40];
        pe.sections.push(Section {
            rva: u32_at(section, 12)?,
            raw_size: u32_at(section, 16)?,
            raw_offset: u32_at(section, 20)?,
        });
    }
    let directory = bytes(pe.at(export_rva)?, 0, 40)?;
    let base = u32_at(directory, 16)?;
    let functions = u32_at(directory, 20)? as usize;
    let names = u32_at(directory, 24)? as usize;
    if functions > 65536 || names > 65536 {
        return Err("export count exceeds probe limit".into());
    }
    let function_table = if functions == 0 {
        &[]
    } else {
        bytes(pe.at(u32_at(directory, 28)?)?, 0, functions * 4)?
    };
    // Keep ordinal-indexed slots while resolving names. Zero RVA is a hole, not an export.
    let mut slots = Vec::with_capacity(functions);
    for i in 0..functions {
        let rva = u32_at(function_table, i * 4)?;
        let ordinal = base.checked_add(i as u32).ok_or("ordinal overflow")?;
        slots.push(if rva == 0 {
            None
        } else {
            Some(Export {
                ordinal,
                rva,
                names: vec![],
                forwarder: if (export_rva..export_end).contains(&rva) {
                    Some(pe.string(rva)?)
                } else {
                    None
                },
            })
        });
    }
    if names != 0 {
        let name_table = bytes(pe.at(u32_at(directory, 32)?)?, 0, names * 4)?;
        let ordinals = bytes(pe.at(u32_at(directory, 36)?)?, 0, names * 2)?;
        for i in 0..names {
            let slot = u16_at(ordinals, i * 2)? as usize;
            let export = slots
                .get_mut(slot)
                .and_then(Option::as_mut)
                .ok_or("named export refers to absent function")?;
            export.names.push(pe.string(u32_at(name_table, i * 4)?)?);
        }
    }
    image.exports = slots.into_iter().flatten().collect();
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put16(data: &mut [u8], offset: usize, value: u16) {
        data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }
    fn put32(data: &mut [u8], offset: usize, value: u32) {
        data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn fixture(pe64: bool) -> Vec<u8> {
        let mut data = vec![0; 0x400];
        data[..2].copy_from_slice(b"MZ");
        put32(&mut data, 0x3c, 0x80);
        data[0x80..0x84].copy_from_slice(b"PE\0\0");
        put16(&mut data, 0x84, if pe64 { 0x8664 } else { 0x14c });
        put16(&mut data, 0x86, 1);
        let optional_size = if pe64 { 240 } else { 224 };
        put16(&mut data, 0x94, optional_size);
        let optional = 0x98;
        put16(&mut data, optional, if pe64 { 0x20b } else { 0x10b });
        put32(&mut data, optional + 60, 0x200);
        let directories = optional + if pe64 { 112 } else { 96 };
        put32(&mut data, directories - 4, 16);
        put32(&mut data, directories, 0x1000);
        put32(&mut data, directories + 4, 0x80);
        let section = optional + optional_size as usize;
        put32(&mut data, section + 12, 0x1000);
        put32(&mut data, section + 16, 0x200);
        put32(&mut data, section + 20, 0x200);
        put32(&mut data, 0x210, 146); // Base ordinal: ordinal 147 is the second slot.
        put32(&mut data, 0x214, 3);
        put32(&mut data, 0x218, 1);
        put32(&mut data, 0x21c, 0x1040);
        put32(&mut data, 0x220, 0x1050);
        put32(&mut data, 0x224, 0x1058);
        put32(&mut data, 0x240, 0); // A missing ordinal.
        put32(&mut data, 0x244, 0x2000); // Ordinary export RVA (not dereferenced).
        put32(&mut data, 0x248, 0x1060); // Forwarder inside export directory.
        put32(&mut data, 0x250, 0x1080);
        put16(&mut data, 0x258, 1);
        data[0x260..0x26b].copy_from_slice(b"other.Func\0");
        data[0x280..0x286].copy_from_slice(b"Named\0");
        data
    }

    #[test]
    fn pe32_and_pe64_preserve_ordinal_base_holes_names_and_forwarders() {
        for pe64 in [false, true] {
            let image = parse(&fixture(pe64)).unwrap();
            assert_eq!(
                image.exports,
                vec![
                    Export {
                        ordinal: 147,
                        rva: 0x2000,
                        names: vec!["Named".into()],
                        forwarder: None
                    },
                    Export {
                        ordinal: 148,
                        rva: 0x1060,
                        names: vec![],
                        forwarder: Some("other.Func".into())
                    },
                ]
            );
        }
    }

    #[test]
    fn every_truncated_prefix_is_rejected_without_panicking() {
        let data = fixture(true);
        for length in 0..0x400 {
            assert!(parse(&data[..length]).is_err(), "accepted prefix {length}");
        }
    }

    #[test]
    fn malformed_counts_rvas_and_name_indices_are_rejected() {
        for (offset, value) in [
            (0x214, u32::MAX),
            (0x21c, 0xfffffff0),
            (0x98 + 116, u32::MAX),
        ] {
            let mut data = fixture(true);
            put32(&mut data, offset, value);
            assert!(parse(&data).is_err());
        }
        let mut data = fixture(true);
        put16(&mut data, 0x258, 3);
        assert!(parse(&data).is_err());
        put16(&mut data, 0x258, 0); // Name points to a hole.
        assert!(parse(&data).is_err());
        let mut data = fixture(true);
        data[0x280..].fill(b'x');
        assert!(parse(&data).is_err());
        data.resize(0x800, b'x');
        put32(&mut data, 0x98 + 240 + 16, 0x600);
        data[0x700] = 0; // Terminated, but beyond the bounded name length.
        assert!(parse(&data).unwrap_err().contains("1024-byte"));
    }

    #[test]
    fn absent_export_directory_is_valid_not_supported() {
        let mut data = fixture(true);
        put32(&mut data, 0x98 + 112, 0);
        put32(&mut data, 0x98 + 116, 0);
        assert!(parse(&data).unwrap().exports.is_empty());
    }
}
