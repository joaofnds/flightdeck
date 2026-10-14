use std::ffi::{c_char, c_int, c_uint};
use std::io;
use std::mem::offset_of;

use crate::kernel::sysctl;
use crate::push_without_growing;

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

/// An interface's name, UTF-8 of up to `IFNAMSIZ` bytes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Name {
    bytes: [u8; IFNAMSIZ],
    length: usize,
}

impl Name {
    /// Takes `name`, or an error where it is past `IFNAMSIZ` bytes or not UTF-8.
    fn new(name: &[u8]) -> io::Result<Self> {
        if name.len() > IFNAMSIZ {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("an interface name of {} bytes, past {IFNAMSIZ}", name.len()),
            ));
        }
        if let Err(error) = std::str::from_utf8(name) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, error));
        }

        let mut bytes = [0; IFNAMSIZ];
        bytes[..name.len()].copy_from_slice(name);
        Ok(Self {
            bytes,
            length: name.len(),
        })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        assert!(self.length <= IFNAMSIZ);

        match std::str::from_utf8(&self.bytes[..self.length]) {
            Ok(name) => name,
            Err(error) => unreachable!("{error}"),
        }
    }
}

impl std::fmt::Debug for Name {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

pub struct Interfaces {
    buffer: Vec<u8>,
    pub read: Vec<Interface>,
}

impl Default for Interfaces {
    fn default() -> Self {
        Self::with_dump_size(DUMP_SIZE)
    }
}

/// The most bytes of the routing dump of every interface the collector reads, sixteen times
/// the 15,920 bytes the dump of 30 interfaces took on the Mac this was measured on.
const DUMP_SIZE: usize = 256 * 1024;

impl Interfaces {
    fn with_dump_size(dump_size: usize) -> Self {
        assert!(dump_size > 0);

        Self {
            buffer: vec![0; dump_size],
            // Each interface read takes a whole header of the dump, so the dump holds no
            // more of them than this, which floors.
            read: Vec::with_capacity(dump_size / size_of::<IfMsghdr2>()),
        }
    }

    pub fn read_all(&mut self) -> io::Result<()> {
        let filled = self.fill_buffer()?;
        let Some(dump) = self.buffer.get(..filled) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("sysctl wrote {filled} bytes into {}", self.buffer.len()),
            ));
        };
        self.read.clear();

        read_all_messages(dump, &mut self.read)
    }

    /// Fills the buffer with the routing dump of every interface and returns the bytes the
    /// kernel wrote. A dump past the buffer is an error, since the buffer does not grow.
    fn fill_buffer(&mut self) -> io::Result<usize> {
        let mut path = [
            CTL_NET,
            PF_ROUTE,
            ROUTE_PROTOCOL,
            AF_UNSPEC,
            NET_RT_IFLIST2,
            EVERY_INTERFACE,
        ];
        let path_count = match c_uint::try_from(path.len()) {
            Ok(count) => count,
            Err(error) => unreachable!("{error}"),
        };
        let mut size = self.buffer.len();
        let result = unsafe {
            sysctl(
                path.as_mut_ptr(),
                path_count,
                self.buffer.as_mut_ptr().cast(),
                &raw mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if result == 0 {
            return Ok(size);
        }

        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(ENOMEM) {
            return Err(io::Error::new(
                io::ErrorKind::QuotaExceeded,
                format!("the interface dump outgrew its {} bytes", self.buffer.len()),
            ));
        }
        Err(error)
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
/// The protocol a `PF_ROUTE` path names third, which sysctl(3) gives as always 0.
const ROUTE_PROTOCOL: c_int = 0;
/// The address family a `PF_ROUTE` path names fourth, where 0 selects every family.
const AF_UNSPEC: c_int = 0;
const NET_RT_IFLIST2: c_int = 6;
/// The interface index a `NET_RT_IFLIST2` path names sixth, where 0 selects every interface.
const EVERY_INTERFACE: c_int = 0;
const RTM_IFINFO2: c_int = 18;
const IFF_LOOPBACK: c_int = 0x8;
const IFNAMSIZ: usize = 16;
const ENOMEM: c_int = 12;

/// Reads into `read` the interface each message of `dump` names, skipping loopback and the
/// messages of other kinds. A message the dump cannot hold, or that names no interface, is
/// an error.
fn read_all_messages(dump: &[u8], read: &mut Vec<Interface>) -> io::Result<()> {
    let mut offset = 0;
    while offset < dump.len() {
        let Some(header) = dump.get(offset..offset + MESSAGE_PREFIX_SIZE) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "the interface dump ends {} bytes into a message",
                    dump.len() - offset
                ),
            ));
        };
        let length = usize::from(u16::from_ne_bytes([
            header[MESSAGE_LENGTH_OFFSET],
            header[MESSAGE_LENGTH_OFFSET + 1],
        ]));
        if length < MESSAGE_PREFIX_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("a routing message gave its length as {length} bytes"),
            ));
        }
        let Some(message) = dump.get(offset..offset + length) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "a routing message of {length} bytes runs past the {} left of the dump",
                    dump.len() - offset
                ),
            ));
        };

        if c_int::from(header[MESSAGE_TYPE_OFFSET]) == RTM_IFINFO2 {
            if let Some(interface) = interface_from(message)? {
                push_without_growing(read, interface);
            }
        }
        offset += length;
        assert!(offset <= dump.len());
    }

    assert_eq!(dump.len(), offset);
    Ok(())
}

/// Reads the interface `message` names, or `None` for loopback, whose traffic is not
/// external.
fn interface_from(message: &[u8]) -> io::Result<Option<Interface>> {
    let Some(link) = message.get(size_of::<IfMsghdr2>()..) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "an interface's message of {} bytes is shorter than its header",
                message.len()
            ),
        ));
    };
    let info = unsafe { message.as_ptr().cast::<IfMsghdr2>().read_unaligned() };
    if info.flags & IFF_LOOPBACK != 0 {
        return Ok(None);
    }

    let Some(&name_length) = link.get(offset_of!(SockaddrDl, nlen)) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("interface {} has no link address", info.index),
        ));
    };
    let name_offset = offset_of!(SockaddrDl, data);
    let Some(name) = link.get(name_offset..name_offset + usize::from(name_length)) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "interface {}'s name of {name_length} bytes runs past its message",
                info.index
            ),
        ));
    };

    let data = info.data;
    Ok(Some(Interface {
        index: info.index,
        name: Name::new(name)?,
        received_bytes: data.ibytes,
        sent_bytes: data.obytes,
        received_packets: data.ipackets,
        sent_packets: data.opackets,
        receive_errors: data.ierrors,
        send_errors: data.oerrors,
        receive_drops: data.iqdrops,
    }))
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
        assert!(!first.is_null());

        let mut entry = first;
        while unsafe { (*entry).if_index } != 0 {
            let name = unsafe { std::ffi::CStr::from_ptr((*entry).if_name) };
            names.push(name.to_string_lossy().into_owned());
            entry = unsafe { entry.add(1) };
        }
        unsafe { if_freenameindex(first) };
        names
    }

    /// A message of `kind` in a routing dump, with `flags`, whose link address names `name`.
    fn message(kind: c_int, flags: c_int, name: &[u8]) -> Vec<u8> {
        let link = size_of::<IfMsghdr2>();
        let link_size = size_of::<SockaddrDl>().max(offset_of!(SockaddrDl, data) + name.len());
        let mut message = vec![0; link + link_size];
        let length = u16::try_from(message.len()).unwrap().to_ne_bytes();
        message[MESSAGE_LENGTH_OFFSET..][..length.len()].copy_from_slice(&length);
        message[MESSAGE_TYPE_OFFSET] = u8::try_from(kind).unwrap();
        let flags = flags.to_ne_bytes();
        message[offset_of!(IfMsghdr2, flags)..][..flags.len()].copy_from_slice(&flags);
        message[link + offset_of!(SockaddrDl, nlen)] = u8::try_from(name.len()).unwrap();
        message[link + offset_of!(SockaddrDl, data)..][..name.len()].copy_from_slice(name);
        message
    }

    /// Sets the length `message` gives itself to `length`.
    fn set_length(message: &mut [u8], length: u16) {
        let length = length.to_ne_bytes();
        message[MESSAGE_LENGTH_OFFSET..][..length.len()].copy_from_slice(&length);
    }

    /// Reads `dump` into room for every interface it could hold.
    fn read_dump(dump: &[u8]) -> io::Result<Vec<Interface>> {
        let mut read = Vec::with_capacity(dump.len() / size_of::<IfMsghdr2>());
        read_all_messages(dump, &mut read)?;
        Ok(read)
    }

    /// The names of `read`, in order.
    fn names(read: &[Interface]) -> Vec<&str> {
        read.iter()
            .map(|interface| interface.name.as_str())
            .collect()
    }

    /// The type of a message that adds an address to an interface, as net/route.h defines
    /// it.
    const RTM_NEWADDR: c_int = 0xc;

    #[test]
    fn reads_the_interface_a_message_names() {
        // Goal: an interface's message reads as the interface its link address names.
        // Method: read a dump of one message naming en0.
        let dump = message(RTM_IFINFO2, 0, b"en0");

        let read = read_dump(&dump).unwrap();

        assert_eq!(vec!["en0"], names(&read));
    }

    #[test]
    fn reads_every_interface_message_of_a_dump() {
        // Goal: a dump reads as every interface its messages name, in order.
        // Method: read a dump of en0's message, an address message, then en1's.
        let mut dump = message(RTM_IFINFO2, 0, b"en0");
        dump.extend(message(RTM_NEWADDR, 0, b""));
        dump.extend(message(RTM_IFINFO2, 0, b"en1"));

        let read = read_dump(&dump).unwrap();

        assert_eq!(vec!["en0", "en1"], names(&read));
    }

    #[test]
    fn skips_loopback() {
        // Goal: a loopback interface is not read, since its traffic is not external.
        // Method: read a dump of one message flagged as loopback.
        let dump = message(RTM_IFINFO2, IFF_LOOPBACK, b"lo0");

        let read = read_dump(&dump).unwrap();

        assert!(read.is_empty());
    }

    #[test]
    fn skips_a_message_of_another_kind() {
        // Goal: a message that does not describe an interface is not read as one.
        // Method: read a dump of one address message.
        let dump = message(RTM_NEWADDR, 0, b"en0");

        let read = read_dump(&dump).unwrap();

        assert!(read.is_empty());
    }

    #[test]
    fn refuses_a_message_shorter_than_its_length_and_type() {
        // Goal: a message whose length does not cover its own length and type is an error,
        // since the dump cannot be read past it.
        // Method: read a dump whose one message gives its length as 0.
        let mut dump = message(RTM_IFINFO2, 0, b"en0");
        set_length(&mut dump, 0);

        let read = read_dump(&dump);

        assert_eq!(io::ErrorKind::InvalidData, read.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_message_that_runs_past_the_dump() {
        // Goal: a message longer than what is left of the dump is an error, never a dump read
        // as ending there.
        // Method: read a dump whose one message gives its length as four bytes more than it
        // has.
        let mut dump = message(RTM_IFINFO2, 0, b"en0");
        let length = u16::try_from(dump.len() + 4).unwrap();
        set_length(&mut dump, length);

        let read = read_dump(&dump);

        assert_eq!(io::ErrorKind::InvalidData, read.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_dump_that_ends_inside_a_message_length_and_type() {
        // Goal: bytes after the last message too few to give a length and a type are an
        // error, never ignored.
        // Method: read a dump of one message and two more bytes.
        let mut dump = message(RTM_IFINFO2, 0, b"en0");
        dump.extend([0, 0]);

        let read = read_dump(&dump);

        assert_eq!(io::ErrorKind::InvalidData, read.unwrap_err().kind());
    }

    #[test]
    fn refuses_an_interface_message_shorter_than_its_header() {
        // Goal: an interface's message too short for its header is an error, never a read past
        // the message.
        // Method: read a dump whose interface message ends 60 bytes into its header.
        let mut dump = message(RTM_IFINFO2, 0, b"en0");
        dump.truncate(60);
        set_length(&mut dump, 60);

        let read = read_dump(&dump);

        assert_eq!(io::ErrorKind::InvalidData, read.unwrap_err().kind());
    }

    #[test]
    fn refuses_an_interface_message_with_no_link_address() {
        // Goal: an interface's message that ends with its header is an error, since it names
        // no interface.
        // Method: read a dump whose interface message is its header alone.
        let mut dump = message(RTM_IFINFO2, 0, b"en0");
        dump.truncate(size_of::<IfMsghdr2>());
        set_length(&mut dump, u16::try_from(size_of::<IfMsghdr2>()).unwrap());

        let read = read_dump(&dump);

        assert_eq!(io::ErrorKind::InvalidData, read.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_name_that_runs_past_its_message() {
        // Goal: a name longer than what is left of its message is an error, never a name read
        // from the next message.
        // Method: read a message naming en0 whose link address gives the name 13 bytes, one
        // more than the message holds.
        let mut dump = message(RTM_IFINFO2, 0, b"en0");
        dump[size_of::<IfMsghdr2>() + offset_of!(SockaddrDl, nlen)] = 13;

        let read = read_dump(&dump);

        assert_eq!(io::ErrorKind::InvalidData, read.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_name_longer_than_an_interface_name_holds() {
        // Goal: a name past the 16 bytes IFNAMSIZ gives an interface name is an error.
        // Method: read a message whose name is 17 bytes.
        let dump = message(RTM_IFINFO2, 0, b"abcdefghijklmnopq");

        let read = read_dump(&dump);

        assert_eq!(io::ErrorKind::InvalidData, read.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_name_that_is_not_utf8() {
        // Goal: a name that is not UTF-8 is an error, never a name printed as something else.
        // Method: read a message whose name is the byte 0xff, which no UTF-8 text holds.
        let dump = message(RTM_IFINFO2, 0, &[0xff]);

        let read = read_dump(&dump);

        assert_eq!(io::ErrorKind::InvalidData, read.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_dump_larger_than_its_buffer_without_growing_it() {
        // Goal: an interface dump larger than the buffer is an error, and the buffer keeps the
        // size it was given.
        // Method: read into a buffer that holds one message's prefix, smaller than any dump.
        let mut interfaces = Interfaces::with_dump_size(MESSAGE_PREFIX_SIZE);
        let capacity = interfaces.buffer.capacity();

        let read = interfaces.read_all();

        assert_eq!(io::ErrorKind::QuotaExceeded, read.unwrap_err().kind());
        assert_eq!(capacity, interfaces.buffer.capacity());
    }

    #[test]
    fn names_every_interface_the_system_lists_but_loopback() {
        // Goal: a read names every interface this Mac has, and no other, but loopback.
        // Method: compare the names read with the names if_nameindex lists, less lo0.
        let mut interfaces = Interfaces::default();

        interfaces.read_all().unwrap();

        let mut read: Vec<String> = interfaces
            .read
            .iter()
            .map(|interface| interface.name.as_str().to_owned())
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
