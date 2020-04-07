use std::error;
use std::ffi::{CString, OsStr, OsString};
use std::fmt::{self, Debug, Display, Formatter};
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::marker::PhantomData;
use std::mem;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::{fs::OpenOptionsExt, io::AsRawFd, io::FromRawFd};
use std::path::PathBuf;
use std::str::FromStr;

#[macro_use]
extern crate log;
extern crate env_logger;

use libc;
use libc::{c_int, c_uint, c_void};
use structopt::StructOpt;

fn cstring_from_os_str(src: &OsStr) -> Result<CString, OsString> {
    return CString::new(src.to_os_string().into_vec())
        .map_err(|e| OsString::from(format!("unexpected \\0 at pos {}", e.nul_position())));
}

#[derive(Debug, StructOpt)]
#[structopt(about)]
struct Opt {
    /// default: FAN_ACCESS,FAN_MODIFY,FAN_CLOSE_WRITE,FAN_CLOSE_NOWRITE,FAN_OPEN,FAN_ONDIR,FAN_EVENT_ON_CHILD
    #[structopt(short, long)]
    events: Option<String>,

    /// paths are relative to the filesystem namespace of this process
    #[structopt(short = "p", long = "process")]
    namespace: Option<u32>,

    /// recursively monitor everything under paths, implies -m unless -f is used
    #[structopt(short, long)]
    recursive: bool,

    /// notify for the mount point, implies -r
    #[structopt(short, long)]
    mount: bool,

    /// notify for the filesystem, implies -r
    #[structopt(short, long)]
    filesystem: bool,

    #[structopt(parse(try_from_os_str = cstring_from_os_str))]
    paths: Vec<CString>,
}

const DEFAULT_EVENTS: &str =
    "FAN_ACCESS,FAN_MODIFY,FAN_CLOSE_WRITE,FAN_CLOSE_NOWRITE,FAN_OPEN,FAN_ONDIR,FAN_EVENT_ON_CHILD";
// no good reason, but fanotify(7) uses 200 in the example code
const MAX_FANOTIFY_BUFS: usize = 200;

trait EnumValues {
    type Enum: Debug;

    fn values() -> Vec<Self::Enum>;
}

struct CEnumParseError<T: EnumValues>(String, PhantomData<T>);

impl<T> CEnumParseError<T>
where
    T: EnumValues + Send + Sync,
{
    fn new<S: AsRef<str> + Sized>(name: S) -> CEnumParseError<T> {
        CEnumParseError::<T>(name.as_ref().into(), PhantomData)
    }
}

impl<T> error::Error for CEnumParseError<T> where T: EnumValues + Send + Sync {}

impl<T> Debug for CEnumParseError<T>
where
    T: EnumValues,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid value: {}, options: {}",
            self.0,
            T::values()
                .iter()
                .map(|e| format!("{:?}", e))
                .collect::<Vec<String>>()
                .join(", ")
        )
    }
}

impl<T> Display for CEnumParseError<T>
where
    T: EnumValues,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

macro_rules! c_enum {
    (
	$(enum $name:ident {
	    $($flag:ident),* $(,)*
	})*
    ) => (
     $(
	 #[repr(u64)]
	 #[derive(Copy, Clone, Debug)]
	 #[allow(non_camel_case_types)]
	 enum $name {
	     $($flag = libc::$flag),*
	 }

	 impl FromStr for $name {
	     type Err = CEnumParseError<$name>;

	     fn from_str(s: &str) -> Result<Self, Self::Err> {
		 match s {
		     $( stringify!($flag) => Ok($name::$flag), )*
		     _ => Err(CEnumParseError::new(s))
		 }
	     }
	 }

	 impl AsRef<str> for $name {
	     fn as_ref(&self) -> &str {
		 match self {
		     $( $name::$flag => stringify!($flag), )*
		 }
	     }
	 }

	 impl EnumValues for $name {
	     type Enum = $name;

	     fn values() -> Vec<$name> {
		 vec![$($name::$flag),*]
	     }
	 }

     )*
    );
}

c_enum! {
    enum FanEvents {
    FAN_ACCESS,
    FAN_MODIFY,
    FAN_CLOSE_WRITE,
    FAN_CLOSE_NOWRITE,
    FAN_OPEN,
    FAN_ONDIR,
    FAN_EVENT_ON_CHILD,
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

libc_wrap! {
    fn read(fd: c_int, buf: *mut c_void, count: usize) -> isize {}
}

fn open_namespace_root(pid: u32) -> io::Result<c_int> {
    let path = format!("/proc/{}/root", pid);
    Ok(OpenOptions::new()
        .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY)
        .open(path)?
        .as_raw_fd())
}

fn handle_fanotify(
    fd: c_int,
    fabuf: &mut Vec<libc::fanotify_event_metadata>,
    opt: &Opt,
) -> io::Result<()> {
    let nread = read(
        fd,
        fabuf.as_mut_ptr() as *mut c_void,
        mem::size_of::<libc::fanotify_event_metadata>() * fabuf.len(),
    );

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
                if nread < mem::size_of::<libc::fanotify_event_metadata>() as isize
                    || metadata.event_len < mem::size_of::<libc::fanotify_event_metadata>() as u32
                    || metadata.event_len > nread as u32
                {
                    break;
                } else {
                    if metadata.vers != libc::FANOTIFY_METADATA_VERSION {
                        return Err(io::Error::from_raw_os_error(libc::EINVAL));
                    }

                    nread -= metadata.event_len as isize;

                    let mut mask_buf = String::new();

                    for m in FanEvents::values() {
                        if (m as u64) & metadata.mask != 0 {
                            mask_buf += format!("{}|", m.as_ref()).as_ref();
                        }
                    }
                    if mask_buf.len() != 0 {
                        mask_buf.remove(mask_buf.len() - 1);
                    }

                    let file = if metadata.fd >= 0 {
                        let procfd_path = format!("/proc/self/fd/{}", metadata.fd);
                        let path = fs::read_link(procfd_path)?;

                        unsafe {
                            // let this drop and close
                            File::from_raw_fd(metadata.fd);
                        };

                        path
                    } else {
                        PathBuf::from("-")
                    };

                    if file.as_os_str() != "-" && opt.recursive {
                        if opt.namespace.is_none() {
                            for p in &opt.paths {
                                if !file.starts_with(OsStr::from_bytes(&p.as_bytes())) {
                                    debug!("dropping unwanted notification: {:?}", file);
                                    continue 'next_metadata;
                                }
                            }
                        }
                    }

                    print!("{}\t{}\t", mask_buf, metadata.fd);
                    io::stdout().write(&file.as_os_str().as_bytes())?;
                    println!();
                    io::stdout().flush()?;
                }
            }
        }
    };

    return Ok(());
}

impl Opt {
    fn from_args_with_default() -> io::Result<Opt> {
        let mut opt = Opt::from_args();

        opt.events.get_or_insert(DEFAULT_EVENTS.into());
        if opt.filesystem {
            opt.recursive = true;
        } else if opt.mount {
            opt.recursive = true;
        } else if opt.recursive {
            opt.mount = true;
        }

        if opt.namespace.is_none() {
            opt.paths = opt
                .paths
                .into_iter()
                .map(|p| {
                    // convert relative paths to absolute paths
                    fs::canonicalize(OsStr::from_bytes(&p.as_bytes()))
                        .map_err(|e| {
                            io::Error::new(ErrorKind::InvalidInput, format!("{:?}: {}", p, e))
                        })
                        // should be safe to unwrap here since the path should not contain
                        // internal nul bytes
                        .map(|p| CString::new(p.as_os_str().as_bytes().to_vec()).unwrap())
                })
                .collect::<Vec<io::Result<_>>>()
                .into_iter()
                .collect::<io::Result<Vec<_>>>()?;
        }

        Ok(opt)
    }
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
        mask |= m
            .parse::<FanEvents>()
            .map_err(|e| io::Error::new(ErrorKind::InvalidInput, e))? as u64;

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

    loop {
        let ready = poll(events.as_mut_ptr(), events.len() as libc::nfds_t, -1)?;
        if ready > 0 {
            for e in &events {
                if e.revents > 0 {
                    match e.fd {
                        libc::STDIN_FILENO => (),
                        _ => handle_fanotify(e.fd, &mut fabuf, &opt)?,
                    }
                }
            }
        }
    }
}
