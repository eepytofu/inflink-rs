//! 从网易云的缓存文件头读取音频规格
//!
//! 网易云把音频流信息存进磁盘缓存时不保存采样率, 位深则从来不在它的接口里。
//! 这两样都写在音频文件自己的头部, 而正在播放的文件就在缓存目录里, 所以只读
//! 文件开头的一小段就能拿到真实的数值。
//!
//! 约束:
//! - 只读头部的有限字节, 不读整个文件, 不重新下载音频
//! - 按歌曲 ID 和音频流的 MD5 认准文件, 同一首歌的不同音质是不同的文件
//! - 任何一项校验不过就什么都不发布, 宁可缺也不猜
//! - 位深只有 FLAC 才有意义; 有损格式只读采样率
//!
//! 读取在独立的线程上进行: 缓存目录可能在慢速磁盘上, 不能卡住 SMTC 的更新。

use std::{
    fs::{
        self,
        File,
    },
    io::{
        self,
        Read,
        Seek,
        SeekFrom,
    },
    path::{
        Path,
        PathBuf,
    },
    sync::{
        LazyLock,
        Mutex,
        mpsc::{
            self,
            Sender,
        },
    },
    thread,
};

use tracing::{
    debug,
    warn,
};

use crate::{
    discord,
    model::{
        AudioHeaderRequest,
        AudioHeaderSpecs,
    },
    smtc_core,
};

/// `.uc` 缓存文件的每个字节都与这个值异或过
const CACHE_XOR_KEY: u8 = 0xA3;
/// `fLaC` 标记 (4) + 元数据块头 (4) + STREAMINFO (34)
const FLAC_HEADER_LEN: usize = 42;
/// MP4 的 `moov` 在文件开头时, 音轨的描述都在它的前面一小段里
const MP4_READ_LIMIT: usize = 16 * 1024;
const DURATION_TOLERANCE_MS: f64 = 1000.0;

const KNOWN_SAMPLE_RATES: [u32; 13] = [
    8000, 11_025, 12_000, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000, 88_200, 96_000, 176_400,
    192_000,
];

#[derive(Debug, Clone, Copy, PartialEq)]
struct HeaderSpecs {
    sample_rate: u32,
    bit_depth: Option<u8>,
    duration_ms: Option<f64>,
}

#[derive(Debug, PartialEq, Eq)]
enum Miss {
    /// 缓存目录或文件不存在
    NoFile,
    /// 文件还没写到需要的位置
    NotReady,
    /// 不是认识的格式
    Unrecognized,
    /// 头部的内容自相矛盾或者超出合理范围
    Invalid,
    /// 文件头里的时长和这首歌对不上
    DurationMismatch,
    Io(io::ErrorKind),
}

impl From<io::Error> for Miss {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::NotFound {
            Self::NoFile
        } else {
            Self::Io(e.kind())
        }
    }
}

const fn be_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// 解析 `fLaC` 标记和紧随其后的 STREAMINFO 块 (RFC 9639 第 8.2 节)
fn parse_flac(header: &[u8]) -> Result<HeaderSpecs, Miss> {
    if header.len() < FLAC_HEADER_LEN {
        return Err(Miss::NotReady);
    }
    if &header[..4] != b"fLaC" {
        return Err(Miss::Unrecognized);
    }
    // 第一个元数据块必须是 34 字节的 STREAMINFO (类型 0)
    if header[4] & 0x7F != 0 || be_u32(&[0, header[5], header[6], header[7]]) != 34 {
        return Err(Miss::Invalid);
    }

    let block = &header[8..FLAC_HEADER_LEN];
    let sample_rate =
        (u32::from(block[10]) << 12) | (u32::from(block[11]) << 4) | (u32::from(block[12]) >> 4);
    let bit_depth = (((block[12] & 1) << 4) | (block[13] >> 4)) + 1;
    let total_samples = (u64::from(block[13] & 0x0F) << 32) | u64::from(be_u32(&block[14..18]));

    if sample_rate == 0 || !(4..=32).contains(&bit_depth) {
        return Err(Miss::Invalid);
    }

    Ok(HeaderSpecs {
        sample_rate,
        bit_depth: Some(bit_depth),
        // 0 表示编码器没有写总采样数
        duration_ms: (total_samples > 0)
            .then(|| total_samples as f64 * 1000.0 / f64::from(sample_rate)),
    })
}

/// 在一段 MP4 数据里找指定类型的盒子, 返回它的内容 (可能因为只读了开头而被截断)
fn find_box(mut data: &[u8], kind: [u8; 4]) -> Option<&[u8]> {
    while data.len() >= 8 {
        let size = be_u32(data) as usize;
        // 64 位长度和 "一直到文件末尾" 的盒子在这里用不上
        if size < 8 {
            return None;
        }
        let end = size.min(data.len());
        if data[4..8] == kind {
            return Some(&data[8..end]);
        }
        if size >= data.len() {
            return None;
        }
        data = &data[size..];
    }
    None
}

/// 从 MP4 开头的 `moov` 里读出音轨的采样率
///
/// 采样率在两个地方各写了一次 (`mdhd` 的时间刻度和 `mp4a` 的采样率), 两处一致
/// 才采用。
fn parse_mp4(data: &[u8]) -> Result<HeaderSpecs, Miss> {
    if data.len() < 12 || &data[4..8] != b"ftyp" {
        return Err(Miss::Unrecognized);
    }
    // moov 不在开头 (放在文件末尾) 或者还没写完时, 这里读不到
    let moov = find_box(data, *b"moov").ok_or(Miss::NotReady)?;

    let mut rest = moov;
    while rest.len() >= 8 {
        let size = be_u32(rest) as usize;
        if size < 8 {
            break;
        }
        let end = size.min(rest.len());
        if &rest[4..8] == b"trak"
            && let Some(specs) = parse_mp4_audio_track(&rest[8..end])
        {
            return specs;
        }
        if size >= rest.len() {
            break;
        }
        rest = &rest[size..];
    }
    Err(Miss::NotReady)
}

/// 不是音轨时返回 `None`
fn parse_mp4_audio_track(trak: &[u8]) -> Option<Result<HeaderSpecs, Miss>> {
    let mdia = find_box(trak, *b"mdia")?;
    let handler = find_box(mdia, *b"hdlr")?;
    if handler.get(8..12)? != b"soun" {
        return None;
    }

    Some((|| {
        let mdhd = find_box(mdia, *b"mdhd").ok_or(Miss::NotReady)?;
        let (timescale, duration) = match mdhd.first() {
            Some(0) if mdhd.len() >= 20 => {
                (be_u32(&mdhd[12..16]), u64::from(be_u32(&mdhd[16..20])))
            }
            Some(1) if mdhd.len() >= 32 => (
                be_u32(&mdhd[20..24]),
                (u64::from(be_u32(&mdhd[24..28])) << 32) | u64::from(be_u32(&mdhd[28..32])),
            ),
            _ => return Err(Miss::Invalid),
        };

        let stsd = find_box(mdia, *b"minf")
            .and_then(|minf| find_box(minf, *b"stbl"))
            .and_then(|stbl| find_box(stbl, *b"stsd"))
            .ok_or(Miss::NotReady)?;
        // 版本和标志 (4) + 条目数 (4), 然后是第一个采样描述
        let entry = stsd.get(8..).ok_or(Miss::NotReady)?;
        if entry.len() < 36 {
            return Err(Miss::NotReady);
        }
        if &entry[4..8] != b"mp4a" {
            return Err(Miss::Unrecognized);
        }
        // 16.16 定点数
        let sample_rate = be_u32(&entry[32..36]) >> 16;

        if sample_rate != timescale || !KNOWN_SAMPLE_RATES.contains(&sample_rate) {
            return Err(Miss::Invalid);
        }

        Ok(HeaderSpecs {
            sample_rate,
            bit_depth: None,
            duration_ms: (duration > 0).then(|| duration as f64 * 1000.0 / f64::from(timescale)),
        })
    })())
}

/// 解析一个 MPEG 音频帧头, 返回 (采样率, 这一帧的字节数)。只接受 Layer III
fn parse_mp3_frame(header: &[u8]) -> Option<(u32, u64)> {
    const V1_KBPS: [u64; 14] = [
        32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
    ];
    const V2_KBPS: [u64; 14] = [8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];

    if header.len() < 4 || header[0] != 0xFF || header[1] & 0xE0 != 0xE0 {
        return None;
    }
    let version = (header[1] >> 3) & 0b11;
    let layer = (header[1] >> 1) & 0b11;
    let bitrate_index = usize::from(header[2] >> 4);
    let rate_index = usize::from((header[2] >> 2) & 0b11);
    let padding = u64::from((header[2] >> 1) & 1);

    if layer != 0b01 || bitrate_index == 0 || bitrate_index == 15 || rate_index == 3 {
        return None;
    }

    let (rates, kbps, samples_per_frame_over_8): ([u32; 3], &[u64; 14], u64) = match version {
        0b11 => ([44_100, 48_000, 32_000], &V1_KBPS, 144),
        0b10 => ([22_050, 24_000, 16_000], &V2_KBPS, 72),
        0b00 => ([11_025, 12_000, 8000], &V2_KBPS, 72),
        _ => return None,
    };

    let sample_rate = rates[rate_index];
    let frame_len = samples_per_frame_over_8 * kbps[bitrate_index - 1] * 1000
        / u64::from(sample_rate)
        + padding;
    Some((sample_rate, frame_len))
}

/// 读取并还原缓存文件里的一段, 文件不够长时返回实际读到的部分
fn read_decoded(file: &mut File, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset))?;
    let mut buffer = Vec::with_capacity(len);
    file.take(len as u64).read_to_end(&mut buffer)?;
    for byte in &mut buffer {
        *byte ^= CACHE_XOR_KEY;
    }
    Ok(buffer)
}

/// MP3: 跳过开头的 `ID3v2` 标签, 读第一帧的帧头, 再确认下一帧的位置上也是同样的帧头
fn probe_mp3(file: &mut File, head: &[u8]) -> Result<HeaderSpecs, Miss> {
    let first_frame_at = if head.starts_with(b"ID3") {
        if head.len() < 10 {
            return Err(Miss::NotReady);
        }
        // 标签长度是 4 个 7 位的字节, 不含 10 字节的标签头; 第 4 位标志表示还有 10 字节的尾部
        let size = head[6..10]
            .iter()
            .fold(0u64, |acc, b| (acc << 7) | u64::from(b & 0x7F));
        let footer = if head[5] & 0x10 == 0 { 0 } else { 10 };
        10 + size + footer
    } else {
        0
    };

    let first = read_decoded(file, first_frame_at, 4)?;
    if first.len() < 4 {
        return Err(Miss::NotReady);
    }
    let (sample_rate, frame_len) = parse_mp3_frame(&first).ok_or(Miss::Unrecognized)?;

    let second = read_decoded(file, first_frame_at + frame_len, 4)?;
    if second.len() < 4 {
        return Err(Miss::NotReady);
    }
    match parse_mp3_frame(&second) {
        Some((next_rate, _)) if next_rate == sample_rate => Ok(HeaderSpecs {
            sample_rate,
            bit_depth: None,
            duration_ms: None,
        }),
        _ => Err(Miss::Invalid),
    }
}

fn probe_file(path: &Path, expected_duration_ms: Option<f64>) -> Result<HeaderSpecs, Miss> {
    // 标准库在 Windows 上默认允许其他进程同时读写和删除, 不会妨碍网易云继续写缓存
    let mut file = File::open(path)?;

    let head = read_decoded(&mut file, 0, 12)?;
    if head.len() < 12 {
        return Err(Miss::NotReady);
    }

    let specs = if head.starts_with(b"fLaC") {
        parse_flac(&read_decoded(&mut file, 0, FLAC_HEADER_LEN)?)?
    } else if &head[4..8] == b"ftyp" {
        parse_mp4(&read_decoded(&mut file, 0, MP4_READ_LIMIT)?)?
    } else if head.starts_with(b"ID3") || parse_mp3_frame(&head).is_some() {
        probe_mp3(&mut file, &head)?
    } else {
        return Err(Miss::Unrecognized);
    };

    if let (Some(expected), Some(actual)) = (expected_duration_ms, specs.duration_ms)
        && (expected - actual).abs() > DURATION_TOLERANCE_MS
    {
        return Err(Miss::DurationMismatch);
    }

    Ok(specs)
}

fn is_md5(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// 缓存文件名形如 `<歌曲 ID>-<音质代码>-<MD5>.uc`
///
/// 音质代码不参与匹配: MD5 已经唯一确定了音频流, 而前端在切歌的瞬间读到的音质
/// 代码可能还是上一首歌的。
fn find_cache_file(dir: &Path, ncm_id: u64, md5: &str) -> Option<PathBuf> {
    if !is_md5(md5) {
        return None;
    }
    let prefix = format!("{ncm_id}-");
    let suffix = format!("-{}.uc", md5.to_ascii_lowercase());

    fs::read_dir(dir).ok()?.flatten().find_map(|entry| {
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        (name.starts_with(&prefix) && name.ends_with(&suffix)).then(|| entry.path())
    })
}

fn cache_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|dir| {
        dir.join("NetEase")
            .join("CloudMusic")
            .join("Cache")
            .join("Cache")
    })
}

fn probe(dir: &Path, request: &AudioHeaderRequest) -> Result<HeaderSpecs, Miss> {
    let path = find_cache_file(dir, request.ncm_id, &request.md5).ok_or(Miss::NoFile)?;
    probe_file(&path, request.duration_ms)
}

fn run(requests: &mpsc::Receiver<AudioHeaderRequest>) {
    let Some(dir) = cache_dir() else {
        warn!("找不到网易云的缓存目录, 无法读取音频文件头");
        return;
    };

    for request in requests {
        match probe(&dir, &request) {
            Ok(specs) => {
                debug!(
                    seq = request.seq,
                    ncm_id = request.ncm_id,
                    sample_rate = specs.sample_rate,
                    bit_depth = ?specs.bit_depth,
                    "已从缓存文件头读到音频规格"
                );
                // 直接交给 Discord 的工作线程: 绕回前端要经过渲染线程, 而切歌时渲染线程
                // 正忙, 一来一回要几百毫秒, 赶不上第一张卡片
                discord::update_audio_header(AudioHeaderSpecs {
                    stream: request.stream,
                    sample_rate: specs.sample_rate,
                    bit_depth: specs.bit_depth,
                });
                // 前端也要一份, 公开的 `getCurrentAudioInfo()` 靠它
                smtc_core::emit_audio_header(
                    request.seq,
                    request.ncm_id,
                    request.md5,
                    specs.sample_rate,
                    specs.bit_depth,
                );
            }
            // 前端会在之后的进度事件里有限次地重试
            Err(miss) => debug!(
                seq = request.seq,
                ncm_id = request.ncm_id,
                ?miss,
                "这次没能从缓存文件头读到音频规格"
            ),
        }
    }
}

static SENDER: LazyLock<Mutex<Option<Sender<AudioHeaderRequest>>>> =
    LazyLock::new(|| Mutex::new(None));

pub fn request(request: AudioHeaderRequest) {
    let Ok(mut guard) = SENDER.lock() else {
        return;
    };

    let sender = guard.get_or_insert_with(|| {
        let (tx, rx) = mpsc::channel();
        if let Err(e) = thread::Builder::new()
            .name("audio-header".into())
            .spawn(move || run(&rx))
        {
            warn!("无法启动音频文件头读取线程: {e}");
        }
        tx
    });

    if sender.send(request).is_err() {
        // 线程已经退出 (例如找不到缓存目录), 下次请求时不再重复尝试
        debug!("音频文件头读取线程不可用");
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    const MD5: &str = "0123456789abcdef0123456789abcdef";

    /// 44 字节: `fLaC` + STREAMINFO, 再多两个字节模拟后面的内容
    fn flac_header(sample_rate: u32, bit_depth: u8, total_samples: u64) -> Vec<u8> {
        let mut header = b"fLaC".to_vec();
        header.extend_from_slice(&[0x00, 0x00, 0x00, 34]);
        let mut block = [0u8; 34];
        block[10] = (sample_rate >> 12) as u8;
        block[11] = (sample_rate >> 4) as u8;
        // 采样率的低 4 位 | 声道数减一 (立体声) | 位深减一的最高位
        block[12] = ((sample_rate & 0x0F) as u8) << 4 | (1 << 1) | ((bit_depth - 1) >> 4);
        block[13] = ((bit_depth - 1) & 0x0F) << 4 | ((total_samples >> 32) & 0x0F) as u8;
        block[14..18].copy_from_slice(&(total_samples as u32).to_be_bytes());
        header.extend_from_slice(&block);
        header.extend_from_slice(&[0xAA, 0xBB]);
        header
    }

    fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }

    fn mp4_header(timescale: u32, entry_rate: u32, duration: u32, handler: &[u8; 4]) -> Vec<u8> {
        let mut mdhd = vec![0u8; 12];
        mdhd.extend_from_slice(&timescale.to_be_bytes());
        mdhd.extend_from_slice(&duration.to_be_bytes());
        mdhd.extend_from_slice(&[0; 4]);

        let mut hdlr = vec![0u8; 8];
        hdlr.extend_from_slice(handler);
        hdlr.extend_from_slice(&[0; 13]);

        let mut entry = vec![0u8; 24];
        entry.extend_from_slice(&(entry_rate << 16).to_be_bytes());
        let mut stsd = vec![0, 0, 0, 0, 0, 0, 0, 1];
        stsd.extend_from_slice(&mp4_box(b"mp4a", &entry));

        let stbl = mp4_box(b"stbl", &mp4_box(b"stsd", &stsd));
        let minf = mp4_box(b"minf", &stbl);
        let mdia = mp4_box(
            b"mdia",
            &[mp4_box(b"mdhd", &mdhd), mp4_box(b"hdlr", &hdlr), minf].concat(),
        );
        let moov = mp4_box(
            b"moov",
            &[mp4_box(b"mvhd", &[0; 100]), mp4_box(b"trak", &mdia)].concat(),
        );
        [mp4_box(b"ftyp", b"M4A \0\0\0\0isomiso2"), moov].concat()
    }

    /// MPEG-1 Layer III, 320 kbps, 44.1 kHz, 无填充: 帧长 1044 字节
    const MP3_FRAME: [u8; 4] = [0xFF, 0xFB, 0xE0, 0x00];

    fn mp3_file(id3_body: usize) -> Vec<u8> {
        let mut data = Vec::new();
        if id3_body > 0 {
            data.extend_from_slice(b"ID3\x03\x00\x00");
            data.extend_from_slice(&[
                (id3_body >> 21) as u8 & 0x7F,
                (id3_body >> 14) as u8 & 0x7F,
                (id3_body >> 7) as u8 & 0x7F,
                id3_body as u8 & 0x7F,
            ]);
            data.resize(10 + id3_body, 0);
        }
        for _ in 0..2 {
            let start = data.len();
            data.extend_from_slice(&MP3_FRAME);
            data.resize(start + 1044, 0x55);
        }
        data
    }

    struct CacheDir(PathBuf);

    impl CacheDir {
        fn new(label: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "inflink-header-test-{}-{label}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn write(&self, name: &str, plain: &[u8]) {
            let encoded: Vec<u8> = plain.iter().map(|b| b ^ CACHE_XOR_KEY).collect();
            File::create(self.0.join(name))
                .unwrap()
                .write_all(&encoded)
                .unwrap();
        }

        fn probe(&self, ncm_id: u64, duration_ms: Option<f64>) -> Result<HeaderSpecs, Miss> {
            probe(
                &self.0,
                &AudioHeaderRequest {
                    seq: 1,
                    stream: 1,
                    ncm_id,
                    md5: MD5.to_string(),
                    duration_ms,
                },
            )
        }
    }

    impl Drop for CacheDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// 数值取自实机的三个缓存文件
    #[test]
    fn flac_stream_info_gives_rate_depth_and_duration() {
        let cases = [
            (48_000, 24, 13_896_000_u64, 289_500.0),
            (44_100, 24, 8_440_475, 191_394.0),
            (48_000, 16, 13_403_040, 279_230.0),
        ];
        for (rate, depth, samples, duration) in cases {
            let specs = parse_flac(&flac_header(rate, depth, samples)).unwrap();
            assert_eq!(specs.sample_rate, rate);
            assert_eq!(specs.bit_depth, Some(depth));
            assert!((specs.duration_ms.unwrap() - duration).abs() < 1.0);
        }
    }

    #[test]
    fn hi_res_rates_survive_the_20_bit_field() {
        let specs = parse_flac(&flac_header(192_000, 24, 0)).unwrap();
        assert_eq!(specs.sample_rate, 192_000);
        assert_eq!(specs.duration_ms, None);
    }

    #[test]
    fn a_flac_header_that_fails_any_check_yields_nothing() {
        let good = flac_header(48_000, 24, 1000);

        assert_eq!(parse_flac(&good[..41]), Err(Miss::NotReady));

        let mut wrong_block = good.clone();
        wrong_block[4] = 0x04;
        assert_eq!(parse_flac(&wrong_block), Err(Miss::Invalid));

        let mut wrong_length = good.clone();
        wrong_length[7] = 33;
        assert_eq!(parse_flac(&wrong_length), Err(Miss::Invalid));

        assert_eq!(parse_flac(&flac_header(0, 24, 1000)), Err(Miss::Invalid));
        assert_eq!(
            parse_flac(&flac_header(48_000, 2, 1000)),
            Err(Miss::Invalid)
        );
    }

    #[test]
    fn mp4_rate_needs_both_fields_to_agree() {
        let specs = parse_mp4(&mp4_header(44_100, 44_100, 44_100 * 200, b"soun")).unwrap();
        assert_eq!(specs.sample_rate, 44_100);
        assert_eq!(specs.bit_depth, None);
        assert!((specs.duration_ms.unwrap() - 200_000.0).abs() < 1.0);

        assert_eq!(
            parse_mp4(&mp4_header(48_000, 44_100, 1, b"soun")),
            Err(Miss::Invalid)
        );
        assert_eq!(
            parse_mp4(&mp4_header(12_345, 12_345, 1, b"soun")),
            Err(Miss::Invalid)
        );
        // 没有音轨
        assert_eq!(
            parse_mp4(&mp4_header(44_100, 44_100, 1, b"vide")),
            Err(Miss::NotReady)
        );
    }

    #[test]
    fn a_truncated_mp4_is_not_ready_rather_than_wrong() {
        let full = mp4_header(48_000, 48_000, 48_000, b"soun");
        for cut in [20, 60, 150, full.len() - 10] {
            assert_eq!(parse_mp4(&full[..cut]), Err(Miss::NotReady), "cut at {cut}");
        }
    }

    #[test]
    fn mp3_frame_headers() {
        assert_eq!(parse_mp3_frame(&MP3_FRAME), Some((44_100, 1044)));
        // MPEG-1 Layer III, 128 kbps, 48 kHz, 有填充
        assert_eq!(
            parse_mp3_frame(&[0xFF, 0xFB, 0x96, 0x00]),
            Some((48_000, 385))
        );
        // Layer II、保留的采样率、空闲码率都不接受
        assert_eq!(parse_mp3_frame(&[0xFF, 0xFD, 0xE0, 0x00]), None);
        assert_eq!(parse_mp3_frame(&[0xFF, 0xFB, 0xEC, 0x00]), None);
        assert_eq!(parse_mp3_frame(&[0xFF, 0xFB, 0x00, 0x00]), None);
        assert_eq!(parse_mp3_frame(&[0x00, 0xFB, 0xE0, 0x00]), None);
    }

    #[test]
    fn the_file_is_picked_by_song_and_md5_not_by_song_alone() {
        let dir = CacheDir::new("identity");
        dir.write(
            "42-999-ffffffffffffffffffffffffffffffff.uc",
            &flac_header(44_100, 16, 0),
        );
        assert_eq!(dir.probe(42, None), Err(Miss::NoFile));

        dir.write(&format!("42-1999-{MD5}.uc"), &flac_header(96_000, 24, 0));
        let specs = dir.probe(42, None).unwrap();
        assert_eq!((specs.sample_rate, specs.bit_depth), (96_000, Some(24)));

        // 另一首歌碰巧前缀相同
        assert_eq!(dir.probe(4, None), Err(Miss::NoFile));
    }

    #[test]
    fn nothing_is_published_until_every_check_passes() {
        let dir = CacheDir::new("checks");
        let name = format!("7-999-{MD5}.uc");

        assert_eq!(dir.probe(7, None), Err(Miss::NoFile));

        dir.write(&name, &flac_header(48_000, 24, 0)[..10]);
        assert_eq!(dir.probe(7, None), Err(Miss::NotReady));

        dir.write(&name, &flac_header(48_000, 24, 0)[..30]);
        assert_eq!(dir.probe(7, None), Err(Miss::NotReady));

        // 预分配但还没写入的文件, 还原之后不是任何已知格式
        dir.write(&name, &[0xA3; 64]);
        assert_eq!(dir.probe(7, None), Err(Miss::Unrecognized));

        dir.write(&name, &flac_header(48_000, 24, 48_000 * 100));
        assert_eq!(dir.probe(7, Some(250_000.0)), Err(Miss::DurationMismatch));
        assert!(dir.probe(7, Some(100_400.0)).is_ok());
    }

    #[test]
    fn lossy_files_give_a_rate_and_no_bit_depth() {
        let dir = CacheDir::new("lossy");

        dir.write(
            &format!("1-320-{MD5}.uc"),
            &mp4_header(44_100, 44_100, 44_100 * 180, b"soun"),
        );
        let specs = dir.probe(1, Some(180_000.0)).unwrap();
        assert_eq!((specs.sample_rate, specs.bit_depth), (44_100, None));

        dir.write(&format!("2-320-{MD5}.uc"), &mp3_file(88));
        let specs = dir.probe(2, Some(180_000.0)).unwrap();
        assert_eq!((specs.sample_rate, specs.bit_depth), (44_100, None));

        dir.write(&format!("3-320-{MD5}.uc"), &mp3_file(0));
        assert_eq!(dir.probe(3, None).unwrap().sample_rate, 44_100);

        // 第二帧的位置上不是帧头: 第一帧多半是误判
        let mut broken = mp3_file(0);
        broken[1044] = 0x00;
        dir.write(&format!("4-320-{MD5}.uc"), &broken);
        assert_eq!(dir.probe(4, None), Err(Miss::Invalid));
    }

    #[test]
    fn a_malformed_md5_never_reaches_the_file_system() {
        let dir = CacheDir::new("md5");
        dir.write("5-999-..uc", &flac_header(48_000, 24, 0));
        assert_eq!(find_cache_file(&dir.0, 5, "."), None);
        assert_eq!(find_cache_file(&dir.0, 5, &"g".repeat(32)), None);
    }
}
