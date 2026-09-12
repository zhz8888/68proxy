use rand::Rng;
use serde::Serialize;
use sha2::{Digest, Sha256};

/// Windows 设备指纹生成（模拟 CLI 环境，不读取真实系统信息）。
/// 注意：这是"模拟 CLI"的反检测伪装，不读取真实系统信息。
/// 本常量为候选 CPU 型号与逻辑核心数池，随机挑选其一伪装机器规格。

const FINGERPRINT_CPUS: &[(&str, u32)] = &[
    ("12th Gen Intel(R) Core(TM) i7-12650H", 10),
    ("12th Gen Intel(R) Core(TM) i5-12400F", 6),
    ("12th Gen Intel(R) Core(TM) i9-12900K", 16),
    ("13th Gen Intel(R) Core(TM) i7-13700K", 16),
    ("13th Gen Intel(R) Core(TM) i5-13600K", 14),
    ("13th Gen Intel(R) Core(TM) i9-13900K", 24),
    ("Intel(R) Core(TM) Ultra 7 155H", 16),
    ("Intel(R) Core(TM) Ultra 9 285H", 16),
    ("Intel(R) Core(TM) i9-14900K", 24),
    ("Intel(R) Core(TM) i7-14700K", 20),
    ("AMD Ryzen 7 7800X3D", 8),
    ("AMD Ryzen 9 7950X", 16),
    ("AMD Ryzen 5 7600", 6),
    ("AMD Ryzen 9 7900X", 12),
    ("AMD Ryzen 7 5800X3D", 8),
];

/// 候选内存容量池（GiB），随机挑选其一伪装机器规格。
const FINGERPRINT_MEMS: &[u32] = &[8, 16, 24, 32, 48, 64];

/// 候选 IANA 时区名池，随机挑选其一伪装地理位置。
const FINGERPRINT_TZS: &[&str] = &[
    "America/New_York",
    "America/Chicago",
    "America/Los_Angeles",
    "America/Toronto",
    "Europe/London",
    "Europe/Berlin",
    "Europe/Paris",
    "Europe/Moscow",
    "Asia/Shanghai",
    "Asia/Tokyo",
    "Asia/Singapore",
    "Asia/Seoul",
    "Asia/Hong_Kong",
    "Australia/Sydney",
    "Pacific/Auckland",
];

/// 计算字符串的 SHA-256 摘要，返回 64 位小写十六进制表示。
fn sha256_hex(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// 生成 `bytes` 字节的随机十六进制字符串（长度 2*bytes）。
fn rand_hex(rng: &mut impl Rng, bytes: usize) -> String {
    (0..bytes)
        .map(|_| format!("{:02x}", rng.gen::<u8>()))
        .collect()
}

/// 指纹明细字段（随机伪造的机器信息，全部为哈希或固定伪装值）。
///
/// 序列化为 camelCase（machineIdHash / macHashes / osUserHash / …），
/// 字段名与上游期望的格式一致。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FingerprintComponents {
    /// 伪装的机器 ID 哈希（SHA-256）。
    pub machine_id_hash: String,
    /// 伪装网卡 MAC 地址的哈希列表（2-5 个）。
    pub mac_hashes: Vec<String>,
    /// 伪装操作系统用户名哈希。
    pub os_user_hash: String,
    /// 伪装主机名哈希。
    pub hostname_hash: String,
    /// 伪装 git 邮箱哈希。
    pub git_email_hash: String,
    /// 伪装平台，固定 win32。
    pub platform: &'static str,
    /// 伪装 CPU 架构，固定 x64。
    pub arch: &'static str,
    /// 伪装操作系统版本号。
    pub os_release: &'static str,
    /// 从池中随机选取的 CPU 型号。
    pub cpu_model: String,
    /// 对应 CPU 型号的逻辑核心数。
    pub cpu_count: u32,
    /// 随机选取的内存容量（GiB）。
    #[serde(rename = "memGiB")]
    pub mem_gib: u32,
    /// 是否运行在容器中，固定 false（避免触发上游容器检测）。
    pub is_container: bool,
    /// 随机选取的时区名。
    pub timezone: &'static str,
    /// 运行环境标识，固定 cli（模拟 CLI 而非 IDE 插件）。
    pub runtime: &'static str,
    /// 指纹采集器版本号。
    pub collector_version: u32,
}

/// 完整设备指纹：稳定的摘要 thumbmark + 可上报的明细分量。
#[derive(Debug, Clone, Serialize)]
pub struct Fingerprint {
    /// 由关键分量拼接后 SHA-256 得到的稳定摘要，作为设备唯一标识。
    pub thumbmark: String,
    /// 指纹明细字段。
    pub components: FingerprintComponents,
}

/// 随机生成一份全新的伪装设备指纹。
///
/// 从 CPU/内存/时区池中随机取值，随机哈希伪造机器 ID、MAC、用户名等标识，
/// 再将关键分量以 `|` 拼接计算 thumbmark 摘要。每次调用结果互不相同。
pub fn generate() -> Fingerprint {
    let mut rng = rand::thread_rng();
    let (cpu_model, cpu_count) = FINGERPRINT_CPUS[rng.gen_range(0..FINGERPRINT_CPUS.len())];
    let mem_gib = FINGERPRINT_MEMS[rng.gen_range(0..FINGERPRINT_MEMS.len())];
    let timezone = FINGERPRINT_TZS[rng.gen_range(0..FINGERPRINT_TZS.len())];
    let mac_count = rng.gen_range(2..=5);

    let mut mac_hashes = Vec::with_capacity(mac_count);
    for _ in 0..mac_count {
        mac_hashes.push(sha256_hex(&rand_hex(&mut rng, 32)));
    }

    let machine_id_hash = sha256_hex(&rand_hex(&mut rng, 32));
    let os_user_hash = sha256_hex(&rand_hex(&mut rng, 16));
    let hostname_hash = sha256_hex(&rand_hex(&mut rng, 16));
    let git_email_hash = sha256_hex(&rand_hex(&mut rng, 16));

    // thumbmark = 各关键分量按固定顺序以 '|' 拼接后的 SHA-256，顺序变化会导致指纹不一致
    let thumb_data = [
        machine_id_hash.clone(),
        mac_hashes.join("|"),
        os_user_hash.clone(),
        hostname_hash.clone(),
        git_email_hash.clone(),
        "win32".to_string(),
        "10.0.22631".to_string(),
        cpu_model.to_string(),
        cpu_count.to_string(),
        mem_gib.to_string(),
    ]
    .join("|");
    let thumbmark = sha256_hex(&thumb_data);

    Fingerprint {
        thumbmark,
        components: FingerprintComponents {
            machine_id_hash,
            mac_hashes,
            os_user_hash,
            hostname_hash,
            git_email_hash,
            platform: "win32",
            arch: "x64",
            os_release: "10.0.22631",
            cpu_model: cpu_model.to_string(),
            cpu_count,
            mem_gib,
            is_container: false,
            timezone,
            runtime: "cli",
            collector_version: 1,
        },
    }
}
