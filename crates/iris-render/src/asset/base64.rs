//! 最小 base64 解码器（标准字母表，支持 padding）。
//!
//! 用于 glTF `data:` URI 内嵌 buffer / image，避免为此引入额外依赖。

const DECODE_TABLE: [i8; 256] = {
    let mut table = [-1i8; 256];
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut i = 0;
    while i < 64 {
        table[alphabet[i] as usize] = i as i8;
        i += 1;
    }
    table
};

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// 标准带 padding 的 base64 编码。
pub fn encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// 解码标准 base64 字符串。非法字符、非法 padding 或数据残位非零返回 `None`。
pub fn decode(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    let mut padding_seen = false;
    let mut data_chars = 0usize;
    for ch in input.bytes() {
        match ch {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/' => {
                if padding_seen {
                    return None; // padding 后不允许再出现数据字符
                }
                data_chars += 1;
                acc = (acc << 6) | DECODE_TABLE[ch as usize] as u32;
                bits += 6;
                if bits >= 8 {
                    bits -= 8;
                    out.push((acc >> bits) as u8);
                }
            }
            b'=' => {
                padding_seen = true;
            }
            // RFC 4648 允许忽略空白
            b' ' | b'\t' | b'\r' | b'\n' => {}
            _ => return None,
        }
    }
    // 长度必须是 4 的倍数（计数据与 padding 字符）
    if (data_chars + input.bytes().filter(|&b| b == b'=').count()) % 4 != 0 {
        return None;
    }
    // 末尾残位必须为 0（否则编码本身已损坏）
    if bits > 0 && acc & ((1 << bits) - 1) != 0 {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_known_vectors() {
        assert_eq!(decode("YWJj").as_deref(), Some(b"abc".as_slice()));
        assert_eq!(
            decode("aGVsbG8sIHdvcmxkIQ==").as_deref(),
            Some(b"hello, world!".as_slice())
        );
        assert_eq!(decode("").as_deref(), Some(&[][..]));
    }

    #[test]
    fn handles_whitespace() {
        assert_eq!(decode("YWJj\n").as_deref(), Some(b"abc".as_slice()));
        assert_eq!(decode("Y W J j").as_deref(), Some(b"abc".as_slice()));
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode("YW*j").is_none());
        assert!(decode("YWJ").is_none(), "长度非 4 的倍数应拒绝");
        assert!(decode("YWJj=").is_none(), "长度非 4 的倍数应拒绝");
        assert!(decode("YWJjZA==x").is_none(), "padding 后有数据应拒绝");
        // 残位非零：末组数据位未对齐到字节边界
        assert!(decode("Zm9=").is_none(), "残位非零的编码应拒绝");
    }

    #[test]
    fn roundtrips_arbitrary_bytes() {
        let data: Vec<u8> = (0..=255u8).chain(0..137).collect();
        let encoded = encode(&data);
        assert_eq!(decode(&encoded).as_deref(), Some(data.as_slice()));
    }

    #[test]
    fn encode_matches_rfc_vectors() {
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(encode(b"foob"), "Zm9vYg==");
    }
}
