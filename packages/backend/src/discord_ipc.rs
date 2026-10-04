//! Discord 本地 IPC 的传输层: 小端的操作码和长度, 后面跟一段 JSON
//!
//! 自己管这条管道是为了能读到 Discord 的回复。`discord-rich-presence` 的
//! `set_activity` 只写不读, 于是既看不到卡片被拒绝, 回复也会在管道里越积越多。
//! 这里的读取从不阻塞: 先问管道里有多少字节, 有多少读多少。

use std::{
    fs::{
        File,
        OpenOptions,
    },
    io::{
        self,
        Read,
        Write,
    },
    os::windows::io::AsRawHandle,
    thread,
    time::{
        Duration,
        Instant,
    },
};

use serde_json::{
    Value,
    json,
};
use windows::Win32::{
    Foundation::HANDLE,
    System::Pipes::PeekNamedPipe,
};

pub const OP_HANDSHAKE: u32 = 0;
pub const OP_FRAME: u32 = 1;
pub const OP_CLOSE: u32 = 2;
pub const OP_PING: u32 = 3;
pub const OP_PONG: u32 = 4;

const HEADER_LEN: usize = 8;
const MAX_FRAME: usize = 1024 * 1024;
// 内存吃紧的 Discord 可能要好几秒才回 READY, 给得太短会永远连不上
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);
const HANDSHAKE_POLL: Duration = Duration::from_millis(20);

pub trait Transport {
    fn write_frame(&mut self, op: u32, payload: &Value) -> io::Result<()>;
    /// 取走已经到达的完整帧, 没有就返回空, 不等待
    fn read_frames(&mut self) -> io::Result<Vec<(u32, Value)>>;
}

pub trait Connector {
    type Transport: Transport;
    fn connect(&mut self, client_id: &str) -> io::Result<Self::Transport>;
}

fn encode(op: u32, payload: &Value) -> Vec<u8> {
    let body = payload.to_string().into_bytes();
    let mut frame = Vec::with_capacity(HEADER_LEN + body.len());
    frame.extend_from_slice(&op.to_le_bytes());
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(&body);
    frame
}

const fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// 从缓冲区里取出所有完整的帧, 不完整的留到下次
fn take_frames(incoming: &mut Vec<u8>) -> io::Result<Vec<(u32, Value)>> {
    let mut frames = Vec::new();
    let mut consumed = 0;

    while incoming.len() - consumed >= HEADER_LEN {
        let op = le_u32(&incoming[consumed..]);
        let len = le_u32(&incoming[consumed + 4..]) as usize;
        if len > MAX_FRAME {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Discord IPC 帧长度异常: {len}"),
            ));
        }
        if incoming.len() - consumed < HEADER_LEN + len {
            break;
        }

        let body = &incoming[consumed + HEADER_LEN..consumed + HEADER_LEN + len];
        // 解析不了的帧按空值处理, 不值得为它断开连接
        frames.push((op, serde_json::from_slice(body).unwrap_or(Value::Null)));
        consumed += HEADER_LEN + len;
    }

    incoming.drain(..consumed);
    Ok(frames)
}

pub struct PipeTransport {
    pipe: File,
    incoming: Vec<u8>,
}

impl PipeTransport {
    fn available(&self) -> io::Result<usize> {
        let mut available = 0u32;
        // Safety: 句柄来自仍然存活的 File, 其余指针要么为空要么指向本地变量
        unsafe {
            PeekNamedPipe(
                HANDLE(self.pipe.as_raw_handle()),
                None,
                0,
                None,
                Some(&raw mut available),
                None,
            )
        }
        .map_err(io::Error::other)?;
        Ok(available as usize)
    }
}

impl Transport for PipeTransport {
    fn write_frame(&mut self, op: u32, payload: &Value) -> io::Result<()> {
        self.pipe.write_all(&encode(op, payload))
    }

    fn read_frames(&mut self) -> io::Result<Vec<(u32, Value)>> {
        let mut chunk = [0u8; 4096];
        loop {
            let want = self.available()?.min(chunk.len());
            if want == 0 {
                break;
            }
            let read = self.pipe.read(&mut chunk[..want])?;
            if read == 0 {
                break;
            }
            self.incoming.extend_from_slice(&chunk[..read]);
            if self.incoming.len() > MAX_FRAME + HEADER_LEN {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Discord IPC 回复堆积过多",
                ));
            }
        }
        take_frames(&mut self.incoming)
    }
}

#[derive(Debug, Default)]
pub struct PipeConnector;

impl PipeConnector {
    fn handshake(pipe: File, client_id: &str) -> io::Result<PipeTransport> {
        let mut transport = PipeTransport {
            pipe,
            incoming: Vec::new(),
        };
        transport.write_frame(OP_HANDSHAKE, &json!({ "v": 1, "client_id": client_id }))?;

        let started = Instant::now();
        loop {
            for (op, payload) in transport.read_frames()? {
                if op == OP_CLOSE {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionRefused,
                        format!("Discord 拒绝了握手: {payload}"),
                    ));
                }
                if payload.get("evt").and_then(Value::as_str) == Some("READY") {
                    return Ok(transport);
                }
            }
            if started.elapsed() > HANDSHAKE_DEADLINE {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Discord 接受了连接但没有回复 READY",
                ));
            }
            thread::sleep(HANDSHAKE_POLL);
        }
    }
}

impl Connector for PipeConnector {
    type Transport = PipeTransport;

    fn connect(&mut self, client_id: &str) -> io::Result<PipeTransport> {
        for i in 0..10 {
            let path = format!(r"\\.\pipe\discord-ipc-{i}");
            if let Ok(pipe) = OpenOptions::new().read(true).write(true).open(path) {
                return Self::handshake(pipe, client_id);
            }
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "找不到 Discord IPC 管道",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_carry_a_little_endian_header() {
        let bytes = encode(OP_FRAME, &json!({"a": 1}));
        assert_eq!(le_u32(&bytes), OP_FRAME);
        assert_eq!(le_u32(&bytes[4..]) as usize, bytes.len() - HEADER_LEN);
        assert_eq!(&bytes[HEADER_LEN..], br#"{"a":1}"#);
    }

    #[test]
    fn a_split_frame_waits_for_the_rest() {
        let encoded = encode(OP_FRAME, &json!({"evt": "READY"}));
        let mut incoming = encoded[..10].to_vec();
        assert!(take_frames(&mut incoming).unwrap().is_empty());

        incoming.extend_from_slice(&encoded[10..]);
        assert_eq!(
            take_frames(&mut incoming).unwrap(),
            vec![(OP_FRAME, json!({"evt": "READY"}))]
        );
        assert!(incoming.is_empty());
    }

    #[test]
    fn several_frames_come_out_in_order() {
        let mut incoming = encode(OP_PING, &json!({"a": 1}));
        incoming.extend_from_slice(&encode(OP_FRAME, &json!({"b": 2})));
        let frames = take_frames(&mut incoming).unwrap();
        assert_eq!(
            frames,
            vec![(OP_PING, json!({"a": 1})), (OP_FRAME, json!({"b": 2}))]
        );
    }

    #[test]
    fn an_implausible_length_is_an_error() {
        let mut incoming = OP_FRAME.to_le_bytes().to_vec();
        incoming.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(take_frames(&mut incoming).is_err());
    }
}
