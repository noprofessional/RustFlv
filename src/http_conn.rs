use crate::epoller::{Epoller, RWHandle};
use crate::my_error::my_error;
use std::collections::BTreeMap;
use std::io::Result as IoResult;
use std::io::{Error, ErrorKind, Read};
use std::net::{TcpListener, TcpStream};
use std::os::unix::io::{AsRawFd, RawFd};
use std::str;

impl RWHandle for TcpListener {
    fn on_read(&mut self, epoller: &mut Epoller) -> IoResult<()> {
        for stream in self.incoming() {
            match stream {
                Ok(s) => {
                    println!("new client! {:?}", s);
                    // s will shutdown when dropped
                    if let Err(nonblock_err) = s.set_nonblocking(true) {
                        println!("client:{:?} set non block failed:{}", s, nonblock_err);
                        continue;
                    }
                    if let Err((s, wait_read_err)) = epoller.wait_read(s) {
                        println!("wait_read client:{:?} failed:{}", s, wait_read_err);
                        continue;
                    }
                }
                Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                    break;
                }
                Err(e) => panic!("encountered IO error: {}", e),
            }
        }
        Ok(())
    }
    fn on_write(&mut self, _epoller: &mut Epoller) -> IoResult<()> {
        panic!("tcp listener should not on write")
    }
}

impl RWHandle for TcpStream {
    fn on_read(&mut self, _: &mut Epoller) -> IoResult<()> {
        let mut buf: [u8; 4096] = [0; 4096];
        let size = self.read(&mut buf)?;
        let input = str::from_utf8(&buf[0..size]).or_else(|err| {
            Err(Error::new(
                ErrorKind::Other,
                format!("tcp stream recv not utf8 chars close, err:{}", err),
            ))
        })?;
        if input.len() == 0 {
            return Err(Error::new(
                ErrorKind::Other,
                format!("tcp stream eof peer closed"),
            ));
        }
        println!("recv input:{}", input);
        Ok(())
    }

    fn on_write(&mut self, _: &mut Epoller) -> IoResult<()> {
        assert!(false);
        Ok(())
    }
}

#[derive(Debug)]
pub struct HttpListener {
    listener: TcpListener,
}

impl HttpListener {
    pub fn bind(address: &str) -> IoResult<Self> {
        let tcplistener = TcpListener::bind(address)?;
        tcplistener.set_nonblocking(true)?;
        return Ok(Self {
            listener: tcplistener,
        });
    }
}

impl AsRawFd for HttpListener {
    fn as_raw_fd(&self) -> RawFd {
        self.listener.as_raw_fd()
    }
}

#[derive(Debug)]
struct HttpStream {
    stream: TcpStream,
    output_buf: Vec<u8>,
}

impl HttpStream {
    fn new(stream: TcpStream) -> Self {
        return Self {
            stream: stream,
            output_buf: Vec::new(),
        };
    }
}

impl AsRawFd for HttpStream {
    fn as_raw_fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }
}

impl RWHandle for HttpListener {
    fn on_read(&mut self, epoller: &mut Epoller) -> IoResult<()> {
        for stream in self.listener.incoming() {
            match stream {
                Ok(s) => {
                    println!("new http client! {:?}", s);
                    if let Err(nonblock_err) = s.set_nonblocking(true) {
                        println!("http client:{:?} set non block failed:{}", s, nonblock_err);
                        continue;
                    }

                    if let Err((conn, wait_read_err)) = epoller.wait_read(HttpStream::new(s)) {
                        println!(
                            "wait_read for http client:{:?} failed:{}",
                            conn, wait_read_err
                        );
                        continue;
                    }
                }
                Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                    break;
                }
                Err(e) => panic!("encountered IO error: {}", e),
            }
        }
        Ok(())
    }
    fn on_write(&mut self, _epoller: &mut Epoller) -> IoResult<()> {
        panic!("http listener should not on write")
    }
}

impl RWHandle for HttpStream {
    fn on_read(&mut self, _epoller: &mut Epoller) -> IoResult<()> {
        let mut buf: [u8; 4096] = [0; 4096];
        let size = self.stream.read(&mut buf)?;
        if size == 0 {
            return Err(my_error(format!("client:{:?} EOF close", self)));
        }
        let req = HttpReq::parse(&buf[0..size])?;
        println!("client asking for {}", req.path);
        Ok(())
    }

    fn on_write(&mut self, _epoller: &mut Epoller) -> IoResult<()> {
        let raw_fd = self.as_raw_fd();
        let ptr = &self.output_buf[0] as *const u8 as *const libc::c_void;
        let sent_size = unsafe { libc::send(raw_fd, ptr, self.output_buf.len(), 0) };
        if sent_size == -1 {
            return Err(Error::last_os_error());
        }
        self.output_buf.drain(0..sent_size as usize);
        Ok(())
    }
}

enum HttpRequestType {
    GET,
    POST,
}

#[allow(dead_code)]
struct HttpReq<'a> {
    req_type: HttpRequestType,
    path: &'a str,
    major_version: u32,
    minor_version: u32,
    addition_param: BTreeMap<&'a str, &'a str>,
    ori_data: String,
}

impl<'a> HttpReq<'a> {
    fn parse(buffer: &'a [u8]) -> IoResult<Self> {
        let ori_data = str::from_utf8(buffer)
            .or_else(|err| Err(my_error(format!("u8 vec to string failed with {}", err))))?;
        let req_type: HttpRequestType;
        let path: &str;
        let major_version: u32;
        let minor_version: u32;
        let mut param_map = BTreeMap::new();

        // parse line by line
        // but first line is special
        let mut lines = ori_data.lines();
        match lines.next() {
            Some(first_line) => {
                let mut parts = first_line.split_ascii_whitespace();
                match parts.next() {
                    Some(req_type_str) => match req_type_str.as_ref() {
                        "GET" => req_type = HttpRequestType::GET,
                        "POST" => req_type = HttpRequestType::POST,
                        _ => {
                            return Err(my_error(format!(
                                "http req has unknow request type:{}",
                                req_type_str
                            )))
                        }
                    },
                    None => return Err(my_error("http req missing request type field")),
                }

                match parts.next() {
                    Some(path_str) => path = path_str,
                    None => return Err(my_error("http req has no path field")),
                }

                match parts.next() {
                    Some(version_str) => {
                        if version_str.len() < 5 {
                            return Err(my_error(format!(
                                "http req version parts not valid:{}",
                                version_str
                            )));
                        }
                        if &version_str[0..5] == "HTTP/" {
                            let mut num_strs = version_str[5..].split('.');
                            let get_num = |iter: Option<&str>| match iter {
                                Some(num_str) => num_str.parse::<u32>().or_else(|err| {
                                    Err(my_error(format!(
                                        "http req parse num:{} failed with:{}",
                                        num_str, err
                                    )))
                                }),
                                None => Err(my_error("http req missing major version number")),
                            };
                            major_version = get_num(num_strs.next())?;
                            minor_version = get_num(num_strs.next())?;
                            let rest = num_strs.collect::<Vec<&str>>().join(".");
                            if rest.len() > 0 {
                                println!("http req ignore strings after minor_version: {}", rest);
                            }
                        } else {
                            return Err(my_error(format!(
                                "http req version field invalid:{}",
                                version_str
                            )));
                        }
                    }
                    None => return Err(my_error("http req has no version field")),
                }
            }
            None => return Err(my_error("http req missing first line")),
        }

        for line in lines {
            if line.len() == 0 {
                break;
            }

            match line.split_once(':') {
                Some((name, value)) => {
                    param_map.insert(name, value);
                    ()
                }
                None => println!("unknow line without ':', ignored: {}", line),
            }
        }

        // TODO body

        return Ok(Self {
            req_type: req_type,
            path: path,
            major_version: major_version,
            minor_version: minor_version,
            addition_param: param_map,
            ori_data: ori_data.to_owned(),
        });
    }
}
