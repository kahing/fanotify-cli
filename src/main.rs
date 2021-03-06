use std::ffi::OsStr;
use std::fmt::{Debug, Display};
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Read, Write};
use std::mem;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::{fs::OpenOptionsExt, io::AsRawFd, io::FromRawFd, io::RawFd};
use std::path::PathBuf;
use std::slice;

#[macro_use]
extern crate log;
extern crate env_logger;

use libc;
use libc::{c_int, c_uint};

#[macro_use]
mod c_enum;
use crate::c_enum::EnumValues;
mod flags;
use flags::Opt;

// no good reason, but fanotify(7) uses 200 in the example code
const MAX_FANOTIFY_BUFS: usize = 200;

c_enum! {
    enum FanEvents {
    FAN_ACCESS,
    FAN_MODIFY,
    FAN_CLOSE_WRITE,
    FAN_CLOSE_NOWRITE,
    FAN_OPEN,
    FAN_Q_OVERFLOW,
    FAN_ACCESS_PERM,
    FAN_OPEN_PERM,
    FAN_ONDIR,
    FAN_EVENT_ON_CHILD,
    }
}

c_enum! {
    enum(u32) FanResponse {
    FAN_ALLOW,
    FAN_DENY,
    }
}

// copied from https://github.com/kahing/catfs/blob/daa2b85798fa8ca38306242d51cbc39ed122e271/src/catfs/rlibc.rs#L45
macro_rules! libc_wrap {
    ($( fn $name:ident($($arg:ident : $argtype:ty),*) -> $rettype:ty $body:block )*) => (
        $(
            fn $name($($arg : $argtype),*) -> io::Result<$rettype> {
                let res: $rettype;
                unsafe { res = libc::$name($($arg),*) }
		if res < 0 {
		    return Err(io::Error::last_os_error());
		} else {
		    return Ok(res);
		}
            }
        )*
    );

    ($( fn $name:ident($($arg:ident : $argtype:ty),*) $body:block )*) => (
        $(
            fn $name($($arg : $argtype),*) -> io::Result<c_int> {
                let res: c_int;
                unsafe { res = libc::$name($($arg),*) }
		if res < 0 {
		    return Err(io::Error::last_os_error());
		} else {
		    return Ok(res);
		}
            }
        )*
    );
}

libc_wrap! {
    fn fanotify_init(flags: libc::c_uint, event_f_flags: libc::c_uint) {}
    fn fanotify_mark(fd: c_int, flags: c_uint, mask: u64, dirfd: c_int, path: *const libc::c_char) {}
    fn poll(fds: *mut libc::pollfd, nfds: libc::nfds_t, timeout: c_int) {}
}

fn open_namespace_root(pid: u32) -> io::Result<c_int> {
    let path = format!("/proc/{}/root", pid);
    Ok(OpenOptions::new()
        .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY)
        .open(path)?
        .as_raw_fd())
}

// adapted from https://stackoverflow.com/questions/31046763/does-rust-have-anything-like-scanf
macro_rules! scan {
    ( $string:expr, $( $x:ty ),+ ) => {{
        let mut iter = $string.split(char::is_whitespace);
        ($(iter.next().and_then(|word| word.parse::<$x>().ok()),)*)
    }}
}

#[cfg(test)]
mod scan_test {
    #[test]
    fn scan_2() {
        assert_eq!(scan!("1 2", String, i32), (Some("1".into()), Some(2)));
    }

    #[test]
    fn scan_none() {
        assert_eq!(scan!("a b", i32, i32), (None, None));
    }
}

trait ReadLine {
    fn read_line(&mut self, buf: &mut String) -> io::Result<usize>;
}

impl ReadLine for io::Stdin {
    fn read_line(&mut self, buf: &mut String) -> io::Result<usize> {
        io::Stdin::read_line(self, buf)
    }
}

fn handle_command(
    input: &mut dyn ReadLine,
    buf: &mut String,
    notify: &mut dyn Write,
) -> io::Result<()> {
    if input.read_line(buf)? == 0 {
        Err(io::Error::new(
            ErrorKind::UnexpectedEof,
            io::Error::last_os_error(),
        ))
    } else {
        match scan!(buf, FanResponse, i32) {
            (Some(resp), Some(fd)) => {
                let command = libc::fanotify_response {
                    response: resp as u32,
                    fd: fd,
                };
                let res = notify.write_all(unsafe {
                    slice::from_raw_parts(
                        &command as *const libc::fanotify_response as *const u8,
                        mem::size_of::<libc::fanotify_response>(),
                    )
                });

                // close the file
                unsafe { File::from_raw_fd(fd) };
                res
            }
            _ => {
                error!("invalid input: {}", buf);
                Err(io::Error::new(
                    ErrorKind::InvalidInput,
                    io::Error::last_os_error(),
                ))
            }
        }
    }
}

struct EventEntry {
    mask: u64,
    fd: Option<RawFd>,
    pid: Option<u32>,
    path: Option<PathBuf>,
}

impl EventEntry {
    fn display_field<T: Display>(f: &Option<T>) -> String {
        f.as_ref()
            .map(|f| format!("{}", f))
            .unwrap_or("-".to_string())
    }

    fn write_to(&self, w: &mut dyn Write) -> io::Result<()> {
        let mut mask_buf = String::new();

        for m in FanEvents::values() {
            if (m as u64) & self.mask != 0 {
                mask_buf += format!("{}|", m.as_ref()).as_ref();
            }
        }
        if mask_buf.len() != 0 {
            mask_buf.remove(mask_buf.len() - 1);
        }

        w.write_fmt(format_args!(
            "{}\t{}\t{}\t",
            mask_buf,
            EventEntry::display_field(&self.fd),
            EventEntry::display_field(&self.pid),
        ))?;

        if let Some(file) = &self.path {
            w.write(&file.as_os_str().as_bytes())?;
        } else {
            w.write_all(b"-")?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod event_entry_tests {
    use super::*;

    #[test]
    fn field_display() {
        assert_eq!(EventEntry::display_field(&Some("1")), "1");
        assert_eq!(EventEntry::display_field::<i32>(&None), "-");
    }

    #[test]
    fn entry_display() -> io::Result<()> {
        let mut buf = vec![];
        EventEntry {
            mask: FanEvents::FAN_ACCESS as u64 | FanEvents::FAN_MODIFY as u64,
            fd: Some(2),
            pid: Some(1),
            path: Some("/foo/bar".into()),
        }
        .write_to(&mut buf)?;

        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "FAN_ACCESS|FAN_MODIFY\t2\t1\t/foo/bar"
        );

        Ok(())
    }
}

fn handle_fanotify(
    notify: &mut File,
    fabuf: &mut Vec<libc::fanotify_event_metadata>,
    opt: &Opt,
) -> io::Result<()> {
    let nread = notify.read(unsafe {
        slice::from_raw_parts_mut(
            fabuf.as_mut_ptr() as *mut u8,
            mem::size_of::<libc::fanotify_event_metadata>() * fabuf.len(),
        )
    });

    match nread {
        Err(errno) => match errno.raw_os_error().unwrap() {
            libc::EAGAIN | libc::EINTR => return Ok(()),
            _ => {
                error!("read: {:?}", errno);
                return Err(errno);
            }
        },
        Ok(mut nread) => {
            'next_metadata: for metadata in fabuf {
                if nread < mem::size_of::<libc::fanotify_event_metadata>() as usize
                    || metadata.event_len < mem::size_of::<libc::fanotify_event_metadata>() as u32
                    || metadata.event_len > nread as u32
                {
                    break;
                } else {
                    if metadata.vers != libc::FANOTIFY_METADATA_VERSION {
                        return Err(io::Error::from_raw_os_error(libc::EINVAL));
                    }

                    nread -= metadata.event_len as usize;

                    let file = if metadata.fd >= 0 {
                        let procfd_path = format!("/proc/self/fd/{}", metadata.fd);
                        let path = fs::read_link(procfd_path)?;

                        if metadata.mask & FanEvents::FAN_OPEN_PERM != 0
                            || metadata.mask & FanEvents::FAN_ACCESS_PERM != 0
                        {
                            // wait for command to close it
                        } else {
                            unsafe {
                                // let this drop and close
                                File::from_raw_fd(metadata.fd);
                            };
                        }

                        if opt.recursive {
                            if opt.namespace.is_none() {
                                for p in &opt.paths {
                                    if !path.starts_with(OsStr::from_bytes(&p.as_bytes())) {
                                        debug!("dropping unwanted notification: {:?}", path);
                                        continue 'next_metadata;
                                    }
                                }
                            }
                        }

                        Some(path)
                    } else {
                        None
                    };

                    EventEntry {
                        mask: metadata.mask,
                        fd: if metadata.fd >= 0 {
                            Some(metadata.fd)
                        } else {
                            None
                        },
                        pid: if metadata.pid >= 0 {
                            Some(metadata.pid as u32)
                        } else {
                            None
                        },
                        path: file,
                    }
                    .write_to(&mut io::stdout())?;

                    println!();
                    io::stdout().flush()?;
                }
            }
        }
    };

    return Ok(());
}

fn main() -> io::Result<()> {
    env_logger::init();

    let opt = Opt::from_args_with_default()?;

    let dirfd = match opt.namespace {
        Some(p) => open_namespace_root(p)?,
        None => libc::AT_FDCWD,
    };

    let mut mask = 0;

    for m in opt.events.as_ref().unwrap().split(',') {
        mask = mask
            | m.parse::<FanEvents>()
                .map_err(|e| io::Error::new(ErrorKind::InvalidInput, e))?;

        debug!(
            "adding event {} = {:x}",
            m,
            m.parse::<FanEvents>().unwrap() as u64
        );
    }

    // TODO: fork myself and sleep in the child forever, so this
    // fd is never closed
    let notify_fd = fanotify_init(
        libc::FAN_CLASS_CONTENT | libc::FAN_CLOEXEC | libc::FAN_NONBLOCK,
        (libc::O_CLOEXEC | libc::O_RDONLY | libc::O_LARGEFILE) as u32,
    )?;

    for path in &opt.paths {
        fanotify_mark(
            notify_fd,
            libc::FAN_MARK_ADD
                | if opt.filesystem {
                    libc::FAN_MARK_FILESYSTEM
                } else if opt.mount {
                    libc::FAN_MARK_MOUNT
                } else {
                    0
                },
            mask,
            dirfd,
            path.as_ptr(),
        )?;
    }

    let mut events = vec![
        libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: notify_fd,
            events: libc::POLLIN,
            revents: 0,
        },
    ];

    let mut fabuf = Vec::new();
    fabuf.reserve_exact(MAX_FANOTIFY_BUFS);
    unsafe { fabuf.set_len(MAX_FANOTIFY_BUFS) };

    let mut notify = unsafe { File::from_raw_fd(notify_fd) };
    let mut command_buf = String::new();

    loop {
        let ready = poll(events.as_mut_ptr(), events.len() as libc::nfds_t, -1)?;
        if ready > 0 {
            for e in &events {
                if e.revents > 0 {
                    match e.fd {
                        libc::STDIN_FILENO => {
                            handle_command(&mut io::stdin(), &mut command_buf, &mut notify)?
                        }
                        _ => handle_fanotify(&mut notify, &mut fabuf, &opt)?,
                    }
                }
            }
        }
    }
}
