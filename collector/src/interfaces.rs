use std::ffi::{c_char, c_int};
use std::io;
use std::mem::offset_of;

use crate::kernel::sysctl;

#[derive(Debug, Clone, Copy)]
pub struct Interface {
    pub index: u16,
    pub name: Name,
    pub received_bytes: u64,
    pub sent_bytes: u64,
    pub received_packets: u64,
    pub sent_packets: u64,
    pub receive_errors: u64,
    pub send_errors: u64,
    pub receive_drops: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Name {
    bytes: [u8; IFNAMSIZ],
    len: usize,
}

impl Name {
    #[must_use]
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len]).unwrap_or("?")
    }
}

impl std::fmt::Debug for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Default)]
pub struct Interfaces {
    buffer: Vec<u8>,
    pub read: Vec<Interface>,
}

impl Interfaces {
    pub fn read_all(&mut self) -> io::Result<()> {
        self.fill_buffer()?;
        self.read.clear();

        let mut offset = 0;
        while let Some(header) = self.buffer.get(offset..offset + MESSAGE_PREFIX_SIZE) {
            let length = usize::from(u16::from_ne_bytes([
                header[MESSAGE_LENGTH_OFFSET],
                header[MESSAGE_LENGTH_OFFSET + 1],
            ]));
            let kind = c_int::from(header[MESSAGE_TYPE_OFFSET]);
            if length == 0 {
                break;
            }
            let Some(message) = self.buffer.get(offset..offset + length) else {
                break;
            };

            if kind == RTM_IFINFO2 {
                self.read.extend(interface_from(message));
            }
            offset += length;
        }

        Ok(())
    }

    fn fill_buffer(&mut self) -> io::Result<()> {
        let mut mib = [CTL_NET, PF_ROUTE, 0, 0, NET_RT_IFLIST2, 0];
        loop {
            let mut size = 0;
            if unsafe {
                sysctl(
                    mib.as_mut_ptr(),
                    6,
                    std::ptr::null_mut(),
                    &raw mut size,
                    std::ptr::null_mut(),
                    0,
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }

            self.buffer.resize(size, 0);
            if unsafe {
                sysctl(
                    mib.as_mut_ptr(),
                    6,
                    self.buffer.as_mut_ptr().cast(),
                    &raw mut size,
                    std::ptr::null_mut(),
                    0,
                )
            } == 0
            {
                self.buffer.truncate(size);
                return Ok(());
            }

            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ENOMEM) {
                return Err(error);
            }
        }
    }
}

/// The header of an interface's message in a routing dump, as `struct if_msghdr2` declares
/// it without the `ifm_` prefix.
#[repr(C)]
struct IfMsghdr2 {
    msglen: u16,
    version: u8,
    r#type: u8,
    addrs: c_int,
    flags: c_int,
    index: u16,
    snd_len: c_int,
    snd_maxlen: c_int,
    snd_drops: c_int,
    timer: c_int,
    data: IfData64,
}

const _: () = assert!(size_of::<IfMsghdr2>() == 160);
const _: () = assert!(align_of::<IfMsghdr2>() == 4);
const _: () = assert!(offset_of!(IfMsghdr2, msglen) == 0);
const _: () = assert!(offset_of!(IfMsghdr2, r#type) == 3);
const _: () = assert!(offset_of!(IfMsghdr2, addrs) == 4);
const _: () = assert!(offset_of!(IfMsghdr2, flags) == 8);
const _: () = assert!(offset_of!(IfMsghdr2, index) == 12);
const _: () = assert!(offset_of!(IfMsghdr2, data) == 32);

/// Where every routing message keeps its length, two bytes, and its type, one byte, in the
/// fields it starts with, which end where `addrs` starts.
const MESSAGE_LENGTH_OFFSET: usize = offset_of!(IfMsghdr2, msglen);
const MESSAGE_TYPE_OFFSET: usize = offset_of!(IfMsghdr2, r#type);
const MESSAGE_PREFIX_SIZE: usize = offset_of!(IfMsghdr2, addrs);

const _: () = assert!(MESSAGE_LENGTH_OFFSET + size_of::<u16>() <= MESSAGE_PREFIX_SIZE);
const _: () = assert!(MESSAGE_TYPE_OFFSET < MESSAGE_PREFIX_SIZE);

/// An interface's counters, as `struct if_data64` declares it without the `ifi_` prefix,
/// packed to four bytes as the header packs it.
#[repr(C, packed(4))]
#[derive(Clone, Copy)]
struct IfData64 {
    r#type: u8,
    typelen: u8,
    physical: u8,
    addrlen: u8,
    hdrlen: u8,
    recvquota: u8,
    xmitquota: u8,
    unused1: u8,
    mtu: u32,
    metric: u32,
    baudrate: u64,
    ipackets: u64,
    ierrors: u64,
    opackets: u64,
    oerrors: u64,
    collisions: u64,
    ibytes: u64,
    obytes: u64,
    imcasts: u64,
    omcasts: u64,
    iqdrops: u64,
    noproto: u64,
    recvtiming: u32,
    xmittiming: u32,
    lastchange: [i32; 2], // struct timeval32
}

const _: () = assert!(size_of::<IfData64>() == 128);
const _: () = assert!(align_of::<IfData64>() == 4);
const _: () = assert!(offset_of!(IfData64, ipackets) == 24);
const _: () = assert!(offset_of!(IfData64, ierrors) == 32);
const _: () = assert!(offset_of!(IfData64, opackets) == 40);
const _: () = assert!(offset_of!(IfData64, oerrors) == 48);
const _: () = assert!(offset_of!(IfData64, ibytes) == 64);
const _: () = assert!(offset_of!(IfData64, obytes) == 72);
const _: () = assert!(offset_of!(IfData64, iqdrops) == 96);

/// The link address that follows the header, as `struct sockaddr_dl` declares it without
/// the `sdl_` prefix. The name starts `data` and runs on past the struct.
#[repr(C)]
struct SockaddrDl {
    len: u8,
    family: u8,
    index: u16,
    r#type: u8,
    nlen: u8,
    alen: u8,
    slen: u8,
    data: [c_char; 12],
}

const _: () = assert!(size_of::<SockaddrDl>() == 20);
const _: () = assert!(offset_of!(SockaddrDl, nlen) == 5);
const _: () = assert!(offset_of!(SockaddrDl, data) == 8);

const CTL_NET: c_int = 4;
const PF_ROUTE: c_int = 17;
const NET_RT_IFLIST2: c_int = 6;
const RTM_IFINFO2: c_int = 18;
const IFF_LOOPBACK: c_int = 0x8;
const IFNAMSIZ: usize = 16;
const ENOMEM: c_int = 12;

fn interface_from(message: &[u8]) -> Option<Interface> {
    let link = message.get(size_of::<IfMsghdr2>()..)?;
    let info = unsafe { message.as_ptr().cast::<IfMsghdr2>().read_unaligned() };
    if info.flags & IFF_LOOPBACK != 0 {
        return None;
    }

    let name_offset = offset_of!(SockaddrDl, data);
    let name_len = usize::from(*link.get(offset_of!(SockaddrDl, nlen))?);
    let name = link
        .get(name_offset..name_offset + name_len)
        .filter(|name| name.len() <= IFNAMSIZ)?;
    let mut bytes = [0; IFNAMSIZ];
    bytes[..name_len].copy_from_slice(name);

    let data = info.data;
    Some(Interface {
        index: info.index,
        name: Name {
            bytes,
            len: name_len,
        },
        received_bytes: data.ibytes,
        sent_bytes: data.obytes,
        received_packets: data.ipackets,
        sent_packets: data.opackets,
        receive_errors: data.ierrors,
        send_errors: data.oerrors,
        receive_drops: data.iqdrops,
    })
}

#[cfg(test)]
mod tests {
    use std::ffi::c_uint;

    use super::*;

    /// An interface's index and name, as `struct if_nameindex` declares it.
    #[repr(C)]
    struct IfNameindex {
        if_index: c_uint,
        if_name: *mut c_char,
    }

    const _: () = assert!(size_of::<IfNameindex>() == 16);
    const _: () = assert!(offset_of!(IfNameindex, if_index) == 0);
    const _: () = assert!(offset_of!(IfNameindex, if_name) == 8);

    unsafe extern "C" {
        fn if_nameindex() -> *mut IfNameindex;
        fn if_freenameindex(first: *mut IfNameindex);
    }

    fn listed_by_the_system() -> Vec<String> {
        let mut names = Vec::new();
        let first = unsafe { if_nameindex() };
        let mut entry = first;
        while unsafe { (*entry).if_index } != 0 {
            let name = unsafe { std::ffi::CStr::from_ptr((*entry).if_name) };
            names.push(name.to_string_lossy().into_owned());
            entry = unsafe { entry.add(1) };
        }
        unsafe { if_freenameindex(first) };
        names
    }

    #[test]
    fn names_every_interface_the_system_lists_but_loopback() {
        let mut interfaces = Interfaces::default();

        interfaces.read_all().unwrap();

        let mut read: Vec<String> = interfaces
            .read
            .iter()
            .map(|i| i.name.as_str().to_owned())
            .collect();
        let mut expected: Vec<String> = listed_by_the_system()
            .into_iter()
            .filter(|name| name != "lo0")
            .collect();
        read.sort();
        expected.sort();
        assert_eq!(expected, read);
    }
}
