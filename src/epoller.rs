use std::collections::btree_map;
use std::collections::BTreeMap;
use std::io::Error;
use std::io::Result as IoResult;
use std::os::unix::io::{AsRawFd, RawFd};
use std::ptr;

pub trait RWHandle: AsRawFd {
    fn on_read(&mut self, epoller: &mut Epoller) -> IoResult<()>;
    fn on_write(&mut self, epoller: &mut Epoller) -> IoResult<()>;
}

/// Epoller is a wrapper for unix epoll
/// It handles RWHandle which is a wrapper for system raw fd
pub struct Epoller<'a> {
    fd: RawFd,
    fd_to_handle: BTreeMap<RawFd, Box<dyn RWHandle + 'a>>,
}

impl<'a> Epoller<'a> {
    pub fn create() -> IoResult<Self> {
        let res = unsafe { libc::epoll_create1(0) };
        match res {
            -1 => Err(Error::last_os_error()),
            _ => Ok(Self {
                fd: res,
                fd_to_handle: BTreeMap::new(),
            }),
        }
    }

    pub fn wait_read<T: RWHandle + 'a>(&mut self, handle: T) -> Result<(), (T, Error)> {
        let raw_fd = handle.as_raw_fd();
        let mut read_event = libc::epoll_event {
            events: libc::EPOLLIN as u32,
            u64: raw_fd as u64,
        };
        let event_ptr = &mut read_event as *mut libc::epoll_event;

        match self.fd_to_handle.entry(raw_fd) {
            btree_map::Entry::Occupied(_) => {
                let res =
                    unsafe { libc::epoll_ctl(self.fd, libc::EPOLL_CTL_MOD, raw_fd, event_ptr) };
                match res {
                    -1 => Err((handle, Error::last_os_error())),
                    _ => Ok(()),
                }
            }
            btree_map::Entry::Vacant(entry) => {
                let res =
                    unsafe { libc::epoll_ctl(self.fd, libc::EPOLL_CTL_ADD, raw_fd, event_ptr) };
                match res {
                    -1 => Err((handle, Error::last_os_error())),
                    _ => {
                        entry.insert(Box::new(handle));
                        Ok(())
                    }
                }
            }
        }
    }

    pub fn run(&mut self, timeout: i32) -> IoResult<()> {
        let mut event_buffer: [libc::epoll_event; 100] =
            [libc::epoll_event { events: 0, u64: 0 }; 100];
        let ready_cnt = unsafe {
            libc::epoll_wait(
                self.fd,
                &mut event_buffer as *mut libc::epoll_event,
                event_buffer.len() as i32,
                timeout as libc::c_int,
            ) as i32
        };

        for i in 0..ready_cnt as usize {
            let raw_fd = event_buffer[i].u64 as RawFd;
            if let Some(mut boxed_handle) = self.fd_to_handle.remove(&raw_fd) {
                if (event_buffer[i].events & libc::EPOLLIN as u32) != 0 {
                    if let Err(err) = (*boxed_handle).on_read(self) {
                        println!("fd:{} read err:{}", raw_fd, err);
                        let res = unsafe {
                            libc::epoll_ctl(self.fd, libc::EPOLL_CTL_DEL, raw_fd, ptr::null_mut())
                        };
                        assert_ne!(
                            res,
                            -1,
                            "remove fd:{} failed with {}",
                            raw_fd,
                            Error::last_os_error()
                        );
                        continue;
                    }
                }
                if (event_buffer[i].events & libc::EPOLLOUT as u32) != 0 {
                    if let Err(err) = (*boxed_handle).on_write(self) {
                        println!("fd:{} write err:{}", raw_fd, err);
                        let res = unsafe {
                            libc::epoll_ctl(self.fd, libc::EPOLL_CTL_DEL, raw_fd, ptr::null_mut())
                        };
                        assert_ne!(
                            res,
                            -1,
                            "remove fd:{} failed with {}",
                            raw_fd,
                            Error::last_os_error()
                        );
                        continue;
                    }
                }
                self.fd_to_handle.insert(raw_fd, boxed_handle);
            }
        }

        Ok(())
    }
}
