use crate::config::SendOptions;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum TextEncoding {
    #[default]
    Utf8,
    Gbk,
    Ascii,
}
impl TextEncoding {
    pub const ALL: [Self; 3] = [Self::Utf8, Self::Gbk, Self::Ascii];
    pub fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Gbk => "GBK",
            Self::Ascii => "ASCII",
        }
    }
}
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum LineEnding {
    #[default]
    None,
    Cr,
    Lf,
    CrLf,
}
impl LineEnding {
    pub const ALL: [Self; 4] = [Self::None, Self::Cr, Self::Lf, Self::CrLf];
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "无换行",
            Self::Cr => "CR",
            Self::Lf => "LF",
            Self::CrLf => "CRLF",
        }
    }
    pub fn bytes(self) -> &'static [u8] {
        match self {
            Self::None => b"",
            Self::Cr => b"\r",
            Self::Lf => b"\n",
            Self::CrLf => b"\r\n",
        }
    }
}
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum Checksum {
    #[default]
    None,
    Sum8,
    Xor8,
    Modbus,
    Ccitt,
    Crc32,
}
impl Checksum {
    pub const ALL: [Self; 6] = [
        Self::None,
        Self::Sum8,
        Self::Xor8,
        Self::Modbus,
        Self::Ccitt,
        Self::Crc32,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "无校验",
            Self::Sum8 => "SUM8",
            Self::Xor8 => "XOR8",
            Self::Modbus => "Modbus CRC16",
            Self::Ccitt => "CRC16/XMODEM",
            Self::Crc32 => "CRC32/ISO-HDLC",
        }
    }
    pub fn calculate(self, bytes: &[u8]) -> Vec<u8> {
        match self {
            Self::None => vec![],
            Self::Sum8 => vec![bytes.iter().fold(0u8, |a, b| a.wrapping_add(*b))],
            Self::Xor8 => vec![bytes.iter().fold(0u8, |a, b| a ^ b)],
            Self::Modbus => {
                let mut crc = 0xffffu16;
                for b in bytes {
                    crc ^= *b as u16;
                    for _ in 0..8 {
                        crc = if crc & 1 != 0 {
                            (crc >> 1) ^ 0xa001
                        } else {
                            crc >> 1
                        };
                    }
                }
                crc.to_le_bytes().to_vec()
            }
            Self::Ccitt => {
                let mut crc = 0u16;
                for b in bytes {
                    crc ^= (*b as u16) << 8;
                    for _ in 0..8 {
                        crc = if crc & 0x8000 != 0 {
                            (crc << 1) ^ 0x1021
                        } else {
                            crc << 1
                        };
                    }
                }
                crc.to_be_bytes().to_vec()
            }
            Self::Crc32 => {
                let mut crc = !0u32;
                for b in bytes {
                    crc ^= *b as u32;
                    for _ in 0..8 {
                        crc = if crc & 1 != 0 {
                            (crc >> 1) ^ 0xedb88320
                        } else {
                            crc >> 1
                        };
                    }
                }
                (!crc).to_le_bytes().to_vec()
            }
        }
    }
}

pub fn parse_hex(text: &str) -> Result<Vec<u8>, String> {
    let normalized = text.replace([',', ';'], " ");
    let mut bytes = Vec::new();
    for token in normalized.split_whitespace() {
        let token = token
            .strip_prefix("0x")
            .or_else(|| token.strip_prefix("0X"))
            .unwrap_or(token);
        if token.is_empty() || token.len() % 2 != 0 || !token.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(format!("无效 HEX：{token}；每个字节需要两位，如 01 0A FF"));
        }
        for pair in token.as_bytes().as_chunks::<2>().0 {
            bytes.push(u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap());
        }
    }
    Ok(bytes)
}
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut text = String::with_capacity(bytes.len() * 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            text.push(' ');
        }
        let _ = write!(text, "{b:02X}");
    }
    text
}
pub fn encode(text: &str, encoding: TextEncoding) -> Result<Vec<u8>, String> {
    match encoding {
        TextEncoding::Utf8 => Ok(text.as_bytes().to_vec()),
        TextEncoding::Ascii => {
            if text.is_ascii() {
                Ok(text.as_bytes().to_vec())
            } else {
                Err("ASCII 不能发送中文，请选择 UTF-8 或 GBK".into())
            }
        }
        TextEncoding::Gbk => {
            let (bytes, _, errors) = encoding_rs::GBK.encode(text);
            if errors {
                Err("文本包含 GBK 无法编码的字符".into())
            } else {
                Ok(bytes.into_owned())
            }
        }
    }
}
pub fn unescape(text: &str, encoding: TextEncoding) -> Result<Vec<u8>, String> {
    let mut result = Vec::new();
    let mut chars = text.chars();
    let mut literal = String::new();
    while let Some(c) = chars.next() {
        if c != '\\' {
            literal.push(c);
            continue;
        }
        result.extend(encode(&literal, encoding)?);
        literal.clear();
        match chars.next().ok_or("转义符末尾缺少字符")? {
            'r' => result.push(b'\r'),
            'n' => result.push(b'\n'),
            't' => result.push(b'\t'),
            '0' => result.push(0),
            '\\' => result.push(b'\\'),
            'x' => {
                let a = chars
                    .next()
                    .and_then(|c| c.to_digit(16))
                    .ok_or("\\x 后需要两位十六进制数")?;
                let b = chars
                    .next()
                    .and_then(|c| c.to_digit(16))
                    .ok_or("\\x 后需要两位十六进制数")?;
                result.push((a * 16 + b) as u8);
            }
            c => return Err(format!("不支持的转义符：\\{c}")),
        }
    }
    result.extend(encode(&literal, encoding)?);
    Ok(result)
}
pub fn build_packet(text: &str, options: &SendOptions) -> Result<Vec<u8>, String> {
    let mut data = if options.hex {
        parse_hex(text)?
    } else if options.escapes {
        unescape(text, options.encoding)?
    } else {
        encode(text, options.encoding)?
    };
    if options.checksum != Checksum::None {
        if options
            .skip_head
            .checked_add(options.skip_tail)
            .is_none_or(|n| n > data.len())
        {
            return Err("排除的帧头、帧尾超过数据长度".into());
        }
        let tail_start = data.len() - options.skip_tail;
        let checksum = options
            .checksum
            .calculate(&data[options.skip_head..tail_start]);
        data.splice(tail_start..tail_start, checksum);
    }
    data.extend_from_slice(options.ending.bytes());
    if data.is_empty() {
        return Err("发送内容为空".into());
    }
    Ok(data)
}

/// The decoder retains incomplete UTF-8/GBK sequences across I/O chunks.
pub struct StreamDecoder {
    decoder: encoding_rs::Decoder,
    encoding: TextEncoding,
}
impl StreamDecoder {
    pub fn new(encoding: TextEncoding) -> Self {
        Self {
            decoder: match encoding {
                TextEncoding::Gbk => encoding_rs::GBK,
                _ => encoding_rs::UTF_8,
            }
            .new_decoder_without_bom_handling(),
            encoding,
        }
    }
    pub fn push(&mut self, data: &[u8]) -> String {
        if self.encoding == TextEncoding::Ascii {
            return data
                .iter()
                .map(|b| if b.is_ascii() { *b as char } else { '�' })
                .collect();
        }
        let mut out = String::with_capacity(data.len() * 4 + 16);
        let _ = self.decoder.decode_to_string(data, &mut out, false);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hex_accepts_compact_and_prefixed_but_rejects_malformed() {
        assert_eq!(parse_hex("01 0x02,0Aff\n00").unwrap(), [1, 2, 10, 255, 0]);
        for bad in ["A", "0x", "GG", "你好", "123"] {
            assert!(parse_hex(bad).is_err(), "{bad}");
        }
    }
    #[test]
    fn standard_checksum_vectors() {
        let v = b"123456789";
        assert_eq!(Checksum::Modbus.calculate(v), [0x37, 0x4b]);
        assert_eq!(Checksum::Ccitt.calculate(v), [0x31, 0xc3]);
        assert_eq!(Checksum::Crc32.calculate(v), [0x26, 0x39, 0xf4, 0xcb]);
        assert_eq!(Checksum::Sum8.calculate(&[255, 2]), [1]);
    }
    #[test]
    fn checksum_inserted_before_tail_and_line_ending() {
        let o = SendOptions {
            hex: true,
            checksum: Checksum::Sum8,
            skip_head: 1,
            skip_tail: 1,
            ending: LineEnding::CrLf,
            ..SendOptions::default()
        };
        assert_eq!(
            build_packet("AA 01 02 EE", &o).unwrap(),
            [0xaa, 1, 2, 3, 0xee, 13, 10]
        );
        assert!(build_packet("00", &o).is_err());
    }
    #[test]
    fn text_encoding_and_escapes() {
        assert_eq!(
            unescape("中\\r\\n\\xFF\\0", TextEncoding::Gbk).unwrap(),
            [0xd6, 0xd0, 13, 10, 255, 0]
        );
        assert!(encode("中", TextEncoding::Ascii).is_err());
        assert!(unescape("\\xG0", TextEncoding::Utf8).is_err());
    }
    #[test]
    fn multibyte_characters_survive_split_reads() {
        for (encoding, bytes) in [
            (TextEncoding::Utf8, vec![0xe4, 0xb8, 0xad]),
            (TextEncoding::Gbk, vec![0xd6, 0xd0]),
        ] {
            let mut d = StreamDecoder::new(encoding);
            let mut result = String::new();
            for b in bytes {
                result.push_str(&d.push(&[b]));
            }
            assert_eq!(result, "中");
        }
    }
}
