use crate::protocol::{Checksum, LineEnding, TextEncoding};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Serial,
    TcpClient,
    TcpServer,
    Udp,
    Loopback,
}
impl Mode {
    pub const ALL: [Self; 5] = [
        Self::Serial,
        Self::TcpClient,
        Self::TcpServer,
        Self::Udp,
        Self::Loopback,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Serial => "串口",
            Self::TcpClient => "TCP 客户端",
            Self::TcpServer => "TCP 服务端",
            Self::Udp => "UDP",
            Self::Loopback => "回环测试",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnectionConfig {
    pub mode: Mode,
    pub port: String,
    pub baud: u32,
    pub data_bits: u8,
    pub parity: String,
    pub stop_bits: u8,
    pub flow: String,
    pub dtr: bool,
    pub rts: bool,
    pub remote: String,
    pub local: String,
    pub receive_gap_ms: u64,
}
impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            mode: Mode::Serial,
            port: String::new(),
            baud: 115200,
            data_bits: 8,
            parity: "None".into(),
            stop_bits: 1,
            flow: "None".into(),
            dtr: false,
            rts: false,
            remote: "127.0.0.1:8080".into(),
            local: "127.0.0.1:8080".into(),
            receive_gap_ms: 20,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SendOptions {
    pub hex: bool,
    pub encoding: TextEncoding,
    pub escapes: bool,
    pub ending: LineEnding,
    pub checksum: Checksum,
    pub skip_head: usize,
    pub skip_tail: usize,
}
impl Default for SendOptions {
    fn default() -> Self {
        Self {
            hex: false,
            encoding: TextEncoding::Utf8,
            escapes: false,
            ending: LineEnding::None,
            checksum: Checksum::None,
            skip_head: 0,
            skip_tail: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "PresetCompat")]
pub struct Preset {
    pub send_count: u64,
    pub name: String,
    pub text: String,
    pub hex: bool,
    pub delay_ms: u64,
}
impl Default for Preset {
    fn default() -> Self {
        Self {
            send_count: 1,
            name: String::new(),
            text: String::new(),
            hex: false,
            delay_ms: 1000,
        }
    }
}

// Old JSON used an enabled checkbox; preserve disabled rows as count 0.
#[derive(Deserialize)]
#[serde(default)]
struct PresetCompat {
    enabled: Option<bool>,
    send_count: Option<u64>,
    name: String,
    text: String,
    hex: bool,
    delay_ms: u64,
}
impl Default for PresetCompat {
    fn default() -> Self {
        Self {
            enabled: None,
            send_count: None,
            name: String::new(),
            text: String::new(),
            hex: false,
            delay_ms: 1000,
        }
    }
}
impl From<PresetCompat> for Preset {
    fn from(p: PresetCompat) -> Self {
        Self {
            send_count: p
                .send_count
                .unwrap_or(u64::from(p.enabled.unwrap_or(true)))
                .min(1_000_000),
            name: p.name,
            text: p.text,
            hex: p.hex,
            delay_ms: p.delay_ms.clamp(10, 3_600_000),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub connection: ConnectionConfig,
    pub send: SendOptions,
    pub text: String,
    pub interval_ms: u64,
    pub single_send_count: u64,
    pub multi_send_count: u64,
    pub multi_repeat: bool,
    pub connection_width: f32,
    pub commands_width: f32,
    pub send_height: f32,
    pub rx_hex: bool,
    pub timestamps: bool,
    pub show_tx: bool,
    pub presets: Vec<Preset>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            connection: ConnectionConfig::default(),
            send: SendOptions::default(),
            text: "Hello, serial!".into(),
            interval_ms: 1000,
            single_send_count: 0,
            multi_send_count: 0,
            multi_repeat: true,
            connection_width: 250.0,
            commands_width: 470.0,
            send_height: 280.0,
            rx_hex: false,
            timestamps: true,
            show_tx: true,
            presets: vec![
                Preset {
                    name: "AT 测试".into(),
                    text: "AT".into(),
                    ..Preset::default()
                },
                Preset {
                    name: "Modbus 读寄存器".into(),
                    text: "01 03 00 00 00 02".into(),
                    hex: true,
                    ..Preset::default()
                },
            ],
        }
    }
}

pub fn settings_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("RustSerialAssistant")
        .join("settings.json")
}
pub fn load(path: &Path) -> Result<Settings, String> {
    if !path.exists() {
        return Ok(Settings::default());
    }
    let bytes = fs::read(path).map_err(|e| format!("读取配置失败：{e}"))?;
    let mut settings: Settings =
        serde_json::from_slice(&bytes).map_err(|e| format!("配置格式错误：{e}"))?;
    settings.interval_ms = settings.interval_ms.clamp(10, 3_600_000);
    settings.connection.receive_gap_ms = settings.connection.receive_gap_ms.clamp(1, 1000);
    settings.presets.truncate(100);
    for p in &mut settings.presets {
        p.delay_ms = p.delay_ms.clamp(10, 3_600_000);
    }
    Ok(settings)
}
pub fn save(path: &Path, settings: &Settings) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?;
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, bytes).map_err(|e| e.to_string())?;
    fs::rename(&temp, path).map_err(|e| format!("保存配置失败：{e}"))
}

/// Import only the numbered command bank; never executes content from an INI file.
pub fn import_sscom(bytes: &[u8]) -> Result<Vec<Preset>, String> {
    let text = match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => encoding_rs::GBK.decode(bytes).0.into_owned(),
    };
    let mut rows = std::collections::BTreeMap::<usize, Preset>::new();
    for line in text.lines() {
        let Some((key, value)) = line.trim().split_once('=') else {
            continue;
        };
        let Some(index) = key.strip_prefix('N').and_then(|s| s.parse::<usize>().ok()) else {
            continue;
        };
        if (1..=100).contains(&index) {
            if let Some((kind, payload)) = value.split_once(',')
                && (kind == "A" || kind == "H")
            {
                let row = rows.entry(index).or_default();
                row.text = payload.into();
                row.hex = kind == "H";
            }
        } else if (101..=200).contains(&index) {
            let mut parts = value.splitn(3, ',');
            let enabled = parts.next().unwrap_or("0") != "0";
            let name = parts.next().unwrap_or("").to_owned();
            let delay = parts
                .next()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(1000)
                .clamp(10, 3_600_000);
            let row = rows.entry(index - 100).or_default();
            row.send_count = u64::from(enabled);
            row.name = name;
            row.delay_ms = delay;
        }
    }
    let rows: Vec<_> = rows.into_values().filter(|r| !r.text.is_empty()).collect();
    if rows.is_empty() {
        Err("没有找到 SSCOM 多条指令（N1=A,... / N1=H,...）".into())
    } else {
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn imports_sscom_bank_without_executing_values() {
        let rows =
            import_sscom(b"N101=1,AT,250\r\nN1=A,AT+X=1,2\r\nN2=H,01 FF\r\nN102=0,hex,20").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].text, "AT+X=1,2");
        assert_eq!(rows[0].delay_ms, 250);
        assert_eq!(rows[1].send_count, 0);
        assert!(rows[1].hex);
    }
    #[test]
    fn settings_can_be_saved_twice_and_loaded() {
        let path =
            std::env::temp_dir().join(format!("serial-settings-{}.json", std::process::id()));
        let mut s = Settings::default();
        save(&path, &s).unwrap();
        s.connection.baud = 9600;
        s.text = "中文参数".into();
        save(&path, &s).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.connection.baud, 9600);
        assert_eq!(loaded.text, "中文参数");
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn legacy_enabled_rows_migrate_and_explicit_counts_take_precedence() {
        let rows:Vec<Preset>=serde_json::from_str(r#"[{"text":"A","enabled":true},{"text":"B","enabled":false},{"text":"C","enabled":false,"send_count":3},{"text":"D"}]"#).unwrap();
        assert_eq!(
            rows.iter().map(|p| p.send_count).collect::<Vec<_>>(),
            [1, 0, 3, 1]
        );
        let saved = serde_json::to_string(&rows).unwrap();
        assert!(!saved.contains("enabled"));
        let restored: Vec<Preset> = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored[2].send_count, 3);
    }
}
