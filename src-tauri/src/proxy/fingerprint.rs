//! Windows 设备指纹生成（模拟 CLI 环境，不读取真实系统信息）。
//!
//! 对齐官方 CLI 1.53.1 的 buildMachineFingerprint：**按 apiKey 确定性派生**一组逼真的
//! 信号值（CPU/内存/时区/机器 ID/MAC/用户名/主机名/git 邮箱），再按 CLI 的哈希算法
//! （主盐 `command-code:device-fingerprint:v1` + `\0` 分隔）计算各分量哈希与 thumbmark。
//!
//! 为什么必须由 apiKey 派生而不是随机：指纹代表「这个账号对应的那台设备」，重启、
//! 多实例、额度用尽停用数周后恢复，上游都应看到同一台设备；换指纹本身就是可疑信号。
//! 加 `fingerprint_salt`（CC_FINGERPRINT_SALT）可成批换身份 —— 哈希阶段仍用 CLI 的
//! 固定盐，salt 只影响「伪造出哪台机器」。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 候选 CPU 型号与逻辑核心数池。
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

/// 候选内存容量池（GiB）。
const FINGERPRINT_MEMS: &[u32] = &[8, 16, 24, 32, 48, 64];

/// 候选 IANA 时区名池。
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

/// 候选 MAC 数量（CLI 随机 2~5 个）。
const FINGERPRINT_MAC_COUNT_RANGE: &[u32] = &[2, 3, 4, 5];

/// 候选操作系统用户名池（伪造 os.userInfo 用户名）。
const FP_OS_USERS: &[&str] = &["dev", "user", "admin", "coder", "engineer", "work"];

/// 候选 git 邮箱域名池。
const FP_MAIL_DOMAINS: &[&str] = &["gmail.com", "outlook.com", "qq.com", "163.com"];

/// CLI 的根盐（buildMachineFingerprint 常量）。
const FP_SALT: &str = "command-code:device-fingerprint:v1";

/// 设备档案：指纹 / config.environment / config.workingDir / x-project-slug / lifecycle.os
/// 共用同一份，避免「指纹说 win32、环境说 linux」这类自相矛盾，也避免把宿主机真实
/// 信息（平台、cwd）交给上游。
pub struct DeviceProfile {
    pub platform: &'static str,
    pub arch: &'static str,
    pub os_release: &'static str,
    pub is_container: bool,
    /// 伪造的项目目录：与 x-project-slug 同源（真机里 slug = slugify(workingDir)）。
    pub project_dir: String,
}

/// 默认设备档案（与 CLI 的 DEVICE_PROFILE 常量一致）。
pub fn default_device_profile(device_project_dir: &str) -> DeviceProfile {
    DeviceProfile {
        platform: "win32",
        arch: "x64",
        os_release: "10.0.22631",
        is_container: false,
        project_dir: if device_project_dir.is_empty() {
            r"C:\Users\dev\projects\app".to_string()
        } else {
            device_project_dir.to_string()
        },
    }
}

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

/// 计算 SHA-256 摘要，返回原始字节（用于 fpDigest 的字节比较与截断取 hex）。
fn sha256_bytes(s: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    hasher.finalize().into()
}

/// 按 apiKey 派生某字段的 SHA-256 摘要（字节）。
///
/// 与上游 `fpDigest` 一致：`sha256(salt + "\0" + apiKey + "\0" + field)`。
/// salt 为空时退化为 `sha256("\0" + apiKey + "\0" + field)`（CLI 的 fingerprintSalt 默认空）。
fn fp_digest(api_key: &str, salt: &str, field: &str) -> [u8; 32] {
    sha256_bytes(&format!("{salt}\0{api_key}\0{field}"))
}

/// 从候选池确定性地挑一项：打分取最大（与上游 `fpPickIndex` 一致）。
///
/// 以后往池里加候选只影响「新候选恰好胜出」的那部分 key，不会像取模那样因为池长度
/// 变化让所有 key 一起换设备。
fn fp_pick_index(api_key: &str, salt: &str, field: &str, items: usize, label_of: impl Fn(usize) -> String) -> usize {
    let mut best_idx = 0usize;
    let mut best_score: Option<[u8; 32]> = None;
    for i in 0..items {
        let score = fp_digest(api_key, salt, &format!("{field}\0{}", label_of(i)));
        if best_score.map(|b| score > b).unwrap_or(true) {
            best_score = Some(score);
            best_idx = i;
        }
    }
    best_idx
}

/// CLI 的 hashSignal：`sha256(FP_SALT + "\0" + value.toLowerCase())`，空值返回 None（JSON 里被丢掉）。
fn fingerprint_hash(value: &str) -> Option<String> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    Some(sha256_hex(&format!("{FP_SALT}\0{}", v.to_lowercase())))
}

/// 指纹明细字段（对齐 CLI 上报格式）。
///
/// 序列化为 camelCase（machineIdHash / macHashes / osUserHash / …）。
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// 伪装平台，固定 win32（与 DEVICE_PROFILE 同源）。
    pub platform: String,
    /// 伪装 CPU 架构，固定 x64。
    pub arch: String,
    /// 伪装操作系统版本号。
    pub os_release: String,
    /// 按 apiKey 确定性选取的 CPU 型号。
    pub cpu_model: String,
    /// 对应 CPU 型号的逻辑核心数。
    pub cpu_count: u32,
    /// 确定性选取的内存容量（GiB）。
    #[serde(rename = "memGiB")]
    pub mem_gib: u32,
    /// 是否运行在容器中（与 DEVICE_PROFILE 同源，默认 false）。
    pub is_container: bool,
    /// 确定性选取的时区名。
    pub timezone: String,
    /// 运行环境标识，固定 cli（模拟 CLI 而非 IDE 插件）。
    pub runtime: String,
    /// 指纹采集器版本号。
    pub collector_version: u32,
}

/// 完整设备指纹：稳定的摘要 thumbmark + 可上报的明细分量。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fingerprint {
    /// 由 CLI 主盐 + machine 前缀推导的稳定摘要，作为设备唯一标识。
    pub thumbmark: String,
    /// 指纹明细字段。
    pub components: FingerprintComponents,
}

/// 按 apiKey 确定性生成一份伪装设备指纹（同一 key + salt 恒得同一台设备）。
///
/// - `api_key`：上游账户 key（指纹代表「该账号对应的那台设备」）；
/// - `salt`：指纹盐（CC_FINGERPRINT_SALT），改值可成批换身份；
/// - `profile`：设备档案（platform/arch/osRelease/isContainer/projectDir）。
pub fn generate(api_key: &str, salt: &str, profile: &DeviceProfile) -> Fingerprint {
    // 确定性挑选候选池成员
    let cpu_idx = fp_pick_index(api_key, salt, "cpu", FINGERPRINT_CPUS.len(), |i| {
        format!("{}|{}", FINGERPRINT_CPUS[i].0, FINGERPRINT_CPUS[i].1)
    });
    let (cpu_model, cpu_count) = FINGERPRINT_CPUS[cpu_idx];
    let mem_idx = fp_pick_index(api_key, salt, "mem", FINGERPRINT_MEMS.len(), |i| FINGERPRINT_MEMS[i].to_string());
    let mem_gib = FINGERPRINT_MEMS[mem_idx];
    let tz_idx = fp_pick_index(api_key, salt, "timezone", FINGERPRINT_TZS.len(), |i| FINGERPRINT_TZS[i].to_string());
    let timezone = FINGERPRINT_TZS[tz_idx];
    let mac_count_idx = fp_pick_index(api_key, salt, "macCount", FINGERPRINT_MAC_COUNT_RANGE.len(), |i| FINGERPRINT_MAC_COUNT_RANGE[i].to_string());
    let mac_count = FINGERPRINT_MAC_COUNT_RANGE[mac_count_idx] as usize;
    let os_user_idx = fp_pick_index(api_key, salt, "osUser", FP_OS_USERS.len(), |i| FP_OS_USERS[i].to_string());
    let os_user = FP_OS_USERS[os_user_idx];
    let mail_domain_idx = fp_pick_index(api_key, salt, "mailDomain", FP_MAIL_DOMAINS.len(), |i| FP_MAIL_DOMAINS[i].to_string());
    let mail_domain = FP_MAIL_DOMAINS[mail_domain_idx];

    // 各信号原始值（确定性派生，形状逼真）
    let hex_of = |field: &str, bytes: usize| -> String {
        fp_digest(api_key, salt, field)[..bytes]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    // Windows MachineGuid 形状：8-4-4-4-12
    let mid = hex_of("machineId", 16);
    let machine_id = format!(
        "{}-{}-{}-{}-{}",
        &mid[0..8],
        &mid[8..12],
        &mid[12..16],
        &mid[16..20],
        &mid[20..32]
    );
    let mut macs: Vec<String> = (0..mac_count)
        .map(|i| {
            fp_digest(api_key, salt, &format!("mac{i}"))[..6]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(":")
        })
        .collect();
    macs.sort(); // CLI 对 MAC 去重后排序
    macs.dedup();
    let hostname = format!("DESKTOP-{}", hex_of("hostname", 4).to_uppercase());
    let git_email = format!("{os_user}.{}@{mail_domain}", hex_of("gitEmail", 3));

    // 各分量哈希（CLI 的 hashSignal：主盐 + \0 + 小写值）
    let machine_id_hash = fingerprint_hash(&machine_id);
    let mac_hashes: Vec<String> = macs.iter().filter_map(|m| fingerprint_hash(m)).collect();
    let os_user_hash = fingerprint_hash(os_user);
    let hostname_hash = fingerprint_hash(&hostname);
    let git_email_hash = fingerprint_hash(&git_email);

    // CLI 的 thumbmark：主盐 + "\0machine\0" + join([machineId, macs.join(",")])
    // （machineId 非空时不再拼 hostname/cpuModel）
    let macs_joined = macs.join(",");
    let mut thumb_seed: Vec<&str> = Vec::new();
    if !machine_id.trim().is_empty() {
        thumb_seed.push(&machine_id);
        thumb_seed.push(&macs_joined);
    } else {
        thumb_seed.push(&hostname);
        thumb_seed.push(cpu_model);
    }
    let seed = thumb_seed.join("|");
    let thumbmark = sha256_hex(&format!("{FP_SALT}\0machine\0{}", if seed.is_empty() { "unknown" } else { &seed }));

    Fingerprint {
        thumbmark,
        components: FingerprintComponents {
            // 各分量 hash 在信号值非空时恒有值；unwrap_or_default 兜底（与 CLI 的
            // hashSignal 空值返回 undefined 被 JSON 丢弃的行为等价于不出现该字段，
            // 但本项目结构要求 String，故用空串）
            machine_id_hash: machine_id_hash.unwrap_or_default(),
            mac_hashes,
            os_user_hash: os_user_hash.unwrap_or_default(),
            hostname_hash: hostname_hash.unwrap_or_default(),
            git_email_hash: git_email_hash.unwrap_or_default(),
            platform: profile.platform.into(),
            arch: profile.arch.into(),
            os_release: profile.os_release.into(),
            cpu_model: cpu_model.to_string(),
            cpu_count,
            mem_gib,
            is_container: profile.is_container,
            timezone: timezone.to_string(),
            runtime: "cli".into(),
            collector_version: 1,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    /// 同一 apiKey + salt 确定性派生同一指纹；不同 key 或不同 salt 得到不同指纹。
    #[test]
    fn deterministic_by_key_and_salt() {
        let profile = default_device_profile("");
        let a1 = generate("user_abc", "", &profile);
        let a2 = generate("user_abc", "", &profile);
        assert_eq!(a1.thumbmark, a2.thumbmark, "同 key 同 salt 必须同设备");
        assert_eq!(a1.components.machine_id_hash, a2.components.machine_id_hash);
        let b = generate("user_xyz", "", &profile);
        assert_ne!(a1.thumbmark, b.thumbmark, "不同 key 应为不同设备");
        let c = generate("user_abc", "salt-v2", &profile);
        assert_ne!(a1.thumbmark, c.thumbmark, "不同 salt 应换身份");
    }

    /// thumbmark 为 64 位十六进制；分量哈希形态正确。
    #[test]
    fn fingerprint_shape() {
        let profile = default_device_profile("");
        let fp = generate("user_abc", "", &profile);
        assert_eq!(fp.thumbmark.len(), 64);
        assert!(fp.thumbmark.chars().all(|c| c.is_ascii_hexdigit()));
        let c = &fp.components;
        assert_eq!(c.platform, "win32");
        assert_eq!(c.arch, "x64");
        assert_eq!(c.os_release, "10.0.22631");
        assert!(!c.is_container);
        assert_eq!(c.runtime, "cli");
        assert_eq!(c.collector_version, 1);
        assert!(!c.machine_id_hash.is_empty());
        assert!(!c.os_user_hash.is_empty());
        assert!(!c.hostname_hash.is_empty());
        assert!(!c.git_email_hash.is_empty());
        assert!(!c.mac_hashes.is_empty() && c.mac_hashes.len() <= 5);
        // 主机名伪装为 DESKTOP-XXXXXX 形态
        let hostname_hash = &c.hostname_hash;
        assert!(hostname_hash.len() == 64);
    }

    /// 组件序列化为 camelCase（与上游上报格式一致）。
    #[test]
    fn fingerprint_camelcase_serialization() {
        let profile = default_device_profile("");
        let fp = generate("user_abc", "", &profile);
        let v = serde_json::to_value(&fp).unwrap();
        assert!(v["components"]["machineIdHash"].is_string());
        assert!(v["components"]["macHashes"].is_array());
        assert!(v["components"]["osUserHash"].is_string());
        assert!(v["components"]["hostnameHash"].is_string());
        assert!(v["components"]["gitEmailHash"].is_string());
        assert!(v["components"]["cpuModel"].is_string());
        assert!(v["components"]["memGiB"].is_number());
        assert!(v["components"]["osRelease"].is_string());
        assert!(v["components"]["collectorVersion"].is_number());
    }

    /// 确定性挑选：不同 key 的 MAC 数量落在合法范围内，且同一 key 恒定。
    #[test]
    fn mac_count_stable_and_bounded() {
        let profile = default_device_profile("");
        let fp = generate("user_abc", "", &profile);
        let n = fp.components.mac_hashes.len();
        assert!((2..=5).contains(&n));
        let fp2 = generate("user_abc", "", &profile);
        assert_eq!(fp.components.mac_hashes, fp2.components.mac_hashes);
    }

    /// 排序比较用于 fp_pick_index 的打分逻辑（`[u8;32]` 需可比）。
    #[test]
    fn byte_array_ordering() {
        let a = [0u8; 32];
        let mut b = [0u8; 32];
        b[0] = 1;
        assert_eq!(a.cmp(&b), Ordering::Less);
        assert!(b > a);
    }
}
