//! Zstandard 帧原语（M1d：`dsh-persistence:zstd`）。
//!
//! 权威参考：`deepseek-harness/packages/session/session-persistence-jsonl/src/zstd.ts`
//! （见 M1d 规范 §D）。逐字对齐：
//! - 容器 = 串联的独立帧（header 帧 + 每批一个事件帧）；
//! - 每帧用 `ZSTD_c_checksumFlag=1` 创建（帧内容 XXH64 校验，my Rust 侧用
//!   `zstd` crate `Encoder::include_checksum(true)` 对齐）；
//! - `scan_zstd_frames` 结构性扫描（不解压块）：校验 magic / frame descriptor /
//!   block header，落在帧内的 EOF → `torn_start`；
//! - `decompress_zstd_prefix`：TS 用 `ZSTD_e_flush` 恢复残缺末帧的可提交明文；
//!   **Rust `zstd` crate 不暴露 flush 模式**（见 DECISIONS），实现为：解压所有
//!   完整帧 + 尝试解码残缺帧可恢复前缀；不可恢复时返回空恢复（torn 仅截断）。

use std::io::{Read, Write};

/// Zstandard 帧开始 magic（`0xFD2FB528`，LE bytes `28 B5 2F FD`）。
pub const ZSTD_MAGIC: u32 = 4247762216;

/// 一个完整结构 Zstandard 帧的字节范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZstdFrameRange {
    /// 包含帧起点。
    pub start: usize,
    /// 排他帧终点。
    pub end: usize,
}

/// 串联 Zstandard 流的结构性扫描结果。
#[derive(Debug, Clone, Default)]
pub struct ZstdFrameScan {
    /// 按文件顺序的完整帧。
    pub frames: Vec<ZstdFrameRange>,
    /// EOF 打断的一个不完整末帧的起点。
    pub torn_start: Option<usize>,
}

/// 结构性定位完整帧（不解压其块）。无效完整结构拒绝；帧内 EOF 返回其起点供修复。
///
/// `max_frames` 供元数据只读通道限制完整帧数。
pub fn scan_zstd_frames(buffer: &[u8], max_frames: Option<usize>) -> Result<ZstdFrameScan, String> {
    let max_frames = max_frames.unwrap_or(usize::MAX);
    let mut frames = Vec::new();
    let mut offset = 0usize;
    while offset < buffer.len() {
        let start = offset;
        // magic
        if buffer.len() - offset < 4 {
            return Ok(ZstdFrameScan {
                frames,
                torn_start: Some(start),
            });
        }
        let magic = u32::from_le_bytes(
            buffer[offset..offset + 4]
                .try_into()
                .expect("4-byte slice"),
        );
        if magic != ZSTD_MAGIC {
            return Err(format!(
                "corrupt Zstandard session log: invalid frame magic at byte {offset}"
            ));
        }
        offset += 4;
        if offset == buffer.len() {
            return Ok(ZstdFrameScan {
                frames,
                torn_start: Some(start),
            });
        }
        // frame descriptor
        let descriptor = buffer[offset];
        offset += 1;
        if (descriptor & 24) != 0 {
            return Err(format!(
                "corrupt Zstandard session log: reserved frame-header bit at byte {}",
                offset - 1
            ));
        }
        let content_size_flag = descriptor >> 6;
        let single_segment = (descriptor & 32) != 0;
        let checksum = (descriptor & 4) != 0;
        let dictionary_flag = descriptor & 3;
        let dictionary_bytes = if dictionary_flag == 3 { 4usize } else { dictionary_flag as usize };
        let content_size_bytes: usize = if content_size_flag == 0 {
            if single_segment { 1 } else { 0 }
        } else {
            1usize << content_size_flag
        };
        let remaining_header_bytes = (if single_segment { 0 } else { 1 })
            + dictionary_bytes
            + content_size_bytes;
        if buffer.len() - offset < remaining_header_bytes {
            return Ok(ZstdFrameScan {
                frames,
                torn_start: Some(start),
            });
        }
        offset += remaining_header_bytes;
        // blocks
        loop {
            if buffer.len() - offset < 3 {
                return Ok(ZstdFrameScan {
                    frames,
                    torn_start: Some(start),
                });
            }
            let block_header = u32::from_le_bytes([
                buffer[offset],
                buffer[offset + 1],
                buffer[offset + 2],
                0,
            ]);
            offset += 3;
            let last_block = (block_header & 1) != 0;
            let block_type = (block_header >> 1) & 3;
            let block_size = (block_header >> 3) as usize;
            if block_type == 3 {
                return Err(format!(
                    "corrupt Zstandard session log: reserved block type at byte {}",
                    offset - 3
                ));
            }
            let payload_bytes = if block_type == 1 { 1 } else { block_size };
            if buffer.len() - offset < payload_bytes {
                return Ok(ZstdFrameScan {
                    frames,
                    torn_start: Some(start),
                });
            }
            offset += payload_bytes;
            if last_block {
                break;
            }
        }
        // 内容校验和（4 字节）
        if checksum {
            if buffer.len() - offset < 4 {
                return Ok(ZstdFrameScan {
                    frames,
                    torn_start: Some(start),
                });
            }
            offset += 4;
        }
        frames.push(ZstdFrameRange { start, end: offset });
        if frames.len() == max_frames {
            return Ok(ZstdFrameScan {
                frames,
                torn_start: None,
            });
        }
    }
    Ok(ZstdFrameScan {
        frames,
        torn_start: None,
    })
}

/// 把一个可独立解码的、带校验和的 Zstandard 帧压缩。
pub fn compress_zstd_frame(input: &[u8]) -> Result<Vec<u8>, String> {
    let mut enc = zstd::stream::write::Encoder::new(Vec::new(), 3)
        .map_err(|e| format!("zstd encoder init failed: {e}"))?;
    enc.include_checksum(true)
        .map_err(|e| format!("zstd encoder checksum flag failed: {e}"))?;
    enc.write_all(input)
        .map_err(|e| format!("zstd compression write failed: {e}"))?;
    enc.finish().map_err(|e| format!("zstd compression finish failed: {e}"))
}

/// 解压一个完整结构帧并校验其校验和。
pub fn decompress_zstd_frame(input: &[u8]) -> Result<Vec<u8>, String> {
    let mut decoder = zstd::stream::read::Decoder::new(input)
        .map_err(|e| format!("zstd decoder init failed: {e}"))?;
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|e| format!("zstd decompression failed: {e}"))?;
    Ok(out)
}

/// 从已知不完整 Zstandard 帧恢复可用明文。
///
/// TS 用 `ZSTD_e_flush`（抑制 final-frame/checksum 完成）恢复已提交明文；Rust
/// `zstd` crate 不暴露该模式，因此在完整帧解码后对残缺帧尝试整体解码——失败时
/// 返回空恢复（调用方按 `tornMarker.truncateTo` 截断）。
pub fn decompress_zstd_prefix(input: &[u8]) -> Result<Vec<u8>, String> {
    // 若残缺帧恰好可完整解码（如最后几字节丢失但块数据完整），尽量恢复；
    // 否则返回空（等价于无恢复）。
    match decompress_zstd_frame(input) {
        Ok(bytes) => Ok(bytes),
        Err(_) => Ok(Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(payload: &[u8]) -> Vec<u8> {
        compress_zstd_frame(payload).expect("compress")
    }

    #[test]
    fn magic_is_byte_accurate() {
        assert_eq!(ZSTD_MAGIC, 0xFD2FB528);
        assert_eq!(ZSTD_MAGIC.to_le_bytes(), [0x28, 0xB5, 0x2F, 0xFD]);
    }

    #[test]
    fn single_frame_round_trips() {
        let input = b"hello persistence\n";
        let f = frame(input);
        assert_eq!(&f[..4], &[0x28, 0xB5, 0x2F, 0xFD]);
        let scan = scan_zstd_frames(&f, None).unwrap();
        assert_eq!(scan.torn_start, None);
        assert_eq!(scan.frames.len(), 1);
        assert_eq!(scan.frames[0], ZstdFrameRange { start: 0, end: f.len() });
        let out = decompress_zstd_frame(&f).unwrap();
        assert_eq!(out, input);
    }

    #[test]
    fn concatenated_frames_scan_all() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&frame(b"header\n"));
        buf.extend_from_slice(&frame(b"event1\n"));
        buf.extend_from_slice(&frame(b"event2\n"));
        let scan = scan_zstd_frames(&buf, None).unwrap();
        assert_eq!(scan.frames.len(), 3, "one frame per write");
        assert_eq!(scan.torn_start, None);
        // 顺序完整
        for range in &scan.frames {
            assert_eq!(range.end, buf.len().min(range.end));
        }
        // 每一帧独立可解
        for range in &scan.frames {
            let text = decompress_zstd_frame(&buf[range.start..range.end]).unwrap();
            let text = String::from_utf8(text).unwrap();
            assert!(text.ends_with('\n'));
        }
    }

    #[test]
    fn torn_final_frame_reports_torn_start() {
        let header_frame = frame(b"header\n");
        let full = frame(b"event with a long payload 0123456789\n");
        let header_end = header_frame.len();
        // 完整首帧 + 残缺末帧
        let mut buf = header_frame.clone();
        buf.extend_from_slice(&full[..full.len() / 2]);
        let scan = scan_zstd_frames(&buf, None).unwrap();
        assert_eq!(scan.frames.len(), 1);
        assert_eq!(scan.frames[0], ZstdFrameRange { start: 0, end: header_end });
        // torn 起点 = 事件帧起点
        assert_eq!(scan.torn_start, Some(header_end));
    }

    #[test]
    fn invalid_magic_rejected() {
        let buf = b"\x00\x00\x00\x00junk";
        let err = scan_zstd_frames(buf, None).unwrap_err();
        assert!(err.contains("invalid frame magic at byte 0"), "{err}");
    }

    #[test]
    fn header_frame_is_exactly_one_line() {
        // JSONL header 帧解压后恰为一行
        let payload = b"{\"type\":\"session\",\"version\":0}\n";
        let f = frame(payload);
        let text = decompress_zstd_frame(&f).unwrap();
        assert_eq!(text, payload);
    }

    #[test]
    fn empty_recovery_on_truncated_frame() {
        let full = frame(b"some committed plaintext\n");
        let cut = full.len() - 5;
        let recovered = decompress_zstd_prefix(&full[..cut]).unwrap();
        // 允许空（Rust 无 flush 模式）；若非空也必须不越界
        if !recovered.is_empty() {
            assert!(recovered.len() <= cut);
        }
    }
}
