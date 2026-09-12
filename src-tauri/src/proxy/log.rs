use serde::Serialize;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// 日志条目（环形缓冲，前端实时展示）。
#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    /// 全局自增序号，前端用它做增量拉取的游标。
    pub seq: u64,
    /// Unix 毫秒时间戳。
    pub ts: u64,
    /// 日志级别：info / warn / error。
    pub level: String,
    /// 日志正文。
    pub msg: String,
}

/// 线程安全的日志环形缓冲区：超过容量 `max` 时从头部丢弃最旧条目。
pub struct LogBuffer {
    entries: Mutex<VecDeque<LogEntry>>,
    seq: Mutex<u64>,
    max: usize,
}

impl LogBuffer {
    /// 创建容量为 `max` 条的空缓冲区（const 以便作为静态全局初始化）。
    pub const fn new(max: usize) -> Self {
        Self {
            entries: Mutex::new(VecDeque::new()),
            seq: Mutex::new(0),
            max,
        }
    }

    /// 追加一条日志并返回生成的条目（含分配的 seq 与时间戳）。
    pub fn push(&self, level: &str, msg: &str) -> LogEntry {
        let entry = LogEntry {
            seq: {
                let mut s = self.seq.lock().unwrap();
                *s += 1;
                *s
            },
            ts: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            level: level.to_string(),
            msg: msg.to_string(),
        };
        let mut entries = self.entries.lock().unwrap();
        entries.push_back(entry.clone());
        while entries.len() > self.max {
            entries.pop_front();
        }
        entry
    }

    /// 增量读取：返回 seq 大于 `after_seq` 的条目中最新的 `limit` 条，按时间正序。
    pub fn drain(&self, limit: usize, after_seq: u64) -> Vec<LogEntry> {
        let entries = self.entries.lock().unwrap();
        entries
            .iter()
            .filter(|e| e.seq > after_seq)
            .rev()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    /// 清空缓冲（不重置 seq，避免前端游标错位）。
    pub fn clear(&self) {
        self.entries.lock().unwrap().clear();
    }
}

/// 全局日志缓冲 + 事件转发器（Tauri AppHandle 在 setup 时注入）。
static LOG_BUFFER: LogBuffer = LogBuffer::new(1000);
/// 可选的外部事件转发回调（如 Tauri emit），每条日志写入缓冲后同步调用。
static SINK: Mutex<Option<Box<dyn Fn(&LogEntry) + Send + Sync>>> = Mutex::new(None);

/// 注册日志事件转发回调（Tauri setup 阶段调用一次，用于向前端推送实时日志）。
pub fn set_sink<F>(f: F)
where
    F: Fn(&LogEntry) + Send + Sync + 'static,
{
    *SINK.lock().unwrap() = Some(Box::new(f));
}

/// 写入一条日志：先入全局缓冲，再转发给已注册的 sink（若有）。
pub fn log(level: &str, msg: &str) {
    let entry = LOG_BUFFER.push(level, msg);
    if let Some(sink) = SINK.lock().unwrap().as_ref() {
        sink(&entry);
    }
}

/// 记录 info 级别日志。
pub fn info(msg: &str) {
    log("info", msg);
}

/// 记录 warn 级别日志。
pub fn warn(msg: &str) {
    log("warn", msg);
}

/// 记录 error 级别日志。
pub fn error(msg: &str) {
    log("error", msg);
}

/// 增量拉取全局日志缓冲（供 Tauri 命令调用，见 LogBuffer::drain）。
pub fn get_logs(limit: usize, after_seq: u64) -> Vec<LogEntry> {
    LOG_BUFFER.drain(limit, after_seq)
}

/// 清空全局日志缓冲。
pub fn clear_logs() {
    LOG_BUFFER.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 缓冲区容量裁剪、增量游标读取与清空后 seq 不重置。
    #[test]
    fn buffer_push_drain_and_cap() {
        let buf = LogBuffer::new(3);
        for i in 0..5 {
            buf.push("info", &format!("m{i}"));
        }
        let all = buf.drain(100, 0);
        assert_eq!(all.len(), 3, "超出容量应丢弃最旧");
        assert_eq!(all[0].msg, "m2");
        assert_eq!(all[2].seq, 5);
        // 游标之后的增量为空
        assert!(buf.drain(100, 5).is_empty());
        assert_eq!(buf.drain(1, 2).len(), 1);
        // 清空但不重置 seq
        buf.clear();
        assert!(buf.drain(10, 0).is_empty());
        let e = buf.push("warn", "after-clear");
        assert!(e.seq > 5);
    }

    /// 全局日志：各级别写入可读回，sink 转发被调用。
    #[test]
    fn global_log_levels_and_sink() {
        info("i1");
        warn("w1");
        error("e1");
        let logs = get_logs(100, 0);
        let msgs: Vec<_> = logs.iter().map(|e| e.msg.as_str()).collect();
        assert!(msgs.contains(&"i1") && msgs.contains(&"w1") && msgs.contains(&"e1"));
        clear_logs();
        assert!(get_logs(100, 0).is_empty());

        // sink 注册后每条日志都会转发
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen2 = seen.clone();
        set_sink(move |e| seen2.lock().unwrap().push(e.msg.clone()));
        error("sink-test");
        assert!(seen.lock().unwrap().iter().any(|m| m == "sink-test"));
        // 还原为空 sink，避免影响其他测试
        set_sink(|_| {});
    }
}
