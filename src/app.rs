use crate::layout::{Axis, Splitter};
use eframe::egui::{self, Color32, RichText, TextEdit};
use serial_assistant::{
    config::{self, Mode, Preset, Settings},
    protocol::{self, Checksum, LineEnding, StreamDecoder, TextEncoding},
    transport::{Command, Direction, Event, Session, Step},
};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

const INK: Color32 = Color32::from_rgb(27, 41, 62);
const MUTED: Color32 = Color32::from_rgb(111, 125, 145);
const ACCENT: Color32 = Color32::from_rgb(27, 107, 224);
const GREEN: Color32 = Color32::from_rgb(16, 153, 117);
const RED: Color32 = Color32::from_rgb(210, 64, 72);
const BORDER: Color32 = Color32::from_rgb(225, 231, 239);
const PAPER: Color32 = Color32::from_rgb(247, 249, 252);
const TERMINAL: Color32 = Color32::from_rgb(16, 25, 40);
const LIMIT_BYTES: usize = 4 * 1024 * 1024;
const LIMIT_ROWS: usize = 3000;
type PortList = Vec<(String, String)>;
type PortScanResult = Result<PortList, String>;

#[derive(Clone)]
struct LogEntry {
    direction: Option<Direction>,
    at: String,
    bytes: Vec<u8>,
    text: String,
    hex: String,
}
impl LogEntry {
    fn message(text: impl Into<String>) -> Self {
        Self {
            direction: None,
            at: chrono::Local::now().format("%H:%M:%S%.3f").to_string(),
            bytes: vec![],
            text: text.into(),
            hex: String::new(),
        }
    }
}
#[derive(PartialEq, Clone, Copy)]
enum SendTab {
    Single,
    File,
}

pub struct SerialApp {
    settings: Settings,
    path: PathBuf,
    save_allowed: bool,
    session: Option<Session>,
    ready: bool,
    disconnecting: bool,
    status: String,
    notice: String,
    error: bool,
    ports: PortList,
    scan: Option<Receiver<PortScanResult>>,
    logs: VecDeque<LogEntry>,
    log_bytes: usize,
    frozen: Option<VecDeque<LogEntry>>,
    rx_decoder: StreamDecoder,
    tx_decoder: StreamDecoder,
    last_encoding: TextEncoding,
    filter: String,
    follow: bool,
    detail: Option<LogEntry>,
    tab: SendTab,
    sending: bool,
    schedule_progress: (u64, Option<u64>),
    schedule_is_multi: bool,
    selected_preset: Option<usize>,
    comment_preset: Option<usize>,
    progress: (u64, u64),
    file_path: Option<PathBuf>,
    file_chunk: usize,
    file_delay: u64,
    capture: bool,
    capture_pending: bool,
    capture_path: Option<PathBuf>,
    last_stats: (u64, u64, u64),
    rate_at: Instant,
    rate_rx: u64,
    rate_tx: u64,
    rates: (u64, u64),
    help: bool,
    last_save: Instant,
}

impl SerialApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        configure_ui(&cc.egui_ctx);
        let path = config::settings_path();
        let (settings, notice, error, save_allowed) = match config::load(&path) {
            Ok(s) => (s, "准备就绪，选择端口后打开连接".into(), false, true),
            Err(e) => (
                Settings::default(),
                format!("{e}；当前使用默认配置，原文件保留。可点击“保存参数”覆盖。"),
                true,
                false,
            ),
        };
        let encoding = settings.send.encoding;
        let mut app = Self {
            settings,
            path,
            save_allowed,
            session: None,
            ready: false,
            disconnecting: false,
            status: "未连接".into(),
            notice,
            error,
            ports: vec![],
            scan: None,
            logs: VecDeque::new(),
            log_bytes: 0,
            frozen: None,
            rx_decoder: StreamDecoder::new(encoding),
            tx_decoder: StreamDecoder::new(encoding),
            last_encoding: encoding,
            filter: String::new(),
            follow: true,
            detail: None,
            tab: SendTab::Single,
            sending: false,
            schedule_progress: (0, None),
            schedule_is_multi: false,
            selected_preset: None,
            comment_preset: None,
            progress: (0, 0),
            file_path: None,
            file_chunk: 1024,
            file_delay: 10,
            capture: false,
            capture_pending: false,
            capture_path: None,
            last_stats: (0, 0, 0),
            rate_at: Instant::now(),
            rate_rx: 0,
            rate_tx: 0,
            rates: (0, 0),
            help: false,
            last_save: Instant::now(),
        };
        app.append(LogEntry::message(
            "欢迎使用串口助手。串口 / TCP / UDP，专注每一次收发。",
        ));
        app.refresh_ports();
        app
    }
    fn inform(&mut self, text: impl Into<String>, error: bool) {
        self.notice = text.into();
        self.error = error;
    }
    fn refresh_ports(&mut self) {
        if self.scan.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.scan = Some(rx);
        std::thread::spawn(move || {
            let result = serialport::available_ports()
                .map(|ports| {
                    let mut ports: Vec<_> = ports
                        .into_iter()
                        .map(|p| {
                            let description = match p.port_type {
                                serialport::SerialPortType::UsbPort(info) => {
                                    info.product.unwrap_or_else(|| {
                                        format!("USB {:04X}:{:04X}", info.vid, info.pid)
                                    })
                                }
                                serialport::SerialPortType::BluetoothPort => "蓝牙串口".into(),
                                _ => "串行端口".into(),
                            };
                            (p.port_name, description)
                        })
                        .collect();
                    ports.sort_by(|a, b| a.0.len().cmp(&b.0.len()).then(a.0.cmp(&b.0)));
                    ports
                })
                .map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
    }
    fn command(&mut self, command: Command) -> bool {
        let result = self
            .session
            .as_ref()
            .ok_or_else(|| "请先打开连接".to_owned())
            .and_then(|s| s.send(command));
        if let Err(e) = result {
            self.inform(e, true);
            false
        } else {
            true
        }
    }
    fn connect(&mut self) {
        self.last_stats = (0, 0, 0);
        self.rate_rx = 0;
        self.rate_tx = 0;
        self.rates = (0, 0);
        self.rate_at = Instant::now();
        self.rx_decoder = StreamDecoder::new(self.settings.send.encoding);
        self.tx_decoder = StreamDecoder::new(self.settings.send.encoding);
        self.status = "正在连接…".into();
        self.disconnecting = false;
        self.ready = false;
        self.session = Some(Session::start(self.settings.connection.clone()));
        self.inform("正在打开连接", false);
    }
    fn disconnect(&mut self) {
        if let Some(s) = &self.session {
            s.disconnect();
        }
        self.disconnecting = true;
        self.ready = false;
        self.status = "正在关闭并保存接收文件…".into();
    }
    fn append(&mut self, entry: LogEntry) {
        self.log_bytes += entry.bytes.len() + entry.text.len() + entry.hex.len();
        self.logs.push_back(entry);
        while self.log_bytes > LIMIT_BYTES || self.logs.len() > LIMIT_ROWS {
            if let Some(old) = self.logs.pop_front() {
                self.log_bytes = self
                    .log_bytes
                    .saturating_sub(old.bytes.len() + old.text.len() + old.hex.len());
            }
        }
    }
    fn poll(&mut self) {
        if let Some(rx) = &self.scan {
            match rx.try_recv() {
                Ok(result) => {
                    self.scan = None;
                    match result {
                        Ok(p) => {
                            self.ports = p;
                            if self.settings.connection.port.is_empty()
                                && let Some((name, _)) = self.ports.first()
                            {
                                self.settings.connection.port = name.clone();
                            }
                        }
                        Err(e) => self.inform(format!("枚举串口失败：{e}"), true),
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => self.scan = None,
                _ => (),
            }
        }
        let mut events = Vec::new();
        if let Some(s) = &self.session {
            events.extend(s.events.try_iter().take(512));
            self.last_stats = s.stats.snapshot();
        }
        for event in events {
            match event {
                Event::Ready(text) => {
                    self.ready = true;
                    self.status = text.clone();
                    self.append(LogEntry::message(format!("连接成功 · {text}")));
                    self.inform("连接成功", false);
                }
                Event::Waiting(text) => {
                    self.status = text.clone();
                    self.append(LogEntry::message(text));
                    self.inform("正在等待客户端连接", false);
                }
                Event::Closed => {
                    self.session = None;
                    self.ready = false;
                    self.disconnecting = false;
                    self.sending = false;
                    self.capture = false;
                    self.capture_pending = false;
                    self.status = "未连接".into();
                    self.rates = (0, 0);
                    self.append(LogEntry::message("连接已关闭"));
                }
                Event::Data {
                    direction,
                    bytes,
                    at,
                } => {
                    let text = if direction == Direction::Rx {
                        self.rx_decoder.push(&bytes)
                    } else {
                        self.tx_decoder.push(&bytes)
                    };
                    let at: chrono::DateTime<chrono::Local> = at.into();
                    self.append(LogEntry {
                        direction: Some(direction),
                        at: at.format("%H:%M:%S%.3f").to_string(),
                        hex: protocol::hex(&bytes),
                        bytes,
                        text,
                    });
                }
                Event::Error(e) => {
                    self.append(LogEntry::message(format!("错误 · {e}")));
                    self.inform(e, true);
                }
                Event::Info(text) => {
                    self.append(LogEntry::message(text.clone()));
                    self.inform(text, false);
                }
                Event::SendingStopped => self.sending = false,
                Event::ScheduleProgress { completed, total } => {
                    self.schedule_progress = (completed, total)
                }
                Event::Progress { sent, total } => self.progress = (sent, total),
                Event::Capture(active) => {
                    self.capture = active;
                    self.capture_pending = false;
                    if active || !self.error {
                        self.inform(
                            if active {
                                "接收数据正在原样写入文件"
                            } else {
                                "接收文件已停止保存"
                            },
                            false,
                        );
                    }
                }
            }
        }
        if self.last_encoding != self.settings.send.encoding {
            self.last_encoding = self.settings.send.encoding;
            self.rx_decoder = StreamDecoder::new(self.last_encoding);
            self.tx_decoder = StreamDecoder::new(self.last_encoding);
            for row in &mut self.logs {
                if let Some(direction) = row.direction {
                    row.text = if direction == Direction::Rx {
                        self.rx_decoder.push(&row.bytes)
                    } else {
                        self.tx_decoder.push(&row.bytes)
                    };
                }
            }
            self.log_bytes = self
                .logs
                .iter()
                .map(|row| row.bytes.len() + row.text.len() + row.hex.len())
                .sum();
        }
        let elapsed = self.rate_at.elapsed().as_secs_f64();
        if elapsed >= 1.0 {
            self.rates = (
                ((self.last_stats.0.saturating_sub(self.rate_rx)) as f64 / elapsed) as u64,
                ((self.last_stats.1.saturating_sub(self.rate_tx)) as f64 / elapsed) as u64,
            );
            self.rate_rx = self.last_stats.0;
            self.rate_tx = self.last_stats.1;
            self.rate_at = Instant::now();
        }
    }
    fn save_settings(&mut self, explicit: bool) {
        if !self.save_allowed && !explicit {
            return;
        }
        match config::save(&self.path, &self.settings) {
            Ok(()) => {
                self.save_allowed = true;
                if explicit {
                    self.inform("连接参数与指令已保存", false);
                }
                self.last_save = Instant::now();
            }
            Err(e) => self.inform(e, true),
        }
    }
    fn send_single(&mut self, timed: bool) {
        match protocol::build_packet(&self.settings.text, &self.settings.send) {
            Ok(bytes) => {
                if timed {
                    let delay = Duration::from_millis(self.settings.interval_ms.max(10));
                    let cycles = (self.settings.single_send_count > 0)
                        .then_some(self.settings.single_send_count);
                    self.schedule_is_multi = false;
                    self.schedule_progress = (0, cycles);
                    self.sending = self.command(Command::Schedule {
                        steps: vec![Step {
                            count: 1,
                            bytes,
                            delay,
                        }],
                        cycles,
                    });
                } else {
                    self.command(Command::Send(bytes));
                }
            }
            Err(e) => self.inform(e, true),
        }
    }
    fn send_preset(&mut self, index: usize) {
        let p = &self.settings.presets[index];
        if p.send_count == 0 {
            self.inform("该指令发送次数为 0，请先设置次数", true);
            return;
        }
        let count = p.send_count;
        let delay = Duration::from_millis(p.delay_ms.max(10));
        let mut options = self.settings.send.clone();
        options.hex = p.hex;
        match protocol::build_packet(&p.text, &options) {
            Ok(bytes) => {
                if count == 1 {
                    self.command(Command::Send(bytes));
                } else {
                    self.schedule_is_multi = false;
                    self.schedule_progress = (0, Some(count));
                    self.sending = self.command(Command::Schedule {
                        steps: vec![Step {
                            count: 1,
                            bytes,
                            delay,
                        }],
                        cycles: Some(count),
                    });
                }
            }
            Err(e) => self.inform(format!("第 {} 条指令：{e}", index + 1), true),
        }
    }
    fn send_bank(&mut self) {
        let mut steps = Vec::new();
        for (index, p) in self
            .settings
            .presets
            .iter()
            .enumerate()
            .filter(|(_, p)| p.send_count > 0 && !p.text.is_empty())
        {
            let mut options = self.settings.send.clone();
            options.hex = p.hex;
            match protocol::build_packet(&p.text, &options) {
                Ok(bytes) => steps.push(Step {
                    count: p.send_count,
                    bytes,
                    delay: Duration::from_millis(p.delay_ms.max(10)),
                }),
                Err(e) => {
                    self.inform(format!("第 {} 条指令：{e}", index + 1), true);
                    return;
                }
            }
        }
        if steps.is_empty() {
            self.inform("请设置至少一条有内容且发送次数大于 0 的指令", true);
            return;
        }
        let cycles = if self.settings.multi_repeat {
            (self.settings.multi_send_count > 0).then_some(self.settings.multi_send_count)
        } else {
            Some(1)
        };
        self.schedule_is_multi = true;
        self.schedule_progress = (0, cycles);
        self.sending = self.command(Command::Schedule { steps, cycles });
    }
    fn toggle_capture(&mut self) {
        if self.capture {
            self.capture_pending = self.command(Command::StopCapture);
        } else if let Some(path) = rfd::FileDialog::new()
            .set_title("选择新的原始接收文件（已有文件不会被覆盖）")
            .set_file_name(format!(
                "RX-{}.bin",
                chrono::Local::now().format("%Y%m%d-%H%M%S")
            ))
            .save_file()
        {
            self.capture_path = Some(path.clone());
            self.capture_pending = self.command(Command::StartCapture(path));
        }
    }
    fn export_log(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("导出当前缓存的收发日志")
            .add_filter("文本日志", &["txt"])
            .set_file_name("serial-log.txt")
            .save_file()
        {
            let mut text = String::new();
            for row in &self.logs {
                text.push_str(&format!(
                    "[{}] {:3} [{} B] {}\n",
                    row.at,
                    match row.direction {
                        Some(Direction::Rx) => "RX",
                        Some(Direction::Tx) => "TX",
                        None => "SYS",
                    },
                    row.bytes.len(),
                    if self.settings.rx_hex && row.direction.is_some() {
                        &row.hex
                    } else {
                        &row.text
                    }
                ));
            }
            match std::fs::write(path, text) {
                Ok(()) => self.inform("日志已导出（仅包含当前缓存）", false),
                Err(e) => self.inform(format!("导出失败：{e}"), true),
            }
        }
    }
    fn import_bank(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("指令文件", &["json", "ini"])
            .pick_file()
        {
            let result = std::fs::read(&path)
                .map_err(|e| e.to_string())
                .and_then(|bytes| {
                    if path
                        .extension()
                        .is_some_and(|x| x.eq_ignore_ascii_case("ini"))
                    {
                        config::import_sscom(&bytes)
                    } else {
                        serde_json::from_slice::<Vec<Preset>>(&bytes).map_err(|e| e.to_string())
                    }
                });
            match result {
                Ok(mut rows) => {
                    rows.truncate(100);
                    for p in &mut rows {
                        p.delay_ms = p.delay_ms.clamp(10, 3_600_000);
                    }
                    self.settings.presets = rows;
                    self.inform("指令库已导入，可编辑后发送", false);
                }
                Err(e) => self.inform(format!("导入失败：{e}"), true),
            }
        }
    }
    fn export_bank(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("JSON 指令库", &["json"])
            .set_file_name("commands.json")
            .save_file()
        {
            let result = serde_json::to_vec_pretty(&self.settings.presets)
                .map_err(|e| e.to_string())
                .and_then(|data| std::fs::write(path, data).map_err(|e| e.to_string()));
            match result {
                Ok(()) => self.inform("指令库已导出", false),
                Err(e) => self.inform(e, true),
            }
        }
    }

    fn header(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(Color32::WHITE)
                    .inner_margin(egui::Margin::symmetric(20, 14)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    egui::Frame::new()
                        .fill(ACCENT)
                        .corner_radius(8)
                        .inner_margin(8.0)
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new("S/")
                                    .color(Color32::WHITE)
                                    .strong()
                                    .size(20.0),
                            );
                        });
                    ui.add_space(8.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new("串口助手").strong().size(21.0));
                        ui.label(RichText::new("RUST SERIAL STUDIO").size(10.0).color(MUTED));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("使用说明").clicked() {
                            self.help = true;
                        }
                        if ui.button("保存参数").clicked() {
                            self.save_settings(true);
                        }
                        ui.add_space(12.0);
                        ui.label(
                            RichText::new(if self.ready {
                                "● 已连接"
                            } else if self.session.is_some() {
                                "● 连接中"
                            } else {
                                "● 未连接"
                            })
                            .color(if self.ready {
                                GREEN
                            } else {
                                MUTED
                            }),
                        );
                    });
                });
            });
    }
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new().fill(Color32::WHITE).inner_margin(14.0).show(ui,|ui| {
            egui::ScrollArea::vertical().id_salt("connection_scroll").show(ui, |ui| {
            ui.label(RichText::new("连接设置").strong().size(17.0)); ui.add_space(14.0);
            let connected=self.session.is_some();
            ui.add_enabled_ui(!connected,|ui| {
                field_label(ui,"通信方式");
                egui::ComboBox::from_id_salt("mode").width(ui.available_width().min(210.0)).selected_text(self.settings.connection.mode.label()).show_ui(ui,|ui| {
                    for m in Mode::ALL {ui.selectable_value(&mut self.settings.connection.mode,m,m.label());}
                }); ui.add_space(12.0);
                match self.settings.connection.mode {
                    Mode::Serial=>{
                        ui.horizontal(|ui| {field_label(ui,"串口端口");ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui|{if ui.add_enabled(self.scan.is_none(),egui::Button::new("刷新").small()).clicked(){self.refresh_ports();}});});
                        egui::ComboBox::from_id_salt("ports").width(ui.available_width().min(210.0)).selected_text(if self.settings.connection.port.is_empty(){"选择串口"}else{&self.settings.connection.port}).show_ui(ui,|ui| {
                            for (name,description) in &self.ports {ui.selectable_value(&mut self.settings.connection.port,name.clone(),format!("{name} · {description}"));}
                        });
                        ui.add(TextEdit::singleline(&mut self.settings.connection.port).desired_width(ui.available_width().min(210.0)).hint_text("也可手动输入 COM3"));
                        if self.ports.is_empty() {ui.label(RichText::new("未检测到串口，可连接 USB 串口后刷新").size(11.0).color(MUTED));}
                        ui.add_space(10.0);field_label(ui,"波特率");
                        ui.horizontal(|ui| {
                            ui.add(egui::DragValue::new(&mut self.settings.connection.baud).range(1..=12_000_000).speed(100.0));
                            egui::ComboBox::from_id_salt("baud").width(90.0).selected_text("常用值").show_ui(ui,|ui| {for b in [1200,2400,4800,9600,19200,38400,57600,115200,230400,460800,921600,1_000_000,2_000_000] {if ui.selectable_label(self.settings.connection.baud==b,b.to_string()).clicked(){self.settings.connection.baud=b;}}});
                        });ui.add_space(12.0);
                        egui::Grid::new("serial_options").spacing([16.0,9.0]).show(ui,|ui| {
                            ui.label("数据位");egui::ComboBox::from_id_salt("bits").width(110.0).selected_text(self.settings.connection.data_bits.to_string()).show_ui(ui,|ui|{for b in [5,6,7,8]{ui.selectable_value(&mut self.settings.connection.data_bits,b,b.to_string());}});ui.end_row();
                            ui.label("校验位");str_combo(ui,"parity",&mut self.settings.connection.parity,&["None","Odd","Even"]);ui.end_row();
                            ui.label("停止位");egui::ComboBox::from_id_salt("stop").width(110.0).selected_text(self.settings.connection.stop_bits.to_string()).show_ui(ui,|ui|{for b in [1,2]{ui.selectable_value(&mut self.settings.connection.stop_bits,b,b.to_string());}});ui.end_row();
                            ui.label("流控制");str_combo(ui,"flow",&mut self.settings.connection.flow,&["None","Hardware","Software"]);ui.end_row();
                        });
                    }
                    Mode::TcpClient=>{field_label(ui,"远端地址与端口");ui.add(TextEdit::singleline(&mut self.settings.connection.remote).desired_width(ui.available_width().min(210.0)));hint(ui,"例如 192.168.1.10:8080\n支持域名，连接超时为每个地址 3 秒");}
                    Mode::TcpServer=>{field_label(ui,"本地监听地址");ui.add(TextEdit::singleline(&mut self.settings.connection.local).desired_width(ui.available_width().min(210.0)));hint(ui,"默认仅在本机监听\n每次接受一个客户端，断开后需重新打开");}
                    Mode::Udp=>{field_label(ui,"本地绑定地址");ui.add(TextEdit::singleline(&mut self.settings.connection.local).desired_width(ui.available_width().min(210.0)));ui.add_space(12.0);field_label(ui,"远端地址与端口");ui.add(TextEdit::singleline(&mut self.settings.connection.remote).desired_width(ui.available_width().min(210.0)));hint(ui,"只接收所填远端的数据报\n本地端口填 0 可自动分配");}
                    Mode::Loopback=>{egui::Frame::new().fill(PAPER).inner_margin(12.0).corner_radius(6).show(ui,|ui|{ui.label(RichText::new("无需设备，立即体验").strong());hint(ui,"发送的每个字节都会在本地原样返回。可验证 HEX、校验、循环与文件收发。");});}
                }
            });
            if self.settings.connection.mode==Mode::Serial {
                ui.add_space(12.0);ui.horizontal(|ui| {
                    let dtr=ui.checkbox(&mut self.settings.connection.dtr,"DTR").changed();
                    let rts=ui.add_enabled(self.settings.connection.flow!="Hardware",egui::Checkbox::new(&mut self.settings.connection.rts,"RTS")).changed();
                    if (dtr||rts)&&self.ready {self.command(Command::SetLines {dtr:self.settings.connection.dtr,rts:self.settings.connection.rts});}
                });
            }
            ui.add_space(18.0);
            if ui.add_enabled(!self.disconnecting,egui::Button::new(RichText::new(if connected {"关闭连接"}else{"打开连接"}).color(Color32::WHITE).strong()).fill(if connected {RED}else{ACCENT}).min_size(egui::vec2(ui.available_width(),38.0))).clicked() {if connected {self.disconnect();}else{self.connect();}}
            ui.add_space(10.0);ui.label(RichText::new(&self.status).size(12.0).color(if self.ready{GREEN}else{MUTED}));
            ui.add_space(22.0);ui.separator();ui.add_space(12.0);
            field_label(ui,"本次连接");ui.add_space(8.0);
            metric(ui,"接收 RX",self.last_stats.0,GREEN);metric(ui,"发送 TX",self.last_stats.1,ACCENT);
            ui.add_space(8.0);ui.horizontal(|ui|{ui.label(RichText::new(format!("↓ {} B/s",self.rates.0)).size(11.0).color(MUTED));ui.label(RichText::new(format!("↑ {} B/s",self.rates.1)).size(11.0).color(MUTED));});
            if ui.small_button("计数归零").clicked(){if let Some(s)=&self.session{s.stats.reset();}self.last_stats=(0,0,0);self.rate_rx=0;self.rate_tx=0;self.rates=(0,0);}
            ui.add_space(20.0);hint(ui,"Ctrl + Enter  发送当前内容\nCtrl + L  清空接收窗口\n参数与指令自动保存到本机");
            });
        });
    }
    fn footer(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("footer")
            .frame(
                egui::Frame::new()
                    .fill(Color32::WHITE)
                    .inner_margin(egui::Margin::symmetric(16, 8)),
            )
            .show(ctx, |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(concat!(
                            "v",
                            env!("CARGO_PKG_VERSION"),
                            " · Rust  |  Designed by liusonglin, Powered by Codex"
                        ))
                        .size(10.0)
                        .color(MUTED),
                    );
                    ui.allocate_ui_with_layout(
                        ui.available_size(),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.label(
                                RichText::new(if self.error { "!" } else { "●" })
                                    .color(if self.error { RED } else { GREEN }),
                            );
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&self.notice).size(12.0).color(if self.error {
                                        RED
                                    } else {
                                        MUTED
                                    }),
                                )
                                .truncate(),
                            )
                            .on_hover_text(&self.notice);
                        },
                    );
                });
            });
    }
    fn send_panel(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new()
            .fill(Color32::WHITE)
            .inner_margin(12.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("发送工作区").strong().size(16.0));
                    ui.add_space(18.0);
                    ui.selectable_value(&mut self.tab, SendTab::Single, "单条发送");
                    ui.selectable_value(&mut self.tab, SendTab::File, "文件发送");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(self.sending, egui::Button::new("停止发送"))
                            .clicked()
                        {
                            self.command(Command::StopSending);
                        }
                        if self.sending {
                            ui.label(RichText::new("● 发送中").color(GREEN));
                        }
                    });
                });
                ui.add_space(10.0);
                if self.tab != SendTab::File {
                    ui.push_id("single_options", |ui| self.send_options(ui, false));
                    ui.add_space(8.0);
                }
                match self.tab {
                    SendTab::Single => self.single_ui(ui),
                    SendTab::File => self.file_ui(ui),
                }
            });
    }
    fn send_options(&mut self, ui: &mut egui::Ui, for_presets: bool) {
        ui.horizontal_wrapped(|ui| {
            if !for_presets {
                ui.checkbox(&mut self.settings.send.hex, "HEX 发送");
            } else {
                ui.label(RichText::new("共享选项").size(11.0).color(MUTED));
            }
            egui::ComboBox::from_id_salt("encoding")
                .width(65.0)
                .selected_text(self.settings.send.encoding.label())
                .show_ui(ui, |ui| {
                    for e in TextEncoding::ALL {
                        ui.selectable_value(&mut self.settings.send.encoding, e, e.label());
                    }
                });
            ui.add_enabled(
                for_presets || !self.settings.send.hex,
                egui::Checkbox::new(&mut self.settings.send.escapes, "转义符"),
            )
            .on_hover_text("支持 \\r \\n \\t \\0 \\xFF 和 \\\\");
            egui::ComboBox::from_id_salt("ending")
                .width(72.0)
                .selected_text(self.settings.send.ending.label())
                .show_ui(ui, |ui| {
                    for e in LineEnding::ALL {
                        ui.selectable_value(&mut self.settings.send.ending, e, e.label());
                    }
                });
            ui.separator();
            egui::ComboBox::from_id_salt("checksum")
                .width(145.0)
                .selected_text(self.settings.send.checksum.label())
                .show_ui(ui, |ui| {
                    for c in Checksum::ALL {
                        ui.selectable_value(&mut self.settings.send.checksum, c, c.label());
                    }
                });
            if self.settings.send.checksum != Checksum::None {
                ui.label("排除头");
                ui.add(
                    egui::DragValue::new(&mut self.settings.send.skip_head)
                        .range(0..=65535)
                        .suffix(" B"),
                );
                ui.label("尾");
                ui.add(
                    egui::DragValue::new(&mut self.settings.send.skip_tail)
                        .range(0..=65535)
                        .suffix(" B"),
                );
            }
        });
    }
    fn single_ui(&mut self, ui: &mut egui::Ui) {
        let available = (ui.available_height() - 100.0).max(42.0);
        ui.add_enabled_ui(!self.sending, |ui| {
            ui.add_sized(
                [ui.available_width(), available],
                TextEdit::multiline(&mut self.settings.text)
                    .font(egui::TextStyle::Monospace)
                    .hint_text("输入文本，或勾选 HEX 后输入 01 03 00 00 00 02"),
            );
        });
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            let can = self.ready && !self.sending;
            if primary_button(ui, "发送  Ctrl+Enter", can).clicked() {
                self.send_single(false);
            }
            ui.add_space(12.0);
            ui.label("间隔");
            ui.add(
                egui::DragValue::new(&mut self.settings.interval_ms)
                    .range(10..=3_600_000)
                    .suffix(" ms")
                    .speed(10.0),
            );
            ui.label("发送次数");
            ui.add_enabled(
                !self.sending,
                egui::DragValue::new(&mut self.settings.single_send_count).range(0..=1_000_000),
            )
            .on_hover_text("定时发送达到此次数自动停止；0 表示不限。手动点击发送始终只发送一次。");
            if ui.add_enabled(can, egui::Button::new("定时发送")).clicked() {
                self.send_single(true);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                match protocol::build_packet(&self.settings.text, &self.settings.send) {
                    Ok(b) => {
                        let preview = protocol::hex(&b[..b.len().min(64)]);
                        ui.label(
                            RichText::new(format!("{} B  ·  预览", b.len()))
                                .color(MUTED)
                                .size(11.0),
                        )
                        .on_hover_text(preview);
                    }
                    Err(e) => {
                        ui.label(RichText::new("检查发送格式").color(RED).size(11.0))
                            .on_hover_text(e);
                    }
                }
            });
        });
        if !self.schedule_is_multi && (self.sending || self.schedule_progress.0 > 0) {
            ui.label(
                RichText::new(format!(
                    "已发送 {} / {} 次",
                    self.schedule_progress.0,
                    self.schedule_progress
                        .1
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "不限".into())
                ))
                .size(11.0)
                .color(ACCENT),
            );
        }
    }
    fn workspace(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                let r = ui.max_rect();
                ui.painter().rect_filled(r, 0.0, PAPER);
                let gap = 8.0;
                let left_max = (r.width() - 720.0).clamp(190.0, 380.0);
                self.settings.connection_width =
                    self.settings.connection_width.clamp(190.0, left_max);
                let right_max =
                    (r.width() - self.settings.connection_width - 340.0 - 2.0 * gap).max(350.0);
                self.settings.commands_width = self.settings.commands_width.clamp(350.0, right_max);
                let send_max = (r.height() - 190.0 - gap).max(190.0);
                self.settings.send_height = self.settings.send_height.clamp(190.0, send_max);
                let left = egui::Rect::from_min_max(
                    r.min,
                    egui::pos2(r.left() + self.settings.connection_width, r.bottom()),
                );
                let right = egui::Rect::from_min_max(
                    egui::pos2(r.right() - self.settings.commands_width, r.top()),
                    r.max,
                );
                let middle = egui::Rect::from_min_max(
                    egui::pos2(left.right() + gap, r.top()),
                    egui::pos2(right.left() - gap, r.bottom()),
                );
                let send = egui::Rect::from_min_max(
                    egui::pos2(middle.left(), r.bottom() - self.settings.send_height),
                    middle.max,
                );
                let receive = egui::Rect::from_min_max(
                    middle.min,
                    egui::pos2(middle.right(), send.top() - gap),
                );
                for (index, rect) in [left, right, receive, send].into_iter().enumerate() {
                    ui.painter().rect_filled(
                        rect,
                        0.0,
                        if index == 2 { PAPER } else { Color32::WHITE },
                    );
                    ui.scope_builder(
                        egui::UiBuilder::new()
                            .id_salt(("workspace_pane", index))
                            .max_rect(rect)
                            .layout(egui::Layout::top_down(egui::Align::Min)),
                        |ui| {
                            ui.set_clip_rect(rect);
                            ui.set_min_size(rect.size());
                            match index {
                                0 => self.sidebar(ui),
                                1 => self.presets_panel(ui),
                                2 => self.log_panel(ui),
                                _ => self.send_panel(ui),
                            }
                        },
                    );
                }
                Splitter {
                    rect: egui::Rect::from_min_max(
                        egui::pos2(left.right(), r.top()),
                        egui::pos2(middle.left(), r.bottom()),
                    ),
                    axis: Axis::Horizontal,
                    origin: r.left(),
                    reverse: false,
                    min: 190.0,
                    max: left_max,
                    reset: 250.0,
                }
                .show(
                    ui,
                    "connection_splitter",
                    &mut self.settings.connection_width,
                );
                Splitter {
                    rect: egui::Rect::from_min_max(
                        egui::pos2(middle.right(), r.top()),
                        egui::pos2(right.left(), r.bottom()),
                    ),
                    axis: Axis::Horizontal,
                    origin: r.right(),
                    reverse: true,
                    min: 350.0,
                    max: right_max,
                    reset: 470.0,
                }
                .show(ui, "commands_splitter", &mut self.settings.commands_width);
                Splitter {
                    rect: egui::Rect::from_min_max(
                        egui::pos2(middle.left(), receive.bottom()),
                        egui::pos2(middle.right(), send.top()),
                    ),
                    axis: Axis::Vertical,
                    origin: r.bottom(),
                    reverse: true,
                    min: 190.0,
                    max: send_max,
                    reset: 280.0,
                }
                .show(ui, "send_splitter", &mut self.settings.send_height);
            });
    }
    fn presets_panel(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new()
            .fill(Color32::WHITE)
            .inner_margin(8.0)
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                ui.horizontal(|ui| {
                    ui.label(RichText::new("多条指令").strong().size(16.0));
                    ui.label(RichText::new("↔ 拖动分隔条加宽").size(11.0).color(MUTED));
                    if ui.small_button("多条帮助").clicked() {
                        self.help = true;
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.add_enabled(
                        !self.sending,
                        egui::Checkbox::new(&mut self.settings.multi_repeat, "循环发送"),
                    );
                    ui.label("循环轮数");
                    ui.add_enabled(
                        !self.sending && self.settings.multi_repeat,
                        egui::DragValue::new(&mut self.settings.multi_send_count)
                            .range(0..=1_000_000)
                            .suffix(" 轮"),
                    )
                    .on_hover_text(
                        "0 表示无限循环；N 表示将发送次数大于 0 的指令从上到下执行 N 轮。未勾选循环时只执行一轮。",
                    );
                    ui.label(RichText::new("0=不限").size(10.0).color(MUTED));
                });
                ui.horizontal_wrapped(|ui| {
                    if primary_button(ui, "开始发送", self.ready && !self.sending).clicked() {
                        self.send_bank();
                    }
                    if ui
                        .add_enabled(self.sending, egui::Button::new("停止"))
                        .clicked()
                    {
                        self.command(Command::StopSending);
                    }
                    if ui
                        .add_enabled(!self.sending, egui::Button::new("导入 .ini / JSON"))
                        .clicked()
                    {
                        self.import_bank();
                        self.selected_preset = None;
                    }
                    if ui.button("导出").clicked() {
                        self.export_bank();
                    }
                });
                egui::CollapsingHeader::new("编码 / 换行 / 校验（与单条共享）")
                    .id_salt("compact_shared_options")
                    .default_open(false)
                    .show(ui, |ui| {
                        ui.push_id("preset_options", |ui| self.send_options(ui, true));
                    });
                if self.schedule_is_multi && (self.sending || self.schedule_progress.0 > 0) {
                    ui.label(
                        RichText::new(format!(
                            "已完成 {} / {} 轮",
                            self.schedule_progress.0,
                            self.schedule_progress
                                .1
                                .map(|n| n.to_string())
                                .unwrap_or_else(|| "不限".into())
                        ))
                        .size(11.0)
                        .color(ACCENT),
                    );
                }
                self.multi_ui(ui);
            });
    }
    fn multi_ui(&mut self, ui: &mut egui::Ui) {
        let editable = !self.sending;
        let mut send = None;
        let mut delete = None;
        let mut shift = None;
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("{}/100 条", self.settings.presets.len()))
                    .size(11.0)
                    .color(MUTED),
            );
            if ui
                .add_enabled(
                    editable && self.settings.presets.len() < 100,
                    egui::Button::new("＋").small(),
                )
                .on_hover_text("添加指令")
                .clicked()
            {
                self.settings.presets.push(Preset::default());
                self.selected_preset = Some(self.settings.presets.len() - 1);
            }
            if ui
                .add_enabled(
                    editable && self.selected_preset.is_some(),
                    egui::Button::new("－").small(),
                )
                .on_hover_text("删除选中指令")
                .clicked()
            {
                delete = self.selected_preset;
            }
            if ui
                .add_enabled(
                    editable && self.selected_preset.is_some_and(|i| i > 0),
                    egui::Button::new("↑").small(),
                )
                .clicked()
            {
                let i = self.selected_preset.unwrap();
                shift = Some((i, i - 1));
            }
            if ui
                .add_enabled(
                    editable
                        && self
                            .selected_preset
                            .is_some_and(|i| i + 1 < self.settings.presets.len()),
                    egui::Button::new("↓").small(),
                )
                .clicked()
            {
                let i = self.selected_preset.unwrap();
                shift = Some((i, i + 1));
            }
        });
        ui.add_space(4.0);
        let table_width = ui.available_width() - 12.0;
        let comment_width = (table_width * 0.27).clamp(85.0, 140.0);
        let text_width = (table_width - comment_width - 24.0 - 60.0 - 62.0 - 16.0).max(70.0);
        egui::ScrollArea::both()
            .id_salt("compact_presets")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(4.0, 2.0);
                ui.spacing_mut().button_padding = egui::vec2(3.0, 1.0);
                ui.spacing_mut().interact_size.y = 22.0;
                egui::Grid::new("compact_presets_table")
                    .striped(true)
                    .spacing([4.0, 2.0])
                    .min_col_width(0.0)
                    .show(ui, |ui| {
                        for (label, width) in [
                            ("HEX", 24.0),
                            ("字符串（双击注释）", text_width),
                            ("点击发送", comment_width),
                            ("发送次数", 60.0),
                            ("延时 ms", 62.0),
                        ] {
                            ui.add_sized(
                                [width, 22.0],
                                egui::Label::new(RichText::new(label).size(11.0)).truncate(),
                            );
                        }
                        ui.end_row();
                        for (i, p) in self.settings.presets.iter_mut().enumerate() {
                            let response =
                                ui.add_enabled(editable, egui::Checkbox::without_text(&mut p.hex));
                            if response.clicked() {
                                self.selected_preset = Some(i);
                            }
                            let response = ui.add_enabled(
                                editable,
                                TextEdit::singleline(&mut p.text)
                                    .id_salt(("compact_text", i))
                                    .desired_width(text_width)
                                    .min_size(egui::vec2(text_width, 22.0))
                                    .font(egui::FontId::monospace(12.0)),
                            );
                            if response.clicked() || response.has_focus() {
                                self.selected_preset = Some(i);
                            }
                            if response.double_clicked() {
                                self.comment_preset = Some(i);
                            }
                            response.context_menu(|ui| {
                                if ui.button("编辑注释").clicked() {
                                    self.comment_preset = Some(i);
                                    ui.close();
                                }
                            });
                            let label = if p.name.is_empty() {
                                format!("指令 {}", i + 1)
                            } else {
                                p.name.clone()
                            };
                            if ui
                                .add_enabled(
                                    self.ready && !self.sending,
                                    egui::Button::new(&label)
                                        .truncate()
                                        .min_size(egui::vec2(comment_width, 22.0)),
                                )
                                .on_hover_text(format!(
                                    "{label}\n按本条设置的发送次数执行；双击左侧字符串可编辑注释"
                                ))
                                .clicked()
                            {
                                send = Some(i);
                                self.selected_preset = Some(i);
                            }
                            let response=ui.add_enabled(editable,egui::DragValue::new(&mut p.send_count).range(0..=1_000_000));
                            if response.clicked() || response.has_focus() {self.selected_preset=Some(i);}
                            response.on_hover_text("本条指令连续发送 N 次后执行下一条；0 表示跳过。整组按从上到下执行。");
                            ui.add_enabled(
                                editable,
                                egui::DragValue::new(&mut p.delay_ms)
                                    .range(10..=3_600_000)
                                    .min_decimals(0),
                            );
                            ui.end_row();
                        }
                    });
            });
        if let Some(i) = send {
            self.send_preset(i);
        }
        if let Some(i) = delete.filter(|i| *i < self.settings.presets.len()) {
            self.settings.presets.remove(i);
            self.selected_preset = None;
            self.comment_preset = None;
        }
        if let Some((a, b)) = shift {
            self.settings.presets.swap(a, b);
            self.selected_preset = Some(b);
        }
    }
    fn file_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        egui::Frame::new()
            .fill(PAPER)
            .corner_radius(8)
            .inner_margin(14.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!self.sending, egui::Button::new("选择文件…"))
                        .clicked()
                    {
                        self.file_path = rfd::FileDialog::new().pick_file();
                    }
                    ui.add(
                        egui::Label::new(
                            self.file_path
                                .as_ref()
                                .map(|p| p.display().to_string())
                                .unwrap_or_else(|| "尚未选择文件".into()),
                        )
                        .truncate(),
                    );
                });
                ui.add_space(10.0);
                hint(
                    ui,
                    "文件按原始字节发送，不附加换行或校验。发送期间可随时停止。",
                );
            });
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.label("分块");
            ui.add_enabled(
                !self.sending,
                egui::DragValue::new(&mut self.file_chunk)
                    .range(1..=16_384)
                    .suffix(" B"),
            );
            ui.label("块间隔");
            ui.add_enabled(
                !self.sending,
                egui::DragValue::new(&mut self.file_delay)
                    .range(0..=60000)
                    .suffix(" ms"),
            );
            if primary_button(
                ui,
                "开始发送文件",
                self.ready && !self.sending && self.file_path.is_some(),
            )
            .clicked()
            {
                self.progress = (0, 0);
                self.sending = self.command(Command::SendFile {
                    path: self.file_path.clone().unwrap(),
                    chunk: self.file_chunk,
                    delay: Duration::from_millis(self.file_delay),
                });
            }
        });
        ui.add_space(10.0);
        let (sent, total) = self.progress;
        ui.add(
            egui::ProgressBar::new(if total > 0 {
                sent as f32 / total as f32
            } else {
                0.0
            })
            .text(format!("{} / {}", format_bytes(sent), format_bytes(total)))
            .desired_width(ui.available_width()),
        );
    }
    fn log_panel(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new().fill(PAPER).inner_margin(12.0).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("数据监视器").strong().size(17.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("清空").clicked() {
                            self.clear_logs();
                        }
                        if ui.button("导出日志").clicked() {
                            self.export_log();
                        }
                    });
                });
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    ui.checkbox(&mut self.settings.rx_hex, "HEX 显示");
                    ui.checkbox(&mut self.settings.timestamps, "时间戳");
                    ui.checkbox(&mut self.settings.show_tx, "显示发送");
                    ui.checkbox(&mut self.follow, "跟随最新");
                });
                ui.horizontal_wrapped(|ui| {
                    ui.add(
                        TextEdit::singleline(&mut self.filter)
                            .desired_width(160.0)
                            .hint_text("筛选内容 / HEX…"),
                    );
                    if ui
                        .small_button(if self.frozen.is_some() {
                            "恢复显示"
                        } else {
                            "暂停显示"
                        })
                        .clicked()
                    {
                        if self.frozen.is_some() {
                            self.frozen = None;
                        } else {
                            self.frozen = Some(self.logs.clone());
                        }
                    }
                    if matches!(self.settings.connection.mode, Mode::Serial | Mode::TcpClient | Mode::TcpServer) {
                        ui.label("分包间隔");
                        if ui.add(egui::DragValue::new(&mut self.settings.connection.receive_gap_ms).range(1..=1000).suffix(" ms"))
                            .on_hover_text("相邻接收数据在此间隔内合并显示；默认 20 ms。只影响显示，不改变原始文件和字节计数。连续数据最多每 200 ms 或 65536 B 显示一块（间隔大于 200 ms 时随之延长）。").changed() && self.session.is_some() {
                            self.command(Command::SetReceiveGap(self.settings.connection.receive_gap_ms));
                        }
                    }
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .add_enabled(
                                self.session.is_some()
                                    && !self.capture_pending
                                    && !self.disconnecting,
                                egui::Button::new(if self.capture {
                                    "■ 停止接收文件"
                                } else {
                                    "接收数据到文件"
                                }),
                            )
                            .clicked()
                        {
                            self.toggle_capture();
                        }
                        if self.capture {
                            ui.label(RichText::new("● 录制中").color(RED))
                                .on_hover_text(
                                    self.capture_path
                                        .as_ref()
                                        .map(|p| p.display().to_string())
                                        .unwrap_or_default(),
                                );
                        }
                    });
                });
                ui.add_space(10.0);
                if self.last_stats.2 > 0 {
                    ui.label(
                        RichText::new(format!(
                            "显示负载较高，已省略 {} 个显示块；字节计数与原始文件独立记录。",
                            self.last_stats.2
                        ))
                        .color(RED)
                        .size(11.0),
                    );
                }
                let logs = self.frozen.as_ref().unwrap_or(&self.logs);
                let filter = self.filter.to_lowercase();
                let visible: Vec<_> = logs
                    .iter()
                    .filter(|row| {
                        (self.settings.show_tx || row.direction != Some(Direction::Tx))
                            && (filter.is_empty()
                                || row.text.to_lowercase().contains(&filter)
                                || row.hex.to_lowercase().contains(&filter))
                    })
                    .collect();
                egui::Frame::new()
                    .fill(TERMINAL)
                    .corner_radius(8)
                    .inner_margin(12.0)
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        let h = (ui.available_height() - 24.0).max(70.0);
                        egui::ScrollArea::both()
                            .id_salt("log")
                            .stick_to_bottom(self.follow && self.frozen.is_none())
                            .max_height(h)
                            .auto_shrink([false, false])
                            .show_rows(ui, 22.0, visible.len(), |ui, range| {
                                for i in range {
                                    let row = visible[i];
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 10.0;
                                        if self.settings.timestamps {
                                            ui.label(
                                                RichText::new(&row.at)
                                                    .monospace()
                                                    .size(11.0)
                                                    .color(Color32::from_rgb(115, 134, 158)),
                                            );
                                        }
                                        let (tag, color) = match row.direction {
                                            Some(Direction::Rx) => {
                                                ("RX", Color32::from_rgb(67, 214, 173))
                                            }
                                            Some(Direction::Tx) => {
                                                ("TX", Color32::from_rgb(113, 170, 255))
                                            }
                                            None => ("•", Color32::from_rgb(146, 162, 185)),
                                        };
                                        ui.label(
                                            RichText::new(tag).monospace().size(11.0).color(color),
                                        );
                                        let full =
                                            if self.settings.rx_hex && row.direction.is_some() {
                                                &row.hex
                                            } else {
                                                &row.text
                                            };
                                        let mut preview: String = full
                                            .chars()
                                            .take(260)
                                            .map(|c| match c {
                                                '\n' => '↵',
                                                '\r' => '␍',
                                                '\t' => '⇥',
                                                c if c.is_control() => '·',
                                                c => c,
                                            })
                                            .collect();
                                        if full.chars().count() > 260 {
                                            preview.push_str(" …");
                                        }
                                        if preview.is_empty() && !row.bytes.is_empty() {
                                            preview = "（等待后续字节完成字符）".into();
                                        }
                                        let response = ui.add(
                                            egui::Label::new(
                                                RichText::new(preview)
                                                    .monospace()
                                                    .size(13.0)
                                                    .color(if row.direction.is_some() {
                                                        Color32::from_rgb(220, 230, 242)
                                                    } else {
                                                        Color32::from_rgb(146, 162, 185)
                                                    }),
                                            )
                                            .sense(egui::Sense::click())
                                            .wrap_mode(egui::TextWrapMode::Extend),
                                        );
                                        if response
                                            .on_hover_text("点击查看完整数据、复制文本或 HEX")
                                            .clicked()
                                        {
                                            self.detail = Some(row.clone());
                                        }
                                        if row.direction.is_some() {
                                            ui.label(
                                                RichText::new(format!("{} B", row.bytes.len()))
                                                    .monospace()
                                                    .size(10.0)
                                                    .color(Color32::from_rgb(94, 113, 137)),
                                            );
                                        }
                                    });
                                }
                            });
                    });
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "{} 条记录 · 缓存 {} · 点击记录查看完整内容",
                            logs.len(),
                            format_bytes(self.log_bytes as u64)
                        ))
                        .size(10.0)
                        .color(MUTED),
                    );
                    if self.frozen.is_some() {
                        ui.label(
                            RichText::new("显示已暂停，后台仍在接收")
                                .size(10.0)
                                .color(ACCENT),
                        );
                    }
                });
            });
    }
    fn clear_logs(&mut self) {
        self.logs.clear();
        self.log_bytes = 0;
        if self.frozen.is_some() {
            self.frozen = Some(VecDeque::new());
        }
        self.rx_decoder = StreamDecoder::new(self.settings.send.encoding);
        self.tx_decoder = StreamDecoder::new(self.settings.send.encoding);
    }
    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(index) = self
            .comment_preset
            .filter(|i| *i < self.settings.presets.len())
        {
            let mut open = true;
            egui::Window::new("编辑指令注释")
                .open(&mut open)
                .collapsible(false)
                .default_width(360.0)
                .show(ctx, |ui| {
                    ui.label(format!("第 {} 条指令", index + 1));
                    ui.add(
                        TextEdit::singleline(&mut self.settings.presets[index].name)
                            .desired_width(f32::INFINITY),
                    );
                    if ui.button("完成").clicked() {
                        self.comment_preset = None;
                    }
                });
            if !open {
                self.comment_preset = None;
            }
        }
        if let Some(row) = &self.detail {
            let mut open = true;
            egui::Window::new("数据详情")
                .open(&mut open)
                .default_size([700.0, 430.0])
                .show(ctx, |ui| {
                    ui.label(format!("{} · {} 字节", row.at, row.bytes.len()));
                    ui.horizontal(|ui| {
                        if ui.button("复制文本").clicked() {
                            ctx.copy_text(row.text.clone());
                        }
                        if ui.button("复制 HEX").clicked() {
                            ctx.copy_text(row.hex.clone());
                        }
                    });
                    egui::ScrollArea::vertical()
                        .max_height(350.0)
                        .show(ui, |ui| {
                            ui.label(RichText::new("文本").strong());
                            ui.add(
                                TextEdit::multiline(&mut row.text.as_str())
                                    .desired_width(f32::INFINITY)
                                    .font(egui::TextStyle::Monospace),
                            );
                            ui.add_space(12.0);
                            ui.label(RichText::new("HEX").strong());
                            ui.add(
                                TextEdit::multiline(&mut row.hex.as_str())
                                    .desired_width(f32::INFINITY)
                                    .font(egui::TextStyle::Monospace),
                            );
                        });
                });
            if !open {
                self.detail = None;
            }
        }
        egui::Window::new("使用说明").open(&mut self.help).default_width(610.0).show(ctx,|ui|{
            ui.heading("从连接到收发，只需几步");ui.add_space(10.0);
            for (title,body) in [
                ("01  选择连接","串口：刷新端口，设置波特率、数据位、停止位和校验位，再打开连接。没有设备时可选择“回环测试”。网络模式支持 TCP 客户端、单客户端 TCP 服务端，以及固定远端 UDP。"),
                ("02  编辑与发送","文本支持 UTF-8、GBK、ASCII；HEX 支持 01 0A FF、010AFF 和 0x01。勾选转义符后支持 \\r、\\n、\\t、\\0、\\xFF、\\\\。Ctrl+Enter 发送单条内容。"),
                ("03  校验与批量","校验先计算排除头尾后的内容，插入到帧尾前，然后追加换行。Modbus CRC16、CRC32 按低字节在前；CRC16/XMODEM 按高字节在前。多条指令共用换行、编码和校验，HEX 单独设置。延时是本条发送完成到下一条发送的时间。"),
                ("04  保存与采集","导出日志只保存当前缓存。长时间采集请使用“接收数据到文件”，原始 RX 字节独立保存，不含时间戳、TX 或额外换行。已有文件不会覆盖。关闭连接会刷新文件。文件发送保持原始字节，不附加校验。"),
                ("05  显示与性能","串口/TCP 按接收空闲间隔合并显示，默认 20 ms，可在监视器中调整；这不代表协议帧边界。持续数据最多每 200 ms 或 65536 B 显示一块，间隔大于 200 ms 时随之延长；UDP 保留数据报边界。长记录点击可查看全文，UTF-8/GBK 支持跨块解码。显示缓存最多约 4194304 B / 3000 条，较早记录自动移除。统计始终以 B 显示，速率为 B/s。暂停显示期间后台仍继续接收。高流量会省略显示块，原始采集和计数独立。"),
            ]{ui.label(RichText::new(title).strong().color(ACCENT));ui.label(body);ui.add_space(12.0);}
            ui.label(RichText::new(format!("配置位置：{}",self.path.display())).size(11.0).color(MUTED));
        });
    }
}
impl eframe::App for SerialApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Enter))
            && self.ready
            && !self.sending
        {
            self.send_single(false);
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::L)) {
            self.clear_logs();
        }
        if ctx.input(|i| i.viewport().close_requested()) && self.session.is_some() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if !self.disconnecting {
                self.disconnect();
            }
            // The close request is retried after the worker flushes and reports Closed.
            ctx.data_mut(|d| d.insert_temp(egui::Id::new("close_after_flush"), true));
        }
        if self.session.is_none()
            && ctx.data(|d| {
                d.get_temp::<bool>(egui::Id::new("close_after_flush"))
                    .unwrap_or(false)
            })
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        self.header(ctx);
        self.footer(ctx);
        self.workspace(ctx);
        self.dialogs(ctx);
        if self.last_save.elapsed() > Duration::from_secs(30) {
            self.save_settings(false);
            self.last_save = Instant::now();
        }
        ctx.request_repaint_after(Duration::from_millis(if self.session.is_some() {
            30
        } else {
            250
        }));
    }
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_settings(false);
    }
}

fn configure_ui(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for path in [
        "C:/Windows/Fonts/msyh.ttc",
        "C:/Windows/Fonts/simhei.ttf",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/System/Library/Fonts/PingFang.ttc",
    ] {
        if let Ok(data) = std::fs::read(path) {
            fonts
                .font_data
                .insert("cjk".into(), egui::FontData::from_owned(data).into());
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(family).or_default().push("cjk".into());
            }
            break;
        }
    }
    ctx.set_fonts(fonts);
    let mut style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::light();
    style.visuals.override_text_color = Some(INK);
    style.visuals.selection.bg_fill = Color32::from_rgb(219, 235, 255);
    style.visuals.selection.stroke = egui::Stroke::new(1.0_f32, ACCENT);
    style.visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(241, 245, 250);
    style.visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, BORDER);
    style.visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(226, 237, 252);
    style.visuals.widgets.active.weak_bg_fill = Color32::from_rgb(211, 230, 253);
    style.visuals.extreme_bg_color = Color32::from_rgb(249, 251, 254);
    style.spacing.item_spacing = egui::vec2(8.0, 7.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.interact_size.y = 28.0;
    style
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(13.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(13.0));
    style
        .text_styles
        .insert(egui::TextStyle::Monospace, egui::FontId::monospace(13.0));
    ctx.set_style(style);
}
fn field_label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(12.0).color(MUTED));
}
fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(11.0).color(MUTED));
}
fn str_combo(ui: &mut egui::Ui, id: &str, value: &mut String, values: &[&str]) {
    egui::ComboBox::from_id_salt(id)
        .width(110.0)
        .selected_text(value.as_str())
        .show_ui(ui, |ui| {
            for v in values {
                ui.selectable_value(value, (*v).to_owned(), *v);
            }
        });
}
fn primary_button(ui: &mut egui::Ui, text: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(
        enabled,
        egui::Button::new(RichText::new(text).color(Color32::WHITE))
            .fill(ACCENT)
            .min_size(egui::vec2(100.0, 31.0)),
    )
}
fn metric(ui: &mut egui::Ui, title: &str, bytes: u64, color: Color32) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).size(12.0).color(MUTED));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(format_bytes(bytes))
                    .size(18.0)
                    .strong()
                    .color(color),
            )
            .on_hover_text(format!("{bytes} 字节"));
        });
    });
}
fn format_bytes(n: u64) -> String {
    format!("{n} B")
}

#[cfg(test)]
mod tests {
    use super::format_bytes;
    #[test]
    fn statistics_always_use_exact_bytes() {
        for n in [0, 1023, 1024, 1_048_576, u64::MAX] {
            assert_eq!(format_bytes(n), format!("{n} B"));
        }
    }
}
