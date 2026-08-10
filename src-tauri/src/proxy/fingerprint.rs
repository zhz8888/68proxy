use rand::Rng;
use serde::Serialize;
use sha2::{Digest, Sha256};

/// Windows 设备指纹生成（模拟 CLI 环境，不读取真实系统信息）。
/// 注意：这是"模拟 CLI"的反检测伪装，不读取真实系统信息。

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

const FINGERPRINT_MEMS: &[u32] = &[8, 16, 24, 32, 48, 64];

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

fn sha256_hex(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn rand_hex(rng: &mut impl Rng, bytes: usize) -> String {
    (0..bytes)
        .map(|_| format!("{:02x}", rng.gen::<u8>()))
        .collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct FingerprintComponents {
    pub machine_id_hash: String,
    pub mac_hashes: Vec<String>,
    pub os_user_hash: String,
    pub hostname_hash: String,
    pub git_email_hash: String,
    pub platform: &'static str,
    pub arch: &'static str,
    pub os_release: &'static str,
    pub cpu_model: String,
    pub cpu_count: u32,
    pub mem_gib: u32,
    pub is_container: bool,
    pub timezone: &'static str,
    pub runtime: &'static str,
    pub collector_version: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Fingerprint {
    pub thumbmark: String,
    pub components: FingerprintComponents,
}

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
