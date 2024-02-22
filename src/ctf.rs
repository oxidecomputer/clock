/*
 * Copyright 2024 Oxide Computer Company
 */

use anyhow::{bail, Result};
use libc::{c_char, c_int, c_ulong, c_void, size_t};

use std::{
    ffi::CStr,
    os::fd::{AsRawFd, IntoRawFd},
    path::Path,
};

#[allow(unused)]
mod consts {
    use libc::c_int;

    /*
     * Values for CTF_TYPE_KIND().  If the kind has an associated data list,
     * CTF_INFO_VLEN() will extract the number of elements in the list, and
     * the type of each element is shown in the comments below.
     */
    pub const CTF_K_UNKNOWN: c_int = 0; /* unknown type (used for padding) */
    pub const CTF_K_INTEGER: c_int = 1; /* variant data is CTF_INT_DATA() (see below) */
    pub const CTF_K_FLOAT: c_int = 2; /* variant data is CTF_FP_DATA() (see below) */
    pub const CTF_K_POINTER: c_int = 3; /* ctt_type is referenced type */
    pub const CTF_K_ARRAY: c_int = 4; /* variant data is single ctf_array_t */
    pub const CTF_K_FUNCTION: c_int = 5; /* ctt_type is return type, variant data is */
    /* list of argument types (ushort_t's) */
    pub const CTF_K_STRUCT: c_int = 6; /* variant data is list of ctf_member_t's */
    pub const CTF_K_UNION: c_int = 7; /* variant data is list of ctf_member_t's */
    pub const CTF_K_ENUM: c_int = 8; /* variant data is list of ctf_enum_t's */
    pub const CTF_K_FORWARD: c_int = 9; /* no additional data; ctt_name is tag */
    pub const CTF_K_TYPEDEF: c_int = 10; /* ctt_type is referenced type */
    pub const CTF_K_VOLATILE: c_int = 11; /* ctt_type is base type */
    pub const CTF_K_CONST: c_int = 12; /* ctt_type is base type */
    pub const CTF_K_RESTRICT: c_int = 13; /* ctt_type is base type */

    pub const CTF_K_MAX: c_int = 31; /* Maximum possible CTF_K_* value */
}
use consts::*;

#[allow(unused)]
#[allow(non_camel_case_types)]
mod types {
    use libc::{c_char, c_int, c_long, c_ulong, c_void};

    pub enum ctf_file_t {}
    pub type ctf_id_t = c_long;

    pub type ctf_member_f = extern "C" fn(
        name: *const c_char,
        member_type: ctf_id_t,
        offset: c_ulong,
        arg: *mut c_void,
    ) -> c_int;
}
pub use types::ctf_id_t;
use types::*;

#[allow(unused)]
#[link(name = "ctf")]
extern "C" {
    fn ctf_fdopen(fd: c_int, errp: *mut c_int) -> *mut ctf_file_t;
    fn ctf_close(fp: *mut ctf_file_t);
    fn ctf_type_kind(fp: *mut ctf_file_t, typ: ctf_id_t) -> c_int;
    fn ctf_type_resolve(fp: *mut ctf_file_t, typ: ctf_id_t) -> ctf_id_t;
    fn ctf_max_id(fp: *mut ctf_file_t) -> ctf_id_t;
    fn ctf_type_name(
        fp: *mut ctf_file_t,
        type_: ctf_id_t,
        buf: *mut c_char,
        len: size_t,
    ) -> *mut c_char;
    fn ctf_member_iter(
        fp: *mut ctf_file_t,
        typ: ctf_id_t,
        func: ctf_member_f,
        arg: *mut c_void,
    ) -> c_int;
}

pub struct Ctf {
    fd: c_int,
    ctf: *mut ctf_file_t,
}

#[allow(unused)]
#[derive(Debug)]
pub struct Member {
    name: String,
    type_: ctf_id_t,
    offset: u64,
}

impl Ctf {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Ctf> {
        let path = path.as_ref();
        let f = std::fs::OpenOptions::new().read(true).open(path)?;

        let mut err = 0;
        let ctf = unsafe { ctf_fdopen(f.as_raw_fd(), &mut err) };
        if ctf == std::ptr::null_mut() {
            bail!("could not read CTF from {path:?}: error {err}");
        }

        Ok(Ctf { fd: f.into_raw_fd(), ctf })
    }

    pub fn members(&self, id: ctf_id_t) -> Result<Vec<Member>> {
        #[derive(Default)]
        struct Arg {
            members: Box<Vec<Member>>,
        }
        let mut arg: Arg = Default::default();

        extern "C" fn member_cb(
            name: *const c_char,
            member_type: ctf_id_t,
            offset: c_ulong,
            arg: *mut c_void,
        ) -> c_int {
            let arg: *mut Arg = arg as *mut Arg;

            let name =
                unsafe { CStr::from_ptr(name) }.to_str().unwrap().to_string();

            unsafe { &mut (*arg).members }.push(Member {
                name,
                type_: member_type,
                offset: offset.try_into().unwrap(),
            });

            0
        }

        let ret = unsafe {
            ctf_member_iter(
                self.ctf,
                id,
                member_cb,
                (&mut arg) as *mut Arg as *mut c_void,
            )
        };
        if ret != 0 {
            bail!("members walk failed {ret}");
        }

        Ok(*arg.members)
    }

    pub fn type_name(&self, id: ctf_id_t) -> Result<String> {
        let mut buf = vec![0i8; 2048];

        let s =
            unsafe { ctf_type_name(self.ctf, id, buf.as_mut_ptr(), buf.len()) };
        if s.is_null() {
            bail!("could not look up name of ID {id}");
        }

        Ok(unsafe { CStr::from_ptr(buf.as_ptr()) }
            .to_str()
            .unwrap()
            .to_string())
    }

    pub fn offset_of(
        &self,
        struct_id: ctf_id_t,
        member_name: &str,
    ) -> Result<usize> {
        let members = self.members(struct_id)?;

        let val = members
            .iter()
            .filter(|m| m.name == member_name)
            .map(|m| m.offset)
            .next();

        if let Some(val) = val {
            if val % 8 == 0 {
                Ok((val / 8).try_into().unwrap())
            } else {
                bail!(
                    "offset of {member_name:?} is {} bits, not byte aligned",
                    val
                );
            }
        } else {
            bail!("member {member_name:?} not found");
        }
    }

    pub fn lookup_struct(&self, name: &str) -> Result<ctf_id_t> {
        let max = unsafe { ctf_max_id(self.ctf) };

        let mut id = (0..max)
            .filter(|id| unsafe {
                ctf_type_kind(self.ctf, *id) == CTF_K_STRUCT
            })
            .map(|id| Ok((id, self.type_name(id)?)))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter(|(_, nam)| nam == name);

        let Some((id, _name)) = id.next() else {
            bail!("could not locate struct {name:?}");
        };

        Ok(id)
    }

    #[allow(unused)]
    pub fn dump(&self) -> Result<()> {
        let max = unsafe { ctf_max_id(self.ctf) };
        println!("max ID = {max}");

        let mut id = (0..max)
            .filter(|id| unsafe {
                ctf_type_kind(self.ctf, *id) == CTF_K_STRUCT
            })
            .map(|id| Ok((id, self.type_name(id)?)))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter(|(_, nam)| nam == "struct fb_info");

        let Some((id, _)) = id.next() else {
            bail!("could not find fb_info");
        };

        println!("fb_info -> {id}");

        println!("    fb @ {}", self.offset_of(id, "fb")?);
        println!("    fb_size @ {}", self.offset_of(id, "fb_size")?);

        Ok(())
    }
}

impl Drop for Ctf {
    fn drop(&mut self) {
        unsafe {
            ctf_close(self.ctf);
            libc::close(self.fd);
        }
    }
}
