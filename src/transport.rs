use crate::config::{ConnectionConfig, Mode};
use crate::receive::{ReceiveBuffer, ReceivedBlock};
use std::{
    fs::File,
    io::{self, BufWriter, Read, Write},
    net::{TcpListener, TcpStream, ToSocketAddrs, UdpSocket},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Rx,
    Tx,
}
#[derive(Debug)]
pub enum Event {
    Ready(String),
    Waiting(String),
    Closed,
    Data {
        direction: Direction,
        bytes: Vec<u8>,
        at: SystemTime,
    },
    Error(String),
    Info(String),
    Progress {
        sent: u64,
        total: u64,
    },
    SendingStopped,
    ScheduleProgress {
        completed: u64,
        total: Option<u64>,
    },
    Capture(bool),
}
#[derive(Clone)]
pub struct Step {
    pub count: u64,
    pub bytes: Vec<u8>,
    pub delay: Duration,
}
pub enum Command {
    Send(Vec<u8>),
    Schedule {
        steps: Vec<Step>,
        cycles: Option<u64>,
    },
    StopSending,
    SendFile {
        path: PathBuf,
        chunk: usize,
        delay: Duration,
    },
    SetLines {
        dtr: bool,
        rts: bool,
    },
    StartCapture(PathBuf),
    StopCapture,
    SetReceiveGap(u64),
}
#[derive(Default)]
pub struct Stats {
    pub rx: AtomicU64,
    pub tx: AtomicU64,
    pub omitted: AtomicU64,
}
impl Stats {
    pub fn snapshot(&self) -> (u64, u64, u64) {
        (
            self.rx.load(Ordering::Relaxed),
            self.tx.load(Ordering::Relaxed),
            self.omitted.load(Ordering::Relaxed),
        )
    }
    pub fn reset(&self) {
        self.rx.store(0, Ordering::Relaxed);
        self.tx.store(0, Ordering::Relaxed);
        self.omitted.store(0, Ordering::Relaxed);
    }
}
pub struct Session {
    pub events: Receiver<Event>,
    pub stats: Arc<Stats>,
    commands: SyncSender<Command>,
    stop: Arc<AtomicBool>,
    cancel_send: Arc<AtomicBool>,
}
impl Session {
    pub fn start(config: ConnectionConfig) -> Self {
        let (tx, events) = mpsc::sync_channel(512);
        let (commands, rx) = mpsc::sync_channel(64);
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Stats::default());
        let worker_stop = stop.clone();
        let worker_stats = stats.clone();
        let cancel_send = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel_send.clone();
        thread::spawn(move || {
            if let Err(e) = run(config, rx, &tx, &worker_stop, &worker_cancel, &worker_stats) {
                let _ = tx.send(Event::Error(e));
            }
            let _ = tx.send(Event::Closed);
        });
        Self {
            events,
            stats,
            commands,
            stop,
            cancel_send,
        }
    }
    pub fn send(&self, command: Command) -> Result<(), String> {
        if matches!(command, Command::StopSending) {
            self.cancel_send.store(true, Ordering::Relaxed);
        }
        self.commands.try_send(command).map_err(|e| match e {
            mpsc::TrySendError::Full(_) => "发送队列已满，请稍后再试".into(),
            mpsc::TrySendError::Disconnected(_) => "连接已经关闭".into(),
        })
    }
    pub fn disconnect(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.disconnect();
    }
}

enum Transport {
    Serial(Box<dyn serialport::SerialPort>),
    Tcp(TcpStream),
    Listener(TcpListener),
    Udp(UdpSocket),
    Loopback(Vec<u8>),
}
fn transient(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}
fn open(c: &ConnectionConfig, events: &SyncSender<Event>) -> Result<Transport, String> {
    let result = match c.mode {
        Mode::Serial => {
            if c.port.trim().is_empty() || c.baud == 0 {
                return Err("请选择串口并输入有效波特率".into());
            }
            let bits = match c.data_bits {
                5 => serialport::DataBits::Five,
                6 => serialport::DataBits::Six,
                7 => serialport::DataBits::Seven,
                8 => serialport::DataBits::Eight,
                _ => return Err("数据位必须为 5～8".into()),
            };
            let parity = match c.parity.as_str() {
                "None" => serialport::Parity::None,
                "Odd" => serialport::Parity::Odd,
                "Even" => serialport::Parity::Even,
                _ => return Err("无效的串口校验位".into()),
            };
            let flow = match c.flow.as_str() {
                "None" => serialport::FlowControl::None,
                "Hardware" => serialport::FlowControl::Hardware,
                "Software" => serialport::FlowControl::Software,
                _ => return Err("无效的流控模式".into()),
            };
            let stop = match c.stop_bits {
                1 => serialport::StopBits::One,
                2 => serialport::StopBits::Two,
                _ => return Err("停止位必须为 1 或 2".into()),
            };
            let mut port = serialport::new(c.port.trim(), c.baud)
                .data_bits(bits)
                .parity(parity)
                .stop_bits(stop)
                .flow_control(flow)
                .timeout(Duration::from_millis(10))
                .dtr_on_open(c.dtr)
                .open()
                .map_err(|e| format!("打开 {} 失败：{e}", c.port))?;
            if flow != serialport::FlowControl::Hardware {
                port.write_request_to_send(c.rts)
                    .map_err(|e| format!("设置 RTS 失败：{e}"))?;
            }
            let _ = events.send(Event::Ready(format!("{} · {} bps", c.port, c.baud)));
            Transport::Serial(port)
        }
        Mode::TcpClient => {
            let addrs: Vec<_> = c
                .remote
                .to_socket_addrs()
                .map_err(|e| format!("解析远端地址失败：{e}"))?
                .collect();
            let mut last = "地址没有解析结果".to_owned();
            let mut stream = None;
            for addr in addrs {
                match TcpStream::connect_timeout(&addr, Duration::from_secs(3)) {
                    Ok(s) => {
                        stream = Some(s);
                        break;
                    }
                    Err(e) => last = e.to_string(),
                }
            }
            let s = stream.ok_or_else(|| format!("TCP 连接失败：{last}"))?;
            s.set_nonblocking(true).map_err(|e| e.to_string())?;
            let _ = s.set_nodelay(true);
            let _ = events.send(Event::Ready(format!("TCP → {}", c.remote)));
            Transport::Tcp(s)
        }
        Mode::TcpServer => {
            let listener = TcpListener::bind(&c.local).map_err(|e| format!("监听失败：{e}"))?;
            listener.set_nonblocking(true).map_err(|e| e.to_string())?;
            let _ = events.send(Event::Waiting(format!(
                "监听 {}，等待一个客户端",
                listener.local_addr().map_err(|e| e.to_string())?
            )));
            Transport::Listener(listener)
        }
        Mode::Udp => {
            let socket = UdpSocket::bind(&c.local).map_err(|e| format!("UDP 绑定失败：{e}"))?;
            socket
                .connect(&c.remote)
                .map_err(|e| format!("UDP 远端设置失败：{e}"))?;
            socket.set_nonblocking(true).map_err(|e| e.to_string())?;
            let _ = events.send(Event::Ready(format!(
                "UDP {} → {}",
                socket.local_addr().map_err(|e| e.to_string())?,
                c.remote
            )));
            Transport::Udp(socket)
        }
        Mode::Loopback => {
            let _ = events.send(Event::Ready("本地回环 · 发出的字节将原样返回".into()));
            Transport::Loopback(Vec::new())
        }
    };
    Ok(result)
}
impl Transport {
    fn read(&mut self, buf: &mut [u8], events: &SyncSender<Event>) -> io::Result<usize> {
        match self {
            Self::Serial(p) => p.read(buf),
            Self::Tcp(s) => s.read(buf).and_then(|n| {
                if n == 0 {
                    Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "TCP 对端已断开",
                    ))
                } else {
                    Ok(n)
                }
            }),
            Self::Udp(s) => s.recv(buf),
            Self::Loopback(data) => {
                let n = data.len().min(buf.len());
                buf[..n].copy_from_slice(&data[..n]);
                data.drain(..n);
                Ok(n)
            }
            Self::Listener(l) => {
                let (stream, addr) = l.accept()?;
                stream.set_nonblocking(true)?;
                let _ = stream.set_nodelay(true);
                *self = Self::Tcp(stream);
                let _ = events.send(Event::Ready(format!("TCP 客户端 {addr}")));
                Ok(0)
            }
        }
    }
    fn write_all(
        &mut self,
        bytes: &[u8],
        stop: &AtomicBool,
        cancel: &AtomicBool,
        stats: &Stats,
    ) -> Result<usize, String> {
        if stop.load(Ordering::Relaxed) || cancel.load(Ordering::Relaxed) {
            return Ok(0);
        }
        if let Self::Listener(_) = self {
            return Err("尚无 TCP 客户端连接".into());
        }
        if let Self::Udp(s) = self {
            if bytes.len() > 65_507 {
                return Err("UDP 单个数据报不能超过 65507 字节".into());
            }
            let n = s.send(bytes).map_err(|e| e.to_string())?;
            stats.tx.fetch_add(n as u64, Ordering::Relaxed);
            return if n == bytes.len() {
                Ok(n)
            } else {
                Err("UDP 数据报未完整发送".into())
            };
        }
        let allowed = match self {
            Self::Serial(p) => Duration::from_secs_f64(
                3.0 + bytes.len() as f64 * 12.0 / p.baud_rate().unwrap_or(9600).max(1) as f64,
            ),
            _ => Duration::from_secs(3),
        };
        let started = Instant::now();
        let mut offset = 0;
        while offset < bytes.len() {
            if stop.load(Ordering::Relaxed) || cancel.load(Ordering::Relaxed) {
                return Ok(offset);
            }
            let written = match self {
                Self::Serial(p) => p.write(&bytes[offset..]),
                Self::Tcp(s) => s.write(&bytes[offset..]),
                Self::Loopback(data) => {
                    data.extend_from_slice(&bytes[offset..]);
                    Ok(bytes.len() - offset)
                }
                _ => unreachable!(),
            };
            match written {
                Ok(0) => return Err("连接无法继续写入".into()),
                Ok(n) => {
                    offset += n;
                    stats.tx.fetch_add(n as u64, Ordering::Relaxed);
                }
                Err(e) if transient(&e) && started.elapsed() < allowed => {
                    thread::sleep(Duration::from_millis(2))
                }
                Err(e) => {
                    return Err(format!(
                        "发送失败（已写入 {offset}/{} 字节）：{e}",
                        bytes.len()
                    ));
                }
            }
        }
        Ok(offset)
    }
}
struct Schedule {
    steps: Vec<Step>,
    index: usize,
    step_sent: u64,
    remaining_cycles: Option<u64>,
    completed_cycles: u64,
    total_cycles: Option<u64>,
    due: Instant,
}
struct FileJob {
    file: File,
    chunk: usize,
    delay: Duration,
    due: Instant,
    sent: u64,
    total: u64,
}
fn emit_data(events: &SyncSender<Event>, stats: &Stats, direction: Direction, bytes: Vec<u8>) {
    emit_data_at(events, stats, direction, bytes, SystemTime::now());
}
fn emit_data_at(
    events: &SyncSender<Event>,
    stats: &Stats,
    direction: Direction,
    bytes: Vec<u8>,
    at: SystemTime,
) {
    if events
        .try_send(Event::Data {
            direction,
            bytes,
            at,
        })
        .is_err()
    {
        stats.omitted.fetch_add(1, Ordering::Relaxed);
    }
}
fn emit_received(events: &SyncSender<Event>, stats: &Stats, block: ReceivedBlock) {
    emit_data_at(events, stats, Direction::Rx, block.bytes, block.at);
}
fn run(
    c: ConnectionConfig,
    commands: Receiver<Command>,
    events: &SyncSender<Event>,
    stop: &AtomicBool,
    cancel: &AtomicBool,
    stats: &Stats,
) -> Result<(), String> {
    let mut transport = open(&c, events)?;
    let mut capture: Option<BufWriter<File>> = None;
    let mut schedule: Option<Schedule> = None;
    let mut file: Option<FileJob> = None;
    // A full-size UDP buffer avoids silently truncating datagrams.
    let mut buf = vec![0u8; 65_536];
    let mut flushed = Instant::now();
    let coalesce = matches!(c.mode, Mode::Serial | Mode::TcpClient | Mode::TcpServer);
    let mut receive = ReceiveBuffer::new(c.receive_gap_ms);
    let result = (|| -> Result<(), String> {
        while !stop.load(Ordering::Relaxed) {
            for _ in 0..64 {
                let command = match commands.try_recv() {
                    Ok(c) => c,
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
                };
                match command {
                    Command::SetReceiveGap(gap_ms) => {
                        if let Some(block) = receive.flush() {
                            emit_received(events, stats, block);
                        }
                        receive = ReceiveBuffer::new(gap_ms);
                    }
                    Command::Send(bytes) => {
                        if schedule.is_some() || file.is_some() {
                            let _ = events.send(Event::Error("请先停止当前发送任务".into()));
                            continue;
                        }
                        cancel.store(false, Ordering::Relaxed);
                        let n = transport.write_all(&bytes, stop, cancel, stats)?;
                        if n > 0 {
                            emit_data(events, stats, Direction::Tx, bytes[..n].to_vec());
                        }
                    }
                    Command::Schedule { steps, cycles } => {
                        cancel.store(false, Ordering::Relaxed);
                        if steps.is_empty()
                            || cycles == Some(0)
                            || steps.iter().any(|step| step.count == 0)
                        {
                            let _ =
                                events.send(Event::Error("没有可发送的指令或发送次数无效".into()));
                            let _ = events.send(Event::SendingStopped);
                            continue;
                        }
                        file = None;
                        schedule = Some(Schedule {
                            steps,
                            remaining_cycles: cycles,
                            completed_cycles: 0,
                            total_cycles: cycles,
                            index: 0,
                            step_sent: 0,
                            due: Instant::now(),
                        });
                        let _ = events.send(Event::ScheduleProgress {
                            completed: 0,
                            total: cycles,
                        });
                    }
                    Command::StopSending => {
                        schedule = None;
                        file = None;
                        let _ = events.send(Event::SendingStopped);
                    }
                    Command::SendFile { path, chunk, delay } => {
                        cancel.store(false, Ordering::Relaxed);
                        schedule = None;
                        file = None;
                        match File::open(&path).and_then(|f| Ok((f.metadata()?.len(), f))) {
                            Ok((total, f)) => {
                                file = Some(FileJob {
                                    file: f,
                                    chunk: chunk.clamp(1, 16_384),
                                    delay,
                                    due: Instant::now(),
                                    sent: 0,
                                    total,
                                });
                                let _ = events.send(Event::Progress { sent: 0, total });
                            }
                            Err(e) => {
                                let _ =
                                    events.send(Event::Error(format!("打开待发送文件失败：{e}")));
                                let _ = events.send(Event::SendingStopped);
                            }
                        }
                    }
                    Command::SetLines { dtr, rts } => {
                        if let Transport::Serial(p) = &mut transport {
                            let result = p.write_data_terminal_ready(dtr).and_then(|_| {
                                if c.flow != "Hardware" {
                                    p.write_request_to_send(rts)
                                } else {
                                    Ok(())
                                }
                            });
                            if let Err(e) = result {
                                let _ = events.send(Event::Error(format!("设置控制线失败：{e}")));
                            }
                        }
                    }
                    Command::StartCapture(path) => {
                        if capture.is_some() {
                            let _ = events.send(Event::Error("已有接收文件正在保存".into()));
                            continue;
                        }
                        // create_new protects an existing capture from accidental truncation.
                        match File::options().write(true).create_new(true).open(path) {
                            Ok(f) => {
                                capture = Some(BufWriter::new(f));
                                let _ = events.send(Event::Capture(true));
                            }
                            Err(e) => {
                                let _ = events.send(Event::Error(format!("创建接收文件失败：{e}")));
                                let _ = events.send(Event::Capture(false));
                            }
                        }
                    }
                    Command::StopCapture => {
                        if let Some(mut f) = capture.take()
                            && let Err(e) = f.flush()
                        {
                            let _ = events.send(Event::Error(format!("刷新接收文件失败：{e}")));
                        }
                        let _ = events.send(Event::Capture(false));
                    }
                }
            }
            if let Some(job) = &mut schedule
                && Instant::now() >= job.due
            {
                let step = &job.steps[job.index];
                let n = transport.write_all(&step.bytes, stop, cancel, stats)?;
                if n > 0 {
                    emit_data(events, stats, Direction::Tx, step.bytes[..n].to_vec());
                }
                job.due = Instant::now() + step.delay.max(Duration::from_millis(10));
                if n == step.bytes.len() {
                    job.step_sent += 1;
                    if job.step_sent >= step.count {
                        job.index += 1;
                        job.step_sent = 0;
                    }
                }
                if job.index == job.steps.len() {
                    job.completed_cycles += 1;
                    let _ = events.send(Event::ScheduleProgress {
                        completed: job.completed_cycles,
                        total: job.total_cycles,
                    });
                    if job.remaining_cycles == Some(1) {
                        schedule = None;
                        let _ = events.send(Event::SendingStopped);
                    } else {
                        if let Some(remaining) = &mut job.remaining_cycles {
                            *remaining -= 1;
                        }
                        job.index = 0;
                    }
                }
            }
            if let Some(job) = &mut file
                && Instant::now() >= job.due
            {
                let mut bytes = vec![0; job.chunk];
                match job.file.read(&mut bytes) {
                    Ok(0) => {
                        let _ = events.send(Event::Progress {
                            sent: job.sent,
                            total: job.total,
                        });
                        file = None;
                        let _ = events.send(Event::Info("文件发送完成".into()));
                        let _ = events.send(Event::SendingStopped);
                    }
                    Ok(n) => {
                        bytes.truncate(n);
                        let sent = transport.write_all(&bytes, stop, cancel, stats)?;
                        job.sent += sent as u64;
                        job.due = Instant::now() + job.delay;
                        if sent > 0 {
                            emit_data(events, stats, Direction::Tx, bytes[..sent].to_vec());
                        }
                        let _ = events.try_send(Event::Progress {
                            sent: job.sent,
                            total: job.total,
                        });
                    }
                    Err(e) => {
                        file = None;
                        let _ = events.send(Event::Error(format!("读取发送文件失败：{e}")));
                        let _ = events.send(Event::SendingStopped);
                    }
                }
            }
            if cancel.load(Ordering::Relaxed) && (schedule.is_some() || file.is_some()) {
                schedule = None;
                file = None;
                let _ = events.send(Event::SendingStopped);
            }
            match transport.read(&mut buf, events) {
                Ok(n) if n > 0 => {
                    stats.rx.fetch_add(n as u64, Ordering::Relaxed);
                    if let Some(f) = &mut capture
                        && let Err(e) = f.write_all(&buf[..n])
                    {
                        capture = None;
                        let _ = events.send(Event::Capture(false));
                        let _ = events.send(Event::Error(format!("接收文件写入失败：{e}")));
                    }
                    if coalesce {
                        for block in receive.push(&buf[..n], SystemTime::now(), Instant::now()) {
                            emit_received(events, stats, block);
                        }
                    } else {
                        // Preserve UDP datagram and local loopback message boundaries.
                        emit_data(events, stats, Direction::Rx, buf[..n].to_vec());
                    }
                }
                Ok(_) => (),
                Err(e) if transient(&e) => (),
                Err(e) => return Err(format!("连接已中断：{e}")),
            }
            if let Some(block) = receive.flush_if_due(Instant::now()) {
                emit_received(events, stats, block);
            }
            if flushed.elapsed() >= Duration::from_secs(1) {
                if let Some(f) = &mut capture
                    && let Err(e) = f.flush()
                {
                    capture = None;
                    let _ = events.send(Event::Capture(false));
                    let _ = events.send(Event::Error(format!("接收文件刷新失败：{e}")));
                }
                flushed = Instant::now();
            }
            thread::sleep(Duration::from_millis(2));
        }
        Ok(())
    })();
    if let Some(block) = receive.flush() {
        emit_received(events, stats, block);
    }
    // Explicitly flush on every exit, including unplugged serial ports and TCP EOF.
    let flushed = if let Some(mut f) = capture {
        f.flush().map_err(|e| format!("保存接收文件失败：{e}"))
    } else {
        Ok(())
    };
    match (result, flushed) {
        (Err(a), Err(b)) => Err(format!("{a}；{b}")),
        (Err(e), _) | (_, Err(e)) => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn wait(session: &Session, condition: impl Fn(&Event) -> bool) -> Event {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let event = session
                .events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("worker event timed out");
            if let Event::Error(e) = &event {
                panic!("{e}");
            }
            if condition(&event) {
                return event;
            }
        }
    }
    fn loopback() -> Session {
        let s = Session::start(ConnectionConfig {
            mode: Mode::Loopback,
            ..Default::default()
        });
        wait(&s, |e| matches!(e, Event::Ready(_)));
        s
    }
    fn temp_file(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "serial-assistant-{label}-{}-{}.bin",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
    #[test]
    fn binary_loopback_and_stats() {
        let s = loopback();
        s.send(Command::Send(vec![0, 255, 13, 10, 128])).unwrap();
        let event = wait(&s, |e| {
            matches!(
                e,
                Event::Data {
                    direction: Direction::Rx,
                    ..
                }
            )
        });
        if let Event::Data { bytes, .. } = event {
            assert_eq!(bytes, [0, 255, 13, 10, 128]);
        }
        assert_eq!(s.stats.snapshot(), (5, 5, 0));
    }
    #[test]
    fn scheduler_stops_and_orders_commands() {
        let s = loopback();
        s.send(Command::Schedule {
            steps: vec![
                Step {
                    count: 1,
                    bytes: vec![1],
                    delay: Duration::from_millis(15),
                },
                Step {
                    count: 1,
                    bytes: vec![2],
                    delay: Duration::from_millis(15),
                },
            ],
            cycles: Some(1),
        })
        .unwrap();
        let mut sent = Vec::new();
        loop {
            match wait(&s, |_| true) {
                Event::Data {
                    direction: Direction::Tx,
                    bytes,
                    ..
                } => sent.extend(bytes),
                Event::SendingStopped => break,
                _ => (),
            }
        }
        assert_eq!(sent, [1, 2]);
        s.send(Command::Schedule {
            steps: vec![Step {
                count: 1,
                bytes: vec![3],
                delay: Duration::from_millis(20),
            }],
            cycles: None,
        })
        .unwrap();
        wait(&s, |e| {
            matches!(
                e,
                Event::Data {
                    direction: Direction::Tx,
                    ..
                }
            )
        });
        s.send(Command::StopSending).unwrap();
        wait(&s, |e| matches!(e, Event::SendingStopped));
        let deadline = Instant::now() + Duration::from_millis(80);
        while let Ok(event) = s
            .events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            // Already-written bytes may still arrive as RX after stopping.
            assert!(!matches!(
                event,
                Event::Data {
                    direction: Direction::Tx,
                    ..
                }
            ));
        }
    }
    #[test]
    fn file_transfer_capture_is_byte_exact() {
        let s = loopback();
        let source = temp_file("source");
        let target = temp_file("capture");
        let data: Vec<_> = (0..20000).map(|i| (i % 256) as u8).collect();
        std::fs::write(&source, &data).unwrap();
        s.send(Command::StartCapture(target.clone())).unwrap();
        wait(&s, |e| matches!(e, Event::Capture(true)));
        s.send(Command::SendFile {
            path: source.clone(),
            chunk: 1024,
            delay: Duration::ZERO,
        })
        .unwrap();
        wait(&s, |e| matches!(e, Event::SendingStopped));
        s.send(Command::StopCapture).unwrap();
        wait(&s, |e| matches!(e, Event::Capture(false)));
        assert_eq!(std::fs::read(&target).unwrap(), data);
        assert_eq!(s.stats.snapshot().0, 20000);
        std::fs::remove_file(source).unwrap();
        std::fs::remove_file(target).unwrap();
    }
    #[test]
    fn tcp_client_roundtrip_and_disconnect() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let peer = thread::spawn(move || {
            let (mut p, _) = listener.accept().unwrap();
            p.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut bytes = [0; 4];
            p.read_exact(&mut bytes).unwrap();
            p.write_all(&bytes).unwrap();
        });
        let s = Session::start(ConnectionConfig {
            mode: Mode::TcpClient,
            remote: addr.to_string(),
            ..Default::default()
        });
        wait(&s, |e| matches!(e, Event::Ready(_)));
        s.send(Command::Send(vec![1, 2, 3, 4])).unwrap();
        wait(
            &s,
            |e| matches!(e,Event::Data {direction:Direction::Rx,bytes,..} if bytes==&[1,2,3,4]),
        );
        peer.join().unwrap();
        loop {
            if matches!(
                s.events.recv_timeout(Duration::from_secs(3)).unwrap(),
                Event::Closed
            ) {
                break;
            }
        }
    }
    #[test]
    fn tcp_server_accepts_peer() {
        let s = Session::start(ConnectionConfig {
            mode: Mode::TcpServer,
            local: "127.0.0.1:0".into(),
            ..Default::default()
        });
        let Event::Waiting(message) = wait(&s, |e| matches!(e, Event::Waiting(_))) else {
            panic!()
        };
        let addr = message
            .strip_prefix("监听 ")
            .unwrap()
            .split('，')
            .next()
            .unwrap();
        let mut peer = TcpStream::connect(addr).unwrap();
        wait(&s, |e| matches!(e, Event::Ready(_)));
        peer.write_all(b"test").unwrap();
        wait(
            &s,
            |e| matches!(e,Event::Data {direction:Direction::Rx,bytes,..} if bytes==b"test"),
        );
    }
    #[test]
    fn udp_roundtrip_preserves_large_datagrams() {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let remote = socket.local_addr().unwrap();
        let peer = thread::spawn(move || {
            let mut b = vec![0; 65536];
            let (n, addr) = socket.recv_from(&mut b).unwrap();
            socket.send_to(&b[..n], addr).unwrap();
        });
        let s = Session::start(ConnectionConfig {
            mode: Mode::Udp,
            local: "127.0.0.1:0".into(),
            remote: remote.to_string(),
            ..Default::default()
        });
        wait(&s, |e| matches!(e, Event::Ready(_)));
        s.send(Command::Send(vec![42; 12000])).unwrap();
        wait(
            &s,
            |e| matches!(e,Event::Data {direction:Direction::Rx,bytes,..} if bytes.len()==12000),
        );
        peer.join().unwrap();
    }
    #[test]
    fn invalid_port_reports_failure_and_closes() {
        let s = Session::start(ConnectionConfig {
            port: "COM_INVALID_PORT".into(),
            ..Default::default()
        });
        assert!(matches!(
            s.events.recv_timeout(Duration::from_secs(4)).unwrap(),
            Event::Error(_)
        ));
        assert!(matches!(
            s.events.recv_timeout(Duration::from_secs(1)).unwrap(),
            Event::Closed
        ));
    }
    #[test]
    fn capture_does_not_overwrite_an_existing_file() {
        let s = loopback();
        let path = temp_file("existing");
        std::fs::write(&path, b"keep").unwrap();
        s.send(Command::StartCapture(path.clone())).unwrap();
        assert!(matches!(
            s.events.recv_timeout(Duration::from_secs(1)).unwrap(),
            Event::Error(_)
        ));
        assert_eq!(std::fs::read(&path).unwrap(), b"keep");
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn disconnect_flushes_capture() {
        let s = loopback();
        let path = temp_file("disconnect");
        s.send(Command::StartCapture(path.clone())).unwrap();
        wait(&s, |e| matches!(e, Event::Capture(true)));
        s.send(Command::Send(b"last bytes".to_vec())).unwrap();
        wait(&s, |e| {
            matches!(
                e,
                Event::Data {
                    direction: Direction::Rx,
                    ..
                }
            )
        });
        s.disconnect();
        wait(&s, |e| matches!(e, Event::Closed));
        assert_eq!(std::fs::read(&path).unwrap(), b"last bytes");
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn cancelled_write_keeps_transport_open() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        stream.set_nonblocking(true).unwrap();
        let cancel = AtomicBool::new(true);
        let mut transport = Transport::Tcp(stream);
        let bytes = b"test";
        let sent = transport
            .write_all(bytes, &AtomicBool::new(false), &cancel, &Stats::default())
            .unwrap();
        assert_eq!(sent, 0);
        assert!(matches!(transport, Transport::Tcp(_)));
        cancel.store(false, Ordering::Relaxed);
        assert_eq!(
            transport
                .write_all(bytes, &AtomicBool::new(false), &cancel, &Stats::default())
                .unwrap(),
            4
        );
        let mut received = [0; 4];
        peer.read_exact(&mut received).unwrap();
        assert_eq!(&received, bytes);
    }
    #[test]
    fn fragmented_tcp_reads_merge_and_capture_remains_byte_exact() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (go_tx, go_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        let peer = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_nodelay(true).unwrap();
            go_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            stream.write_all(b"H").unwrap();
            thread::sleep(Duration::from_millis(3));
            stream.write_all(b"ello, serial!").unwrap();
            finish_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        });
        let s = Session::start(ConnectionConfig {
            mode: Mode::TcpClient,
            remote: addr.to_string(),
            receive_gap_ms: 80,
            ..Default::default()
        });
        wait(&s, |e| matches!(e, Event::Ready(_)));
        let path = temp_file("fragmented");
        s.send(Command::StartCapture(path.clone())).unwrap();
        wait(&s, |e| matches!(e, Event::Capture(true)));
        go_tx.send(()).unwrap();
        let event = wait(&s, |e| {
            matches!(
                e,
                Event::Data {
                    direction: Direction::Rx,
                    ..
                }
            )
        });
        if let Event::Data { bytes, .. } = event {
            assert_eq!(bytes, b"Hello, serial!");
        }
        assert_eq!(s.stats.snapshot(), (14, 0, 0));
        s.send(Command::StopCapture).unwrap();
        wait(&s, |e| matches!(e, Event::Capture(false)));
        assert_eq!(std::fs::read(&path).unwrap(), b"Hello, serial!");
        finish_tx.send(()).unwrap();
        peer.join().unwrap();
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn finite_single_and_multi_schedules_stop_at_exact_count() {
        for steps in [
            vec![Step {
                count: 1,
                bytes: vec![1],
                delay: Duration::from_millis(10),
            }],
            vec![
                Step {
                    count: 1,
                    bytes: vec![1],
                    delay: Duration::from_millis(10),
                },
                Step {
                    count: 1,
                    bytes: vec![2],
                    delay: Duration::from_millis(10),
                },
            ],
        ] {
            let s = loopback();
            let expected = steps
                .iter()
                .flat_map(|s| s.bytes.clone())
                .collect::<Vec<_>>()
                .repeat(3);
            s.send(Command::Schedule {
                steps,
                cycles: Some(3),
            })
            .unwrap();
            let mut sent = Vec::new();
            let mut completed = 0;
            loop {
                match wait(&s, |_| true) {
                    Event::Data {
                        direction: Direction::Tx,
                        bytes,
                        ..
                    } => sent.extend(bytes),
                    Event::ScheduleProgress {
                        completed: n,
                        total,
                    } => {
                        assert_eq!(total, Some(3));
                        completed = n;
                    }
                    Event::SendingStopped => break,
                    _ => (),
                }
            }
            assert_eq!(sent, expected);
            assert_eq!(completed, 3);
            let deadline = Instant::now() + Duration::from_millis(40);
            while let Ok(event) = s
                .events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                assert!(!matches!(
                    event,
                    Event::Data {
                        direction: Direction::Tx,
                        ..
                    }
                ));
            }
        }
    }
    #[test]
    fn zero_cycle_schedule_is_rejected_without_transmitting() {
        let s = loopback();
        s.send(Command::Schedule {
            steps: vec![Step {
                count: 1,
                bytes: vec![1],
                delay: Duration::from_millis(10),
            }],
            cycles: Some(0),
        })
        .unwrap();
        assert!(matches!(
            s.events.recv_timeout(Duration::from_secs(1)).unwrap(),
            Event::Error(_)
        ));
        wait(&s, |e| matches!(e, Event::SendingStopped));
        assert_eq!(s.stats.snapshot().1, 0);
    }
    #[test]
    fn each_row_repeats_before_advancing_in_top_to_bottom_order() {
        let s = loopback();
        s.send(Command::Schedule {
            steps: vec![
                Step {
                    count: 3,
                    bytes: b"A".to_vec(),
                    delay: Duration::from_millis(10),
                },
                Step {
                    count: 2,
                    bytes: b"B".to_vec(),
                    delay: Duration::from_millis(10),
                },
            ],
            cycles: Some(2),
        })
        .unwrap();
        let mut sent = Vec::new();
        let mut completed = 0;
        loop {
            match wait(&s, |_| true) {
                Event::Data {
                    direction: Direction::Tx,
                    bytes,
                    ..
                } => sent.extend(bytes),
                Event::ScheduleProgress { completed: n, .. } => completed = n,
                Event::SendingStopped => break,
                _ => (),
            }
        }
        assert_eq!(sent, b"AAABBAAABB");
        assert_eq!(completed, 2);
        assert_eq!(s.stats.snapshot().1, 10);
    }
}
