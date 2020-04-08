use std::ffi::{CString, OsStr, OsString};
use std::fmt::Debug;
use std::fs;
use std::io::{self, ErrorKind};
use std::os::unix::ffi::{OsStrExt, OsStringExt};

use structopt::StructOpt;

fn cstring_from_os_str(src: &OsStr) -> Result<CString, OsString> {
    return CString::new(src.to_os_string().into_vec())
        .map_err(|e| OsString::from(format!("unexpected \\0 at pos {}", e.nul_position())));
}

#[derive(Debug, StructOpt)]
#[structopt(about)]
pub struct Opt {
    /// default: FAN_ACCESS,FAN_MODIFY,FAN_CLOSE_WRITE,FAN_CLOSE_NOWRITE,FAN_OPEN,FAN_ONDIR,FAN_EVENT_ON_CHILD
    #[structopt(short, long)]
    pub events: Option<String>,

    /// paths are relative to the filesystem namespace of this process
    #[structopt(short = "p", long = "process")]
    pub namespace: Option<u32>,

    /// recursively monitor everything under paths, implies -m unless -f is used
    #[structopt(short, long)]
    pub recursive: bool,

    /// notify for the mount point, implies -r
    #[structopt(short, long)]
    pub mount: bool,

    /// notify for the filesystem, implies -r
    #[structopt(short, long)]
    pub filesystem: bool,

    #[structopt(parse(try_from_os_str = cstring_from_os_str))]
    pub paths: Vec<CString>,
}

const DEFAULT_EVENTS: &str =
    "FAN_ACCESS,FAN_MODIFY,FAN_CLOSE_WRITE,FAN_CLOSE_NOWRITE,FAN_OPEN,FAN_ONDIR,FAN_EVENT_ON_CHILD";

impl Opt {
    pub fn from_args_with_default() -> io::Result<Opt> {
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
