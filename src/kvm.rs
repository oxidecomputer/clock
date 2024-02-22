/*
 * Copyright 2024 Oxide Computer Company
 */

use std::{ffi::CString, mem::size_of};

use anyhow::{bail, Result};
use libc::{
    c_char, c_int, c_long, c_short, c_ushort, c_void, size_t, ssize_t,
    uintptr_t,
};

enum KvmT {}

#[repr(C)]
struct nlist {
    n_name: *mut c_char,
    n_value: c_long,
    n_scnum: c_short,
    n_type: c_ushort,
    n_sclass: c_char,
    n_numaux: c_char,
}

#[link(name = "kvm")]
extern "C" {
    fn kvm_open(
        namelist: *const c_char,
        corefile: *const c_char,
        swapfile: *const c_char,
        flag: c_int,
        errstr: *mut c_char,
    ) -> *mut KvmT;
    fn kvm_close(kd: *mut KvmT) -> c_int;
    fn kvm_nlist(kd: *mut KvmT, nlist: *mut nlist) -> c_int;
    fn kvm_kread(
        kd: *mut KvmT,
        addr: uintptr_t,
        buf: *mut c_void,
        nbytes: size_t,
    ) -> ssize_t;
}

pub struct Kvm {
    kvm: *mut KvmT,
}

impl Drop for Kvm {
    fn drop(&mut self) {
        unsafe { kvm_close(self.kvm) };
    }
}

impl Kvm {
    pub fn new() -> Result<Kvm> {
        let kvm = unsafe {
            kvm_open(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                libc::O_RDONLY,
                std::ptr::null_mut(),
            )
        };

        if kvm.is_null() {
            bail!("could not access the kernel");
        }

        Ok(Kvm { kvm })
    }

    pub fn locate(&self, name: &str) -> Result<usize> {
        let name = CString::new(name)?;
        let mut nlist: [nlist; 2] = unsafe { std::mem::zeroed() };
        nlist[0].n_name = name.as_ptr() as *mut c_char;

        let r = unsafe { kvm_nlist(self.kvm, nlist.as_mut_ptr()) };
        if r != 0 {
            bail!("nlist failed");
        }

        if nlist[0].n_type == 0 {
            bail!("could not locate {name:?}");
        }

        Ok(nlist[0].n_value as usize)
    }

    pub fn read_buf(&self, addr: usize, buf: &mut [u8]) -> Result<()> {
        let r = unsafe {
            kvm_kread(
                self.kvm,
                addr,
                buf.as_mut_ptr() as *mut c_void,
                buf.len(),
            )
        };
        if r == -1 {
            let e = std::io::Error::last_os_error();
            bail!("could not read 0x{addr:x}: {e}");
        } else if r != buf.len() as isize {
            bail!("read {r} bytes, but wanted {}", buf.len());
        } else {
            Ok(())
        }
    }

    #[allow(unused)]
    pub fn read_usize(&self, addr: usize) -> Result<usize> {
        let mut buf = [0u8; size_of::<usize>()];

        self.read_buf(addr, &mut buf)?;

        Ok(usize::from_ne_bytes(buf))
    }

    pub fn read_u16(&self, addr: usize) -> Result<u16> {
        let mut buf = [0u8; size_of::<u16>()];

        self.read_buf(addr, &mut buf)?;

        Ok(u16::from_ne_bytes(buf))
    }
}
