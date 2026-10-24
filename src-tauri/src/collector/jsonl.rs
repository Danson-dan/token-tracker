/**
 * @Author: Danson zheng
 * @Date: 2026-10-23
 * @Description: JSONL 增量读取器。按字节偏移只消费新增内容，缓存没有换行的半行，
 * 并在文件被截断或轮转时复位，保证 tail 过程既不丢行也不重复消费旧行。
 */

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

/// 一次读取得到的完整行与文件回退信号
#[derive(Debug, Default)]
pub struct TailBatch {
    /// 行号从 1 开始，空行也会占用行号
    pub lines: Vec<(usize, String)>,
    /// 文件长度小于已读偏移，说明被截断或轮转，调用方需要重置解析状态
    pub truncated: bool,
}

/// 基于字节偏移的 JSONL 增量读取器
#[derive(Debug, Default)]
pub struct JsonlTailer {
    offset: u64,
    line_no: usize,
    /// 保存最后一段没有换行符的内容，等下次追加后拼接再解析
    pending: Vec<u8>,
}

impl JsonlTailer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn read_new_lines(&mut self, path: &Path) -> io::Result<TailBatch> {
        let mut file = File::open(path)?;
        let file_len = file.metadata()?.len();
        let mut batch = TailBatch::default();

        if file_len < self.offset {
            self.reset();
            batch.truncated = true;
        }

        file.seek(SeekFrom::Start(self.offset))?;
        let mut chunk = Vec::new();
        file.read_to_end(&mut chunk)?;
        self.offset = self.offset.saturating_add(chunk.len() as u64);
        self.pending.extend_from_slice(&chunk);

        let mut consumed = 0usize;
        while let Some(relative) = self.pending[consumed..]
            .iter()
            .position(|byte| *byte == b'\n')
        {
            let line_end = consumed + relative;
            let raw = &self.pending[consumed..line_end];
            self.line_no += 1;
            let line = String::from_utf8_lossy(raw)
                .trim_end_matches('\r')
                .to_string();
            batch.lines.push((self.line_no, line));
            consumed = line_end + 1;
        }

        if consumed > 0 {
            self.pending.drain(..consumed);
        }

        Ok(batch)
    }

    fn reset(&mut self) {
        self.offset = 0;
        self.line_no = 0;
        self.pending.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_path(name: &str) -> PathBuf {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "token-tracker-jsonl-{}-{}-{}",
            std::process::id(),
            name,
            unique
        ))
    }

    fn append(path: &Path, content: &str) {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("测试文件应可追加");
        file.write_all(content.as_bytes())
            .expect("测试内容应写入成功");
    }

    #[test]
    fn partial_line_is_buffered_until_newline() {
        let path = temp_path("partial");
        fs::write(&path, "{\"a\":1}\n{\"b\":").expect("初始内容应写入成功");

        let mut tailer = JsonlTailer::new();
        let first = tailer.read_new_lines(&path).expect("首次读取应成功");
        assert_eq!(first.lines, vec![(1, "{\"a\":1}".to_string())]);

        append(&path, "2}\n");
        let second = tailer.read_new_lines(&path).expect("追加读取应成功");
        assert_eq!(second.lines, vec![(2, "{\"b\":2}".to_string())]);

        fs::remove_file(&path).expect("测试文件应可删除");
    }

    #[test]
    fn truncation_resets_offset_and_line_numbers() {
        let path = temp_path("truncate");
        fs::write(&path, "{\"a\":1}\n{\"b\":2}\n").expect("初始内容应写入成功");

        let mut tailer = JsonlTailer::new();
        let first = tailer.read_new_lines(&path).expect("首次读取应成功");
        assert_eq!(first.lines.len(), 2);
        assert!(!first.truncated);

        fs::write(&path, "{\"c\":3}\n").expect("截断内容应写入成功");
        let second = tailer.read_new_lines(&path).expect("截断后读取应成功");
        assert!(second.truncated);
        assert_eq!(second.lines, vec![(1, "{\"c\":3}".to_string())]);

        fs::remove_file(&path).expect("测试文件应可删除");
    }
}
