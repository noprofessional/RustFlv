mod epoller;
mod http_conn;
mod my_error;

use std::collections::BTreeMap;
use std::convert::TryInto;
use std::env;
use std::fmt;
use std::fs;
use std::io::Error;
use std::io::ErrorKind;
use std::io::Result;
use epoller::Epoller;
use http_conn::HttpListener;
use my_error::my_error;

const FLV_HEADER_LEN: usize = 9;
const TAG_HEADER_LEN: usize = 11;
const TAG_HEADER_DATA_SIZE_LEN: usize = 3;
const TAG_HEADER_TIMESTAMP_LEN: usize = 4;
const TAG_HEADER_STREAM_ID_LEN: usize = 4;
const PRE_TAG_SIZE_LEN: usize = 4;
const AVC_PACKET_COMPOSITION_TIME_LEN: usize = 3;

struct TagHeader {
    data_size: usize,
    timestamp: i32,
}

impl fmt::Display for TagHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[header]:data_size:{}, timestamp:{}",
            self.data_size, self.timestamp
        )
    }
}

impl TagHeader {
    fn parse(mut data: &[u8]) -> Result<(&[u8], Self)> {
        if data.len() < TAG_HEADER_DATA_SIZE_LEN {
            return Err(Error::new(
                ErrorKind::Other,
                "tag header data size parse failed:not enouth data",
            ));
        }

        let data_size =
            ((data[0] as usize) << 16) | ((data[1] as usize) << 8) | ((data[2] as usize) << 0);

        // 前进
        data = &data[TAG_HEADER_DATA_SIZE_LEN..];

        if data.len() < TAG_HEADER_TIMESTAMP_LEN {
            return Err(Error::new(
                ErrorKind::Other,
                "tag header timestamp parse failed:not enouth data",
            ));
        }

        let timestamp = ((data[3] as i32) << 24)
            | ((data[0] as i32) << 16)
            | ((data[1] as i32) << 8)
            | ((data[2] as i32) << 0);

        data = &data[TAG_HEADER_TIMESTAMP_LEN..];

        if data.len() < TAG_HEADER_STREAM_ID_LEN {
            return Err(Error::new(
                ErrorKind::Other,
                "tag header streamid parse failed:not enough data",
            ));
        }

        if &data[0..3] != [0, 0, 0] {
            return Err(Error::new(
                ErrorKind::Other,
                "tag header streamid parse failed:not 0",
            ));
        }
        data = &data[3..];

        return Ok((
            data,
            Self {
                data_size: data_size,
                timestamp: timestamp,
            },
        ));
    }
}
#[allow(dead_code)]
#[derive(Debug)]
enum VideoFrameType {
    KeyFrame,
    InterFrame,
    DisposableInterFrame,
    GeneratedKeyFrame,
    InfoOrCommandFrame,
}

impl fmt::Display for VideoFrameType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VideoFrameType::KeyFrame => write!(f, "[FrameType]:I frame"),
            VideoFrameType::InterFrame => write!(f, "[FrameType]:B/P frame"),
            _ => write!(f, "{:?}", self),
        }
    }
}

impl VideoFrameType {
    fn parse(data: &[u8]) -> Result<(&[u8], Self)> {
        if data.len() == 0 {
            return Err(Error::new(
                ErrorKind::Other,
                "video tag frame type parse failed. reason: not enough data.",
            ));
        }

        match (data[0] & 0xf0) >> 4 {
            1 => Ok((data, Self::KeyFrame)),
            2 => Ok((data, Self::InterFrame)),
            _ => Err(Error::new(
                ErrorKind::Other,
                format!(
                    "video tag frame type parse failed. reason: invalid value {}.",
                    data[0]
                ),
            )),
        }
    }
}

struct AVCNALUData {
    composition_time: u32,
    nalu_data: Vec<u8>,
}

impl AVCNALUData {
    fn parse(data: &[u8]) -> Result<(&[u8], Self)> {
        if data.len() < AVC_PACKET_COMPOSITION_TIME_LEN {
            return Err(Error::new(
                ErrorKind::Other,
                "avc packet parse cts failed: not enough data.",
            ));
        }

        let composition_time =
            ((data[0] as u32) << 16) | ((data[1] as u32) << 8) | ((data[2] as u32) << 0);

        let nalu_data = (&data[AVC_PACKET_COMPOSITION_TIME_LEN..]).to_vec();

        Ok((
            &data[data.len()..],
            Self {
                composition_time: composition_time,
                nalu_data: nalu_data,
            },
        ))
    }
}

impl fmt::Display for AVCNALUData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            " cts:{} nalu size:{}",
            self.composition_time,
            self.nalu_data.len()
        )
    }
}

enum AVCPacketData {
    AVCHeader(Vec<u8>),
    AVCNALU(AVCNALUData),
    AVCEndOfSequence,
}

impl AVCPacketData {
    fn parse(mut data: &[u8]) -> Result<(&[u8], Self)> {
        let avc_packet_type = data[0];
        data = &data[1..];
        match avc_packet_type {
            0 => {
                if data.len() < 3 {
                    return Err(my_error(
                        "avc packet header parsed failed: composition time not enough data",
                    ));
                }
                if data[0..3] != [0, 0, 0] {
                    return Err(my_error(
                        "avc packet header parsed failed: composition time not 0",
                    ));
                }
                data = &data[3..];

                Ok((&data[data.len()..], Self::AVCHeader(Vec::from(data))))
            }

            1 => AVCNALUData::parse(data)
                .and_then(|(rest_data, nalu_data)| Ok((rest_data, Self::AVCNALU(nalu_data))))
                .or_else(|err| {
                    Err(my_error(format!(
                        "avc packet naul data parsed failed:{}",
                        err
                    )))
                }),
            2 => Ok((data, Self::AVCEndOfSequence)),
            _ => Err(Error::new(
                ErrorKind::Other,
                format!(
                    " avc packet data parsed failed. reason: invaild avc packet type{}",
                    data[0]
                ),
            )),
        }
    }
}

impl fmt::Display for AVCPacketData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AVCPacketData::AVCHeader(data) => {
                write!(f, "[avc header]:")?;
                Ok(for byte in data {
                    write!(f, "{:02x} ", byte)?;
                })
            }
            AVCPacketData::AVCNALU(nal_data) => write!(f, "[avc nalu data]:{}", nal_data),
            AVCPacketData::AVCEndOfSequence => write!(f, "avc end of seq"),
        }
    }
}

#[allow(dead_code)]
enum VideoPacket {
    H263,
    Screen,
    VP6,
    VP6Alpha,
    ScreenV2,
    AVC(AVCPacketData),
}

impl VideoPacket {
    fn parse(data: &[u8]) -> Result<(&[u8], Self)> {
        match data[0] & 0x0f {
            7 => AVCPacketData::parse(&data[1..])
                .and_then(|(rest_data, packet_data)| Ok((rest_data, Self::AVC(packet_data)))),
            _ => Err(Error::new(
                ErrorKind::Other,
                format!(
                    "video packet parsed failed. reason: codecid {} not supported.",
                    data[0] & 0x0f
                ),
            )),
        }
    }
}

impl fmt::Display for VideoPacket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VideoPacket::AVC(data) => write!(f, "{}", data),
            _ => write!(f, "unsupported codec type"),
        }
    }
}

struct VideoTag {
    header: TagHeader,
    frame_type: VideoFrameType,
    packet_data: VideoPacket,
}

impl VideoTag {
    fn len(&self) -> usize {
        return TAG_HEADER_LEN + self.header.data_size;
    }

    fn parse(mut data: &[u8]) -> Result<(&[u8], Self)> {
        let (rest_data, header) = TagHeader::parse(data)
            .or_else(|err| Err(my_error(format!("video tag header parse failed:{}", err))))?;

        if rest_data.len() < header.data_size {
            return Err(my_error(format!(
                "script tag body parse failed: not enough data {}/{}",
                rest_data.len(),
                header.data_size
            )));
        }
        data = &rest_data[0..header.data_size];
        let return_data = &rest_data[header.data_size..];

        let (rest_data, frame_type) = VideoFrameType::parse(data).or_else(|err| {
            Err(my_error(format!(
                "video tag frame type parse failed:{}",
                err
            )))
        })?;
        data = rest_data;

        let (_rest_data, packet_data) = VideoPacket::parse(data)
            .or_else(|err| Err(my_error(format!("video tag packet parse failed:{}", err))))?;

        Ok((
            return_data,
            Self {
                header: header,
                frame_type: frame_type,
                packet_data: packet_data,
            },
        ))
    }
}

impl fmt::Display for VideoTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[VideoTag]:{}|{}|{}",
            self.header, self.frame_type, self.packet_data
        )
    }
}

enum SoundFormatType {
    MP3,
    AAC,
}
impl fmt::Display for SoundFormatType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MP3 => write!(f, "[format]:mp3"),
            Self::AAC => write!(f, "[format]:aac"),
        }
    }
}
enum SoundSampleRate {
    Rate5500,
    Rate11k,
    Rate22k,
    Rate44k,
}
impl fmt::Display for SoundSampleRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rate5500 => write!(f, "[rate]:5.5k"),
            Self::Rate11k => write!(f, "[rate]:11k"),
            Self::Rate22k => write!(f, "[rate]:22k"),
            Self::Rate44k => write!(f, "[rate]:44k"),
        }
    }
}
enum SoundSampleSize {
    Size8Bit,
    Size16Bit,
}

impl fmt::Display for SoundSampleSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Size8Bit => write!(f, "[sample size]:8bit"),
            Self::Size16Bit => write!(f, "[sample size]:16bit"),
        }
    }
}

enum SoundType {
    TypeMono,
    TypeStero,
}

impl fmt::Display for SoundType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TypeMono => write!(f, "[type]:mono"),
            Self::TypeStero => write!(f, "[type]:stero"),
        }
    }
}

struct AudioTag {
    header: TagHeader,
    sound_format: SoundFormatType,
    sound_rate: SoundSampleRate,
    sound_size: SoundSampleSize,
    sound_type: SoundType,
    sound_data: Vec<u8>,
}

impl AudioTag {
    fn len(&self) -> usize {
        TAG_HEADER_LEN + self.header.data_size
    }

    fn parse(mut data: &[u8]) -> Result<(&[u8], Self)> {
        let (rest_data, header) = TagHeader::parse(data)
            .or_else(|err| Err(my_error(format!("audio tag header parse failed:{}", err))))?;

        if rest_data.len() < header.data_size {
            return Err(my_error(format!(
                "audio tag body parse failed: not enough data {}/{}",
                rest_data.len(),
                header.data_size
            )));
        }
        data = &rest_data[0..header.data_size];
        let return_data = &rest_data[header.data_size..];

        if data.len() < 1 {
            return Err(my_error("audio tag format parse failed: not enough data"));
        }

        let sound_format = match (data[0] & 0b11110000) >> 4 {
            2 => SoundFormatType::MP3,
            10 => SoundFormatType::AAC,
            _ => {
                return Err(my_error(format!(
                    "audio tag sound format parse failed: unsupported type {}",
                    data[0]
                )))
            }
        };

        let sound_rate = match (data[0] & 0b00001100) >> 2 {
            0 => SoundSampleRate::Rate5500,
            1 => SoundSampleRate::Rate11k,
            2 => SoundSampleRate::Rate22k,
            3 => SoundSampleRate::Rate44k,
            _ => {
                return Err(my_error(format!(
                    "audio tag sound rate failed: unsupported type {}",
                    data[0]
                )))
            }
        };

        let sound_size = match (data[0] & 0b00000010) >> 1 {
            0 => SoundSampleSize::Size8Bit,
            1 => SoundSampleSize::Size16Bit,
            _ => return Err(my_error("not possible")),
        };

        let sound_type = match data[0] & 0b00000001 {
            0 => SoundType::TypeMono,
            1 => SoundType::TypeStero,
            _ => return Err(my_error("not possible")),
        };

        if let SoundFormatType::AAC = sound_format {
            match sound_rate {
                SoundSampleRate::Rate44k => (),
                _ => {
                    return Err(my_error(format!(
                        "audio tag parse failed: AAC rate is not 44k but {}",
                        sound_rate
                    )))
                }
            }

            match sound_size {
                SoundSampleSize::Size16Bit => (),
                _ => {
                    return Err(my_error(format!(
                        "audio tag parse failed: AAC sample size is not 16bit but {}",
                        sound_size
                    )))
                }
            }

            match sound_type {
                SoundType::TypeStero => (),
                _ => {
                    return Err(my_error(format!(
                        "audio tag parse failed: AAC type is not stero but {}",
                        sound_type
                    )))
                }
            }
        }

        data = &data[1..];
        let sound_data = Vec::from(data);
        Ok((
            return_data,
            AudioTag {
                header: header,
                sound_format: sound_format,
                sound_rate: sound_rate,
                sound_size: sound_size,
                sound_type: sound_type,
                sound_data: sound_data,
            },
        ))
    }
}

impl fmt::Display for AudioTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[AudioTag]:{}|{}|{}|{}|{}|{}",
            self.header,
            self.sound_format,
            self.sound_rate,
            self.sound_size,
            self.sound_type,
            self.sound_data.len()
        )
    }
}

struct AMF0Date {
    date_time: f64,
    local_offset: i16,
}

/*
   AMF0_P_Number = 0x00,
   AMF0_P_Boolean = 0x01,
   AMF0_P_String = 0x02,
   AMF0_P_Object = 0x03,
   AMF0_P_MovieClip = 0x04,
   AMF0_P_Null = 0x05,
   AMF0_P_Undefined = 0x06,
   AMF0_P_Reference = 0x07,
   AMF0_P_MixedArray = 0x08,
   AMF0_P_EndOfObject = 0x09,
   AMF0_P_Array = 0x0a,
   AMF0_P_Date = 0x0b,
   AMF0_P_LongString = 0x0c,
*/
enum AMF0 {
    Number(f64),
    Boolean(bool),
    String(String),
    ObjectMap(BTreeMap<String, Box<AMF0>>),
    MovieClip(String),
    Null,
    Undefine,
    Reference(u16),
    ECMAArray((u32, BTreeMap<String, Box<AMF0>>)),
    EndIndicator,
    Array(BTreeMap<String, Box<AMF0>>),
    Date(AMF0Date),
    LongString(String),
}

fn print_map(f: &mut fmt::Formatter<'_>, map: &BTreeMap<String, Box<AMF0>>) -> fmt::Result {
    write!(f, "{{")?;
    for (name, val) in map {
        write!(f, "{}:{},", name, val)?;
    }
    write!(f, "}}")
}

impl AMF0 {
    fn parse(mut data: &[u8]) -> Result<(&[u8], Self)> {
        let amf0_type = data[0];
        data = &data[1..];
        match amf0_type {
            0 => {
                if data.len() < 8 {
                    Err(my_error(
                        "amf0 num parse failed: not enough data.".to_string(),
                    ))
                } else {
                    let number = f64::from_be_bytes((&data[0..8]).try_into().unwrap());
                    data = &data[8..];
                    Ok((data, Self::Number(number)))
                }
            }
            1 => {
                if data.len() < 1 {
                    Err(my_error(
                        "amf0 bool parse failed: not enough data.".to_string(),
                    ))
                } else {
                    let bool_val = data[0] == 0;
                    data = &data[1..];
                    Ok((data, Self::Boolean(bool_val)))
                }
            }
            2 => Self::parse_string(data)
                .and_then(|(rest_data, string_val)| Ok((rest_data, Self::String(string_val))))
                .or_else(|err| Err(my_error(format!("amf0 string parse failed:{}", err)))),
            3 => {
                let mut map = BTreeMap::new();

                loop {
                    if data.len() < 3 {
                        break Err(my_error("amf0 obj map parse failed: end of data."));
                    }

                    if data[0..3] == [0, 0, 9] {
                        break Ok((&data[3..], Self::ObjectMap(map)));
                    }

                    let (rest_data, name) = Self::parse_string(data).or_else(|err| {
                        Err(my_error(format!("amf0 obj map parse name failed:{}", err)))
                    })?;
                    data = rest_data;

                    let (rest_data, val) = Self::parse(data)?;
                    data = rest_data;
                    map.insert(name, Box::new(val));
                }
            }
            4 => Self::parse_string(data)
                .and_then(|(data, val)| Ok((data, Self::MovieClip(val))))
                .or_else(|err| Err(my_error(format!("amf0 string parse failed:{}", err)))),
            5 => Ok((data, Self::Null)),
            6 => Ok((data, Self::Undefine)),
            7 => {
                if data.len() < 2 {
                    Err(my_error("amf0 reference parse failed: not enough data"))
                } else {
                    let val = u16::from_be_bytes((&data[0..2]).try_into().unwrap());
                    data = &data[2..];
                    Ok((data, Self::Reference(val)))
                }
            }
            8 => {
                let mut map = BTreeMap::new();

                if data.len() < 4 {
                    Err(my_error("amf0 ECMA array parse failed: not enough data"))
                } else {
                    // ECMAArrayLen 只是hint 实际的array结束点还是AMF::EndIndicator
                    let hint_len = u32::from_be_bytes(data[0..4].try_into().unwrap());
                    data = &data[4..];
                    println!("hint len:{}", hint_len);
                    loop {
                        if data.len() < 3 {
                            break Err(my_error("amf0 obj map parse failed: end of data."));
                        }

                        if data[0..3] == [0, 0, 9] {
                            break Ok((&data[3..], Self::ECMAArray((hint_len, map))));
                        }

                        let (rest_data, name) = Self::parse_string(data).or_else(|err| {
                            Err(my_error(format!("amf0 obj map parse name failed:{}", err)))
                        })?;
                        data = rest_data;

                        let (rest_data, val) = Self::parse(data)?;
                        data = rest_data;
                        map.insert(name, Box::new(val));
                    }
                }
            }
            9 => Ok((data, AMF0::EndIndicator)),
            10 => {
                let mut map = BTreeMap::new();
                if data.len() < 4 {
                    return Err(my_error("amf0 array parse failed: not enough data"));
                }

                let array_len = u32::from_be_bytes(data.try_into().unwrap());
                for _ in 0..array_len {
                    let (rest_data, amf0_val) = Self::parse(data)?;
                    data = rest_data;
                    if let Self::String(name) = amf0_val {
                        let (rest_data, val) = Self::parse(data)?;
                        data = rest_data;
                        map.insert(name, Box::new(val));
                    } else {
                        return Err(my_error("amf0 array parse failed: name not string"));
                    }
                }
                Ok((data, Self::Array(map)))
            }
            11 => {
                if data.len() < 8 + 2 {
                    return Err(my_error("amf0 date.datetime parse failed: not enough data"));
                }

                let date_time = f64::from_be_bytes(data.try_into().unwrap());
                data = &data[8..];

                let local_offset = i16::from_be_bytes(data.try_into().unwrap());
                data = &data[2..];

                Ok((
                    data,
                    Self::Date(AMF0Date {
                        date_time: date_time,
                        local_offset: local_offset,
                    }),
                ))
            }
            12 => {
                if data.len() < 4 {
                    Err(my_error(
                        "amf0 long string size parse failed: not enough data.",
                    ))
                } else {
                    let string_len = u32::from_be_bytes(data.try_into().unwrap()) as usize;
                    data = &data[4..];
                    if data.len() < string_len {
                        Err(my_error("amf0 long string parse failed: not enough data."))
                    } else {
                        let string_val =
                            String::from_utf8_lossy((&data[0..string_len]).try_into().unwrap())
                                .to_string();
                        Ok((&data[string_len..], AMF0::LongString(string_val)))
                    }
                }
            }
            _ => Err(my_error(format!(
                "amf0 parse failed: unknow type {}",
                amf0_type
            ))),
        }
    }

    fn parse_string(mut data: &[u8]) -> Result<(&[u8], String)> {
        if data.len() < 2 {
            Err(my_error("amf0 string size parse failed: not enough data."))
        } else {
            let string_len = u16::from_be_bytes(data[0..2].try_into().unwrap()) as usize;
            data = &data[2..];
            if data.len() < string_len {
                Err(my_error("amf0 string parse failed: not enough data."))
            } else {
                let string_val =
                    String::from_utf8_lossy((&data[0..string_len]).try_into().unwrap()).to_string();
                Ok((&data[string_len..], string_val))
            }
        }
    }
}

impl fmt::Display for AMF0 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{{")?;
        match self {
            Self::Number(num) => write!(f, "{}", num),
            Self::Boolean(boolean) => write!(f, "{}", boolean),
            Self::String(string) => write!(f, "{}", string),
            Self::ObjectMap(map) => {
                write!(f, "obj map({}):", map.len())?;
                print_map(f, map)
            }
            Self::MovieClip(path) => write!(f, "movie clip path:{}", path),
            Self::Null => write!(f, "null"),
            Self::Undefine => write!(f, "undefine"),
            Self::Reference(val) => write!(f, "reference:{}", val),
            Self::ECMAArray((hint_len, map)) => {
                write!(f, "ecma map({}/{}):", hint_len, map.len())?;
                print_map(f, map)
            }
            Self::EndIndicator => write!(f, "end indicator."),
            Self::Array(map) => {
                write!(f, "array({}):", map.len())?;
                print_map(f, map)
            }
            Self::Date(date_val) => write!(
                f,
                "date:{{ base:{}, locale:{} }}",
                date_val.date_time, date_val.local_offset
            ),
            Self::LongString(string) => write!(f, "long string:{}", string),
        }?;

        write!(f, "}}")
    }
}

struct ScriptTag {
    header: TagHeader,
    obj_name: String,
    obj_val: AMF0,
}

impl ScriptTag {
    fn len(&self) -> usize {
        TAG_HEADER_LEN + self.header.data_size
    }

    fn parse(mut data: &[u8]) -> Result<(&[u8], Self)> {
        let (rest_data, header) = TagHeader::parse(data)
            .or_else(|err| Err(my_error(format!("script tag header parse failed:{}", err))))?;

        if rest_data.len() < header.data_size {
            return Err(my_error(format!(
                "script tag body parse failed: not enough data {}/{}",
                rest_data.len(),
                header.data_size
            )));
        }
        data = &rest_data[0..header.data_size];
        let return_data = &rest_data[header.data_size..];

        let (rest_data, amf0_val) = AMF0::parse(data).or_else(|err| {
            Err(my_error(format!(
                "script tag name parse failed:{{ {} }}",
                err
            )))
        })?;

        data = rest_data;

        if let AMF0::String(obj_name) = amf0_val {
            let (_rest_data, amf0_val) = AMF0::parse(data).or_else(|err| {
                Err(my_error(format!(
                    "script tag val parse failed:{{ {} }}",
                    err
                )))
            })?;

            Ok((
                return_data,
                Self {
                    header: header,
                    obj_name: obj_name,
                    obj_val: amf0_val,
                },
            ))
        } else {
            Err(my_error(
                "script tag parse failed: first type not string no function name",
            ))
        }
    }
}

impl fmt::Display for ScriptTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "script tag:{{name:{{{}}}, val:{}}}",
            self.obj_name, self.obj_val
        )
    }
}

enum FlvTag {
    VideoTag(VideoTag),
    AudioTag(AudioTag),
    ScriptTag(ScriptTag),
}

impl FlvTag {
    fn tag_len(&self) -> usize {
        match self {
            FlvTag::VideoTag(tag_data) => tag_data.len(),
            FlvTag::AudioTag(tag_data) => tag_data.len(),
            FlvTag::ScriptTag(tag_data) => tag_data.len(),
        }
    }

    fn parse(mut data: &[u8]) -> Result<(&[u8], Self)> {
        if data.len() < TAG_HEADER_LEN {
            return Err(Error::new(ErrorKind::Other, "tag has not enouth data"));
        }

        let tag_type = data[0];
        data = &data[1..];

        if tag_type == 8 {
            AudioTag::parse(data)
                .and_then(|(rest_data, tag)| Ok((rest_data, FlvTag::AudioTag(tag))))
        } else if tag_type == 9 {
            VideoTag::parse(data)
                .and_then(|(rest_data, tag)| Ok((rest_data, FlvTag::VideoTag(tag))))
        } else if tag_type == 18 {
            ScriptTag::parse(data)
                .and_then(|(rest_data, tag)| Ok((rest_data, FlvTag::ScriptTag(tag))))
        } else {
            Err(Error::new(
                ErrorKind::Other,
                format!("tag type {} not support", tag_type),
            ))
        }
    }
}

impl fmt::Display for FlvTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FlvTag::AudioTag(tag) => write!(f, "{}", tag),
            FlvTag::VideoTag(tag) => write!(f, "{}", tag),
            FlvTag::ScriptTag(tag) => write!(f, "[ScriptTag]:{}", tag),
        }
    }
}

fn parse_pre_tag_size(data: &[u8]) -> Result<(&[u8], usize)> {
    if data.len() < PRE_TAG_SIZE_LEN {
        Err(Error::new(
            ErrorKind::Other,
            "pre tag size parse failed, reason: not enough data.",
        ))
    } else {
        let size = u32::from_be_bytes((&data[0..PRE_TAG_SIZE_LEN]).try_into().unwrap()) as usize;
        Ok((&data[PRE_TAG_SIZE_LEN..], size))
    }
}

fn parse_flv(mut data: &[u8]) -> Result<()> {
    if data.len() < FLV_HEADER_LEN {
        return Err(my_error("flv header parse failed: not enough data"));
    }

    if data[0] != 'F' as u8 || data[1] != 'L' as u8 || data[2] != 'V' as u8 {
        return Err(Error::new(
            ErrorKind::Other,
            "First Three Bytes is not 'F' 'L' 'V'.",
        ));
    }

    let version = data[3];
    println!("flv version:{}", version);

    let reserved_bit_not_zero = (data[4] & 0b11111010) != 0;
    if reserved_bit_not_zero {
        return Err(Error::new(
            ErrorKind::Other,
            format!("Type flag reserved bit not 0, flag:{:#08b}.", data[4]),
        ));
    }

    let has_video = (data[4] & 0b0000001) != 0;
    let has_audio = (data[4] & 0b0000100) != 0;
    println!(
        "type flag:{:#08b} HasVideo:{} HasAudio:{}",
        data[4], has_video, has_audio
    );

    let data_offset = u32::from_be_bytes((&data[5..9]).try_into().unwrap()) as usize;
    if version == 1 && data_offset != FLV_HEADER_LEN {
        return Err(Error::new(
            ErrorKind::Other,
            format!("flv version 1, but data offset:{} is not 9", data_offset),
        ));
    }
    println!("data offset:{}", data_offset);

    data = &data[data_offset..];
    let first_pre_tag_size = u32::from_be_bytes((&data[0..4]).try_into().unwrap()) as usize;
    if first_pre_tag_size != 0 {
        return Err(Error::new(
            ErrorKind::Other,
            format!("flv first pre tag size is {} not 0", first_pre_tag_size),
        ));
    }

    data = &data[4..];

    let mut tag_cnt = 0;
    while data.len() > 0 {
        tag_cnt += 1;
        let (rest_data, flv_tag) = FlvTag::parse(data)?;
        data = rest_data;

        //println!("[{}]:{}\n", tag_cnt, flv_tag);

        let (rest_data, pre_tag_size) = parse_pre_tag_size(data)?;
        data = rest_data;
        let tag_size = flv_tag.tag_len();
        if pre_tag_size != tag_size {
            return Err(Error::new(
                ErrorKind::Other,
                format!(
                    "flv tag {} : pre tag size {} is not equal to size in tag {}",
                    tag_cnt, pre_tag_size, tag_size
                ),
            ));
        }
    }

    return Ok(());
}

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();

    println!("args {:?}", args);

    if args.len() < 2 {
        return Err(my_error("argument missing! usage: flv-server flv_filename"));
    }

    let filename = &args[1];

    let contents = fs::read(filename)?;

    parse_flv(&contents)?;

    println!("file {} content size:{}", filename, contents.len());

    let running: bool = true;

    let http_listener = HttpListener::bind("192.168.74.3:8848")
        .or_else(|err| Err(my_error(format!("bind http listener failed with {}", err))))?;

    let mut epoller = Epoller::create()?;
    if let Err((http_listener, err)) = epoller.wait_read(http_listener) {
        println!("epoll wait_read for {:?} failed with {}", http_listener, err);
        // listener will close when dropped
        return Err(err);
    }

    while running {
        println!("test {}", line!());
        epoller.run(-1)?;
    }

    return Ok(());
}

#[cfg(test)]
mod tests {
    fn get_timestamp(data: &[u8; 4]) -> i32 {
        ((data[3] as i32) << 24)
            | ((data[0] as i32) << 16)
            | ((data[1] as i32) << 8)
            | ((data[2] as i32) << 0)
    }

    #[test]
    fn test_timestamp() {
        assert_eq!(get_timestamp(&[0x00, 0x00, 0x00, 0x80]), -0x80000000);
        assert_eq!(get_timestamp(&[0x00, 0x00, 0x00, 0x00]), 0);
        assert_eq!(get_timestamp(&[0xff, 0xff, 0xff, 0xff]), -1);
        assert_eq!(get_timestamp(&[0xff, 0xff, 0xfe, 0xff]), -2);
        assert_eq!(get_timestamp(&[0xff, 0xff, 0xff, 0x00]), 0xffffff);
    }
}
