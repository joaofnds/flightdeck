use std::io;

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
    bytes: [u8; libc::IFNAMSIZ],
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
        while let Some(header) = self.buffer.get(offset..offset + 4) {
            let length = usize::from(u16::from_ne_bytes([header[0], header[1]]));
            let kind = libc::c_int::from(header[3]);
            if length == 0 {
                break;
            }
            let Some(message) = self.buffer.get(offset..offset + length) else {
                break;
            };

            if kind == libc::RTM_IFINFO2 {
                self.read.extend(interface_from(message));
            }
            offset += length;
        }
        Ok(())
    }

    fn fill_buffer(&mut self) -> io::Result<()> {
        let mut mib = [libc::CTL_NET, libc::PF_ROUTE, 0, 0, libc::NET_RT_IFLIST2, 0];
        loop {
            let mut size = 0;
            if unsafe {
                libc::sysctl(
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
                libc::sysctl(
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
            if error.raw_os_error() != Some(libc::ENOMEM) {
                return Err(error);
            }
        }
    }
}

fn interface_from(message: &[u8]) -> Option<Interface> {
    let link = message.get(size_of::<libc::if_msghdr2>()..)?;
    let info = unsafe { message.as_ptr().cast::<libc::if_msghdr2>().read_unaligned() };
    if info.ifm_flags & libc::IFF_LOOPBACK != 0 {
        return None;
    }

    let name_offset = std::mem::offset_of!(libc::sockaddr_dl, sdl_data);
    let name_len = usize::from(*link.get(std::mem::offset_of!(libc::sockaddr_dl, sdl_nlen))?);
    let name = link
        .get(name_offset..name_offset + name_len)
        .filter(|name| name.len() <= libc::IFNAMSIZ)?;
    let mut bytes = [0; libc::IFNAMSIZ];
    bytes[..name_len].copy_from_slice(name);

    let data = info.ifm_data;
    Some(Interface {
        index: info.ifm_index,
        name: Name {
            bytes,
            len: name_len,
        },
        received_bytes: data.ifi_ibytes,
        sent_bytes: data.ifi_obytes,
        received_packets: data.ifi_ipackets,
        sent_packets: data.ifi_opackets,
        receive_errors: data.ifi_ierrors,
        send_errors: data.ifi_oerrors,
        receive_drops: data.ifi_iqdrops,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed_by_the_system() -> Vec<String> {
        let mut names = Vec::new();
        let first = unsafe { libc::if_nameindex() };
        let mut entry = first;
        while unsafe { (*entry).if_index } != 0 {
            let name = unsafe { std::ffi::CStr::from_ptr((*entry).if_name) };
            names.push(name.to_string_lossy().into_owned());
            entry = unsafe { entry.add(1) };
        }
        unsafe { libc::if_freenameindex(first) };
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
